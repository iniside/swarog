// Package messaging is the durable async plane's one and only module. It owns
// schema "messaging" (a shared outbox log + a per-subscriber inbox dedup ledger),
// implements bus.Transport, and installs it via ctx.Bus.SetTransport — so the bus
// leaf gains a durable plane without importing any module (hard constraint #1:
// dependency points module → leaf, never the reverse). It is the ONLY module that
// implements bus.Transport and imports outbox.
//
// A producer reaches it purely via bus.EmitTx (writes one messaging.outbox row in
// the producer's own domain tx); a consumer via bus.OnTx/OnTxRaw (a durable
// handler run inside a per-subscriber inbox-dedup tx). Neither ever sees the
// outbox, the inbox, the relay, EVENTS_SUBSCRIBERS, or MESSAGING_ORIGIN — messaging
// owns the whole envelope. Delivery is topology-transparent: the same code path
// serves both the monolith (in-process local targets) and a split (HTTP POST to a
// peer's /events endpoint), chosen by durability intent, never by topology.
package messaging

import (
	"context"
	"database/sql"
	"io"
	"log/slog"
	"net/http"
	"os"
	"sync"
	"time"

	"github.com/jackc/pgx/v5"

	"gamebackend/bus"
	"gamebackend/lifecycle"
	"gamebackend/outbox"
	"gamebackend/registry"
)

// defaultDSN is the fallback DSN for the LISTEN connection — same default as the
// app's shared pool (internal/app/app.go). Raw pgx is needed because database/sql
// cannot WaitForNotification; the pooled ctx.DB serves every other query.
const defaultDSN = "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable"

// defaultOrigin is the stable identity a monolith stamps on its outbox rows when
// MESSAGING_ORIGIN is unset. It must be stable across restarts so a crashed
// process resumes draining its own unsent rows — never a pid/hostname.
const defaultOrigin = "monolith"

// notifyChannel is the LISTEN/NOTIFY channel the outbox insert trigger fires on.
const notifyChannel = "messaging_outbox"

// housekeepBatch bounds each retention DELETE so a prune never takes a long lock.
const housekeepBatch = 1000

// Service is the registry marker messaging Provides under "messaging". It exists
// only so a process that hosts a durable producer/consumer (which declares
// Requires("messaging")) fails loud at validateRequires when messaging is absent —
// the REAL wiring is via ctx.Bus.SetTransport, not a method on this interface. No
// consumer imports this package to Require it (they use the bus); validateRequires
// matches on the module Name(). The typed value is kept for a possible future
// explicit assert and to make the Provide self-documenting.
type Service interface{ messaging() }

// *Module is the durable transport the bus leaf declares but never implements.
var _ bus.Transport = (*Module)(nil)

// localSub is one in-process durable subscription: a stable subscriber name (the
// inbox dedup key) plus the bytes-level handler the bus closure installed.
type localSub struct {
	subscriber string
	h          func(ctx context.Context, tx *sql.Tx, payload []byte) error
}

// Module owns schema "messaging" and implements bus.Transport. Pointer receiver:
// it holds the shared pool, the relay, the local-subscription table, and the
// background-loop handles.
type Module struct {
	db     *sql.DB
	log    *slog.Logger
	origin string
	dsn    string
	relay  *outbox.Relay

	// localHandlers maps topic -> subscriptions. It MUST be allocated in the
	// struct literal / Register (phase 1), NEVER Init: a consumer registered
	// before messaging calls SubscribeTx during its phase-2 Init, which runs
	// BEFORE messaging.Init (messaging is registered last). A map first allocated
	// in Init would be nil then -> a nil-map append panic at boot. The mutex
	// guards SubscribeTx's append (during Init) against the relay/handleInbound
	// reads (during Start) — different phases, but locked for safety.
	mu            sync.Mutex
	localHandlers map[string][]localSub

	// housekeeping retention window + tick interval, resolved in Init.
	retention time.Duration
	houseTick time.Duration

	cancel context.CancelFunc
	wg     sync.WaitGroup
}

func (*Module) Name() string       { return "messaging" }
func (*Module) Requires() []string { return nil } // foundation-like: depends on nobody
func (*Module) messaging()         {}             // satisfies Service (registry marker)

// schemaDDL creates this module's own schema — full logical isolation (#10).
// Idempotent (IF NOT EXISTS / OR REPLACE). The AFTER INSERT trigger fires the
// pg_notify the relay's LISTEN loop wakes on; the partial index keeps the
// unsent-rows scan cheap; the inbox PK (event_id, subscriber) is what makes
// dedup PER SUBSCRIBER so a failing subscriber never blocks another's delivery.
const schemaDDL = `
CREATE SCHEMA IF NOT EXISTS messaging;
CREATE TABLE IF NOT EXISTS messaging.outbox (
	id         bigserial   PRIMARY KEY,
	origin     text        NOT NULL,
	topic      text        NOT NULL,
	payload    jsonb       NOT NULL,
	created_at timestamptz NOT NULL DEFAULT now(),
	sent_at    timestamptz
);
CREATE INDEX IF NOT EXISTS outbox_unsent_idx ON messaging.outbox (id) WHERE sent_at IS NULL;
CREATE TABLE IF NOT EXISTS messaging.inbox (
	event_id     text        NOT NULL,
	subscriber   text        NOT NULL,
	processed_at timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (event_id, subscriber)
);
CREATE OR REPLACE FUNCTION messaging.notify_outbox() RETURNS trigger
	LANGUAGE plpgsql AS $$
BEGIN
	PERFORM pg_notify('messaging_outbox', NEW.topic);
	RETURN NULL;
END;
$$;
CREATE OR REPLACE TRIGGER outbox_notify
	AFTER INSERT ON messaging.outbox
	FOR EACH ROW EXECUTE FUNCTION messaging.notify_outbox();`

// Migrate creates schema "messaging". Idempotent.
func (*Module) Migrate(_ context.Context, db *sql.DB) error {
	_, err := db.Exec(schemaDDL)
	return err
}

// Register runs in Build's phase 1, BEFORE any Init. It (a) allocates
// localHandlers so a consumer's phase-2 SubscribeTx cannot nil-map-panic, (b)
// installs the transport so every consumer's OnTx sees a live durable plane, and
// (c) Provides the "messaging" registry marker so validateRequires can enforce
// Requires("messaging") on durable producers/consumers. All three must precede
// any Init — hence phase 1.
func (m *Module) Register(ctx *lifecycle.Context) error {
	if m.localHandlers == nil {
		m.localHandlers = map[string][]localSub{}
	}
	ctx.Bus.SetTransport(m)
	registry.Provide[Service](ctx.Registry, "messaging", m)
	return nil
}

// EnqueueTx (bus.Transport) writes one outbox row inside the PRODUCER's domain tx,
// so the event is durable iff the domain change commits. It stamps m.origin (the
// producer never sets it) and does NOT commit — the caller owns the tx.
func (m *Module) EnqueueTx(tx *sql.Tx, topic string, payload []byte) error {
	_, err := tx.Exec(
		`INSERT INTO messaging.outbox (origin, topic, payload) VALUES ($1, $2, $3::jsonb)`,
		m.origin, topic, payload)
	return err
}

// SubscribeTx (bus.Transport) records an in-process durable subscription. Called
// from a consumer's Init (phase 2, before messaging.Init builds the relay), so it
// only appends under the mutex; Init later snapshots these into relay localTargets.
func (m *Module) SubscribeTx(topic, subscriber string, h func(ctx context.Context, tx *sql.Tx, payload []byte) error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.localHandlers[topic] = append(m.localHandlers[topic], localSub{subscriber: subscriber, h: h})
}

// Init only wires up — no I/O (#8). It resolves config, snapshots the local
// subscriptions into per-(topic, subscriber) relay targets, constructs the single
// relay, and mounts the one inbound sink. The relay does not start here (Start).
func (m *Module) Init(ctx *lifecycle.Context) error {
	m.db = ctx.DB
	m.log = ctx.Log
	m.origin = envOr("MESSAGING_ORIGIN", defaultOrigin)
	m.dsn = envOr("DATABASE_URL", defaultDSN)
	m.retention = envDuration("MESSAGING_RETENTION", 168*time.Hour)
	m.houseTick = envDuration("MESSAGING_HOUSEKEEP_INTERVAL", time.Hour)

	subs := outbox.ParseSubscribers(os.Getenv("EVENTS_SUBSCRIBERS"))
	localTargets := m.buildLocalTargets()

	m.relay = outbox.NewRelay(ctx.DB, "messaging", m.origin, subs, localTargets, m.log)

	// One inbound sink for the whole durable plane. A peer relay POSTs a foreign
	// event here (topic in X-Event-Topic, id in X-Event-Id); the handler dedups
	// per subscriber and runs each local subscriber's effect in its own tx.
	ctx.Mux.HandleFunc("POST /events", m.handleInbound)
	return nil
}

// buildLocalTargets snapshots localHandlers into one relay LocalTarget per
// (topic, subscriber). The relay delivers EVERY drained row to EVERY local target
// (it is not topic-scoped), so each target's Deliver filters by topic: a row of a
// different topic is a no-op success (nothing to do), and only a matching row runs
// consume. Per-target = per-subscriber isolation: one failing subscriber pins
// redelivery of only its own (event_id, subscriber) inbox row.
func (m *Module) buildLocalTargets() []outbox.LocalTarget {
	m.mu.Lock()
	defer m.mu.Unlock()
	var targets []outbox.LocalTarget
	for topic, subs := range m.localHandlers {
		for _, sub := range subs {
			topic, sub := topic, sub // capture per iteration
			targets = append(targets, outbox.LocalTarget{
				Subscriber: sub.subscriber,
				Deliver: func(ctx context.Context, deliveredTopic string, payload []byte, eventID string) error {
					if deliveredTopic != topic {
						return nil // not this subscription's topic — nothing to do
					}
					return m.consume(ctx, sub.subscriber, eventID, payload, sub.h)
				},
			})
		}
	}
	return targets
}

// consume runs one subscriber's handler exactly once for eventID. In ONE tx it
// claims the event in the inbox keyed (event_id, subscriber) (ON CONFLICT DO
// NOTHING); a first delivery (1 row) runs the handler within the SAME tx before
// commit, a duplicate (0 rows) is a committed no-op. Any handler error rolls back
// and propagates → the row stays unsent (local) / a 500 is returned (inbound) →
// redelivered next tick. Each subscriber gets its OWN tx and its OWN inbox row, so
// a failing subscriber can never roll back a different subscriber's effect — the
// fate isolation that (event_id, subscriber) exists to provide.
func (m *Module) consume(ctx context.Context, subscriber, eventID string, payload []byte, h func(context.Context, *sql.Tx, []byte) error) error {
	tx, err := m.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer func() { _ = tx.Rollback() }() // no-op after a successful Commit

	res, err := tx.ExecContext(ctx,
		`INSERT INTO messaging.inbox (event_id, subscriber) VALUES ($1, $2) ON CONFLICT DO NOTHING`,
		eventID, subscriber)
	if err != nil {
		return err
	}
	if n, _ := res.RowsAffected(); n == 0 {
		return tx.Commit() // already processed by this subscriber — idempotent no-op
	}
	if err := h(ctx, tx, payload); err != nil {
		return err
	}
	return tx.Commit()
}

// handleInbound is the receiver side: a peer's relay POSTs a foreign event here.
// It delivers to EVERY local subscriber of the topic, each via its own consume tx
// (dedup + effect). If ANY subscriber fails it replies 500 so the sender's relay
// retries the whole event; the per-subscriber inbox makes already-succeeded
// subscribers a no-op on that retry, so at-least-once delivery is effectively
// exactly-once per subscriber.
func (m *Module) handleInbound(w http.ResponseWriter, r *http.Request) {
	eventID := r.Header.Get("X-Event-Id")
	topic := r.Header.Get("X-Event-Topic")
	if eventID == "" || topic == "" {
		http.Error(w, "missing event id or topic", http.StatusBadRequest)
		return
	}
	payload, err := io.ReadAll(r.Body)
	if err != nil {
		http.Error(w, "read body", http.StatusBadRequest)
		return
	}

	m.mu.Lock()
	subs := append([]localSub(nil), m.localHandlers[topic]...) // snapshot under lock
	m.mu.Unlock()

	for _, sub := range subs {
		if err := m.consume(r.Context(), sub.subscriber, eventID, payload, sub.h); err != nil {
			m.log.Error("inbound consume failed", "subscriber", sub.subscriber, "topic", topic, "event_id", eventID, "err", err)
			http.Error(w, "internal error", http.StatusInternalServerError)
			return
		}
	}
	w.WriteHeader(http.StatusOK)
}

// Start launches the relay, the LISTEN loop, and the housekeeping ticker. Like
// config/outbox it roots a fresh background context so a short Start deadline can't
// kill the loops; Stop cancels them.
//
//nolint:contextcheck // intentional: the loops' lifetime is bounded by Stop, not Start's ctx.
func (m *Module) Start(_ context.Context) error {
	if err := m.relay.Start(context.Background()); err != nil {
		return err
	}
	runCtx, cancel := context.WithCancel(context.Background())
	m.cancel = cancel
	m.wg.Add(2)
	go func() { defer m.wg.Done(); m.listen(runCtx) }()
	go func() { defer m.wg.Done(); m.housekeep(runCtx) }()
	return nil
}

// Stop halts delivery first (messaging is registered last, so reverse-order Stop
// runs it before any consumer tears down), then waits for the background loops to
// exit. relay.Stop waits for the drain loop (and thus any in-flight local consume,
// which runs synchronously inside a drain) to finish; the wg wait covers the LISTEN
// + housekeeping goroutines. Inbound-HTTP consume is already quiesced by the time
// modules Stop (shutdown order: stop HTTP → drain bus → Stop modules, #8).
func (m *Module) Stop(ctx context.Context) error {
	if m.cancel != nil {
		m.cancel()
	}
	if m.relay != nil {
		_ = m.relay.Stop(ctx)
	}
	done := make(chan struct{})
	go func() { m.wg.Wait(); close(done) }()
	select {
	case <-done:
	case <-ctx.Done():
	}
	return nil
}

// listen keeps a dedicated pgx connection LISTENing on messaging_outbox and Kicks
// the relay on every NOTIFY so a freshly-written row drains promptly. It never dies
// on a DB outage: each (re)connect backs off on failure. NOTIFY is best-effort — a
// dropped notification only delays a row until the relay's 500 ms ticker, which is
// the correctness floor. Mirrors config's LISTEN loop.
func (m *Module) listen(ctx context.Context) {
	for ctx.Err() == nil {
		m.listenOnce(ctx)
	}
}

func (m *Module) listenOnce(ctx context.Context) {
	conn, err := pgx.Connect(ctx, m.dsn)
	if err != nil {
		if ctx.Err() == nil {
			m.log.Error("messaging listener connect failed", "err", err)
		}
		m.backoff(ctx)
		return
	}
	// Close with a fresh context: during shutdown the loop ctx is already cancelled.
	//nolint:contextcheck // intentional: close must not use the (possibly-cancelled) loop ctx.
	defer func() { _ = conn.Close(context.Background()) }()

	if _, err := conn.Exec(ctx, "LISTEN "+notifyChannel); err != nil {
		if ctx.Err() == nil {
			m.log.Error("messaging listener LISTEN failed", "err", err)
		}
		m.backoff(ctx)
		return
	}
	// A row may have been written between relay start and this LISTEN; kick once so
	// it isn't stranded until the first tick.
	m.relay.Kick()

	for {
		_, err := conn.WaitForNotification(ctx)
		if ctx.Err() != nil {
			return // clean shutdown
		}
		if err != nil {
			m.log.Error("messaging listener wait failed", "err", err)
			m.backoff(ctx)
			return // reconnect via the outer loop
		}
		m.relay.Kick()
	}
}

// housekeep prunes the ledgers past the retention window on a ticker: sent outbox
// rows and processed inbox rows older than now()-retention. Both DELETEs are
// batch-bounded (ctid IN (… LIMIT n)) so a prune never takes a long lock; the
// interval rides as a bound parameter (never string-interpolated). Self-owned — no
// coupling to scheduler.
func (m *Module) housekeep(ctx context.Context) {
	t := time.NewTicker(m.houseTick)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			m.pruneOnce(ctx)
		}
	}
}

func (m *Module) pruneOnce(ctx context.Context) {
	// make_interval(secs => $1) builds the window from a bound double — the
	// duration is never string-interpolated into SQL.
	secs := m.retention.Seconds()
	if _, err := m.db.ExecContext(ctx,
		`DELETE FROM messaging.inbox WHERE ctid IN (
			SELECT ctid FROM messaging.inbox WHERE processed_at < now() - make_interval(secs => $1) LIMIT $2)`,
		secs, housekeepBatch); err != nil && ctx.Err() == nil {
		m.log.Error("messaging inbox prune failed", "err", err)
	}
	if _, err := m.db.ExecContext(ctx,
		`DELETE FROM messaging.outbox WHERE ctid IN (
			SELECT ctid FROM messaging.outbox WHERE sent_at IS NOT NULL AND sent_at < now() - make_interval(secs => $1) LIMIT $2)`,
		secs, housekeepBatch); err != nil && ctx.Err() == nil {
		m.log.Error("messaging outbox prune failed", "err", err)
	}
}

// backoff waits a short interval, returning early on cancellation.
func (m *Module) backoff(ctx context.Context) {
	t := time.NewTimer(time.Second)
	defer t.Stop()
	select {
	case <-ctx.Done():
	case <-t.C:
	}
}

func envOr(key, def string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return def
}

// envDuration reads key as a Go duration (e.g. "168h", "30m"), falling back to def
// when unset or unparseable.
func envDuration(key string, def time.Duration) time.Duration {
	v := os.Getenv(key)
	if v == "" {
		return def
	}
	d, err := time.ParseDuration(v)
	if err != nil {
		return def
	}
	return d
}
