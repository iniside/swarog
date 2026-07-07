// Package audit keeps an append-only ledger of domain events for GameOps
// visibility. It owns schema "audit" and touches no other module's tables.
//
// It listens to the bus GENERICALLY, by topic string — audit never imports a
// domain's payload types, it just records the raw event JSON. The cost of that
// decoupling is that the topic lists below are a conscious, REQUIRED edit point
// when a new event should be logged (the bus has no wildcard subscribe);
// generic-subscribe only avoids importing the payload type (and its apidiff
// coupling), not the edit itself.
//
// Two planes, chosen by the PRODUCER's durability intent (never by topology):
//
//   - durableTopics — producers that emit via the messaging durable plane
//     (EmitTx → outbox). audit subscribes with bus.OnTxRaw (untyped durable): the
//     transport hands the raw JSON and runs the ledger insert inside its
//     per-(event_id,"audit") inbox-dedup tx, exactly-once in BOTH topologies.
//     messaging owns the outbox/inbox/HTTP receive — audit just records.
//   - bestEffortTopics — producers that emit plain bus.Emit (no outbox). Nothing
//     durable ever carries them, so audit logs them in-process best-effort via
//     ctx.Bus.Subscribe. A dropped event is acceptable (fire-and-forget); making
//     them durable would need those producers to adopt EmitTx first.
//
// Retention is enforced by REACTING to scheduler.fired{Name:"audit-prune"} on the
// durable plane (bus.OnTx) — the scheduler is a decoupled event source, audit does
// the pruning in its own schema inside the handed tx.
package audit

import (
	"context"
	"database/sql"
	"encoding/json"
	"log/slog"
	"net/http"
	"os"
	"strconv"
	"time"

	"gamebackend/bus"
	"gamebackend/lifecycle"
	"gamebackend/modules/admin/adminapi"
	"gamebackend/modules/scheduler/schedulerevents"
)

// durableTopics are the domain events that traverse the messaging durable plane
// (their producers emit via EmitTx). audit records them via bus.OnTxRaw, atomic
// with messaging's inbox dedup. The anti-drift test (audit_test.go) asserts this
// set equals the producers' declared topics, so a rename on either side fails the
// build (topiccheck sees OnTxRaw, but this test also guards the exact set).
var durableTopics = []string{
	"character.created",
	"character.deleted",
}

// bestEffortTopics are domain events whose producers emit best-effort only (plain
// bus.Emit, no outbox), so audit logs them in-process via ctx.Bus.Subscribe — no
// durable delivery exists to carry them. The anti-drift test guards this set too.
// scheduler.fired is DELIBERATELY in neither list: it is CONSUMED (bus.OnTx for
// prune), not logged, so listing it here would fail the anti-drift test.
var bestEffortTopics = []string{
	"player.registered",
	"config.changed",
	"match.finished",
}

// pruneScheduleName is the scheduler.fired Name audit reacts to. It is shared
// vocabulary (a string), like a topic — the scheduler seeds this schedule name,
// audit reacts to it.
const pruneScheduleName = "audit-prune"

const defaultRetentionDays = 30

const (
	adminItemID = "audit"
	adminLabel  = "Audit Log"
)

type Module struct {
	log       *slog.Logger
	db        *sql.DB
	retention int // days; from AUDIT_RETENTION_DAYS
}

func (*Module) Name() string       { return "audit" }
func (*Module) Requires() []string { return []string{"messaging"} } // durable plane (OnTx/OnTxRaw); best-effort topics need no dep

const schemaDDL = `
CREATE SCHEMA IF NOT EXISTS audit;

CREATE TABLE IF NOT EXISTS audit.log (
	id      bigserial   PRIMARY KEY,
	topic   text        NOT NULL,
	payload jsonb       NOT NULL,
	at      timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS log_at_idx ON audit.log(at);`

// Migrate creates this module's own schema. Idempotent.
func (*Module) Migrate(_ context.Context, db *sql.DB) error {
	_, err := db.Exec(schemaDDL)
	return err
}

// Init only wires up — no DB I/O (constraint #8). Durable topics subscribe via
// bus.OnTxRaw (messaging owns the outbox/inbox/HTTP), best-effort topics via the
// in-process bus; it reacts to scheduler.fired for pruning and contributes the
// admin viewer.
func (m *Module) Init(ctx *lifecycle.Context) error {
	m.log = ctx.Log
	m.db = ctx.DB
	m.retention = envInt("AUDIT_RETENTION_DAYS", defaultRetentionDays)

	// Durable plane: the producer emitted via EmitTx; messaging delivers here
	// through its per-(event_id,"audit") inbox-dedup tx, in BOTH topologies. We
	// subscribe by raw string (no payload-type import) and insert the raw JSON on
	// the HANDED tx, so the ledger row commits atomically with the dedup row.
	for _, topic := range durableTopics {
		topic := topic // one binding per handler (belt-and-braces on the closures)
		bus.OnTxRaw(ctx.Bus, topic, "audit", func(ctx context.Context, tx *sql.Tx, raw json.RawMessage) error {
			return m.record(ctx, tx, topic, raw)
		})
	}

	// Best-effort plane: these producers emit plain bus.Emit (no outbox), so
	// nothing durable ever carries them. Log in-process, fire-and-forget — a
	// dropped event is acceptable, and there is no HTTP sink because no relay POSTs.
	for _, topic := range bestEffortTopics {
		topic := topic
		ctx.Bus.Subscribe(topic, func(e bus.Event) { m.recordBestEffort(topic, e.Data) })
	}

	// Prune retention as a REACTION to scheduler.fired on the durable plane. The
	// prune runs inside messaging's per-(event_id,"audit") inbox-dedup tx, so a
	// redelivered tick is a committed no-op; a non-prune schedule name returns nil
	// (marked processed, nothing to do).
	bus.OnTx(ctx.Bus, schedulerevents.FiredEvent, "audit", func(ctx context.Context, tx *sql.Tx, f schedulerevents.Fired) error {
		if f.Name != pruneScheduleName {
			return nil
		}
		return m.prune(ctx, tx)
	})

	ctx.Contribute(adminapi.Slot, adminapi.Item{ID: adminItemID, Section: "Platform", Label: adminLabel, Render: m.adminRender})
	ctx.Mux.HandleFunc("GET /admin-data/"+adminItemID, m.handleAdminData)
	return nil
}

// record appends one durable event to the ledger on the handed tx (messaging's
// inbox-dedup tx), so the ledger row commits atomically with the (event_id,"audit")
// dedup row — recorded at most once, retried on failure.
func (m *Module) record(ctx context.Context, tx *sql.Tx, topic string, raw json.RawMessage) error {
	_, err := tx.ExecContext(ctx,
		`INSERT INTO audit.log (topic, payload) VALUES ($1, $2::jsonb)`, topic, []byte(raw))
	return err
}

// recordBestEffort appends one best-effort event to the ledger using the pool
// directly (no tx, no dedup). The bus is fire-and-forget, so a marshal or insert
// failure is logged and swallowed — audit must never become the reason "you can't
// add a field to an event" (a payload with unexported fields marshals to {} rather
// than blocking). Distinct from record because best-effort topics have no durable
// producer and therefore no transport-handed tx.
func (m *Module) recordBestEffort(topic string, data any) {
	b, err := json.Marshal(data)
	if err != nil {
		m.log.Error("audit marshal failed", "topic", topic, "err", err)
		return
	}
	if _, err := m.db.Exec(`INSERT INTO audit.log (topic, payload) VALUES ($1, $2::jsonb)`, topic, b); err != nil {
		m.log.Error("audit insert failed", "topic", topic, "err", err)
	}
}

// prune deletes ledger rows older than the retention window on the handed tx
// (messaging's inbox-dedup tx). Idempotent — a dropped scheduler.fired tick is
// caught by the next one.
func (m *Module) prune(ctx context.Context, tx *sql.Tx) error {
	_, err := tx.ExecContext(ctx,
		`DELETE FROM audit.log WHERE at < now() - make_interval(days => $1)`, m.retention)
	return err
}

// handleAdminData serves the admin content over HTTP as adminapi.ItemData so a
// remote admin process can render it, using the SAME adminRender logic.
func (m *Module) handleAdminData(w http.ResponseWriter, r *http.Request) {
	content, err := m.adminRender(r.Context())
	if err != nil {
		m.log.Error("admin-data render failed", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(adminapi.ItemData{
		ID: adminItemID, Section: "Platform", Label: adminLabel, Content: content,
	})
}

// adminRender is the read-only "Audit Log" admin view: the most recent 100 entries.
func (m *Module) adminRender(ctx context.Context) (adminapi.Content, error) {
	rows, err := m.db.QueryContext(ctx,
		`SELECT topic, payload::text, at FROM audit.log ORDER BY at DESC, id DESC LIMIT 100`)
	if err != nil {
		return adminapi.Content{}, err
	}
	defer func() { _ = rows.Close() }()

	table := &adminapi.Table{Columns: []string{"Topic", "Payload", "At"}}
	for rows.Next() {
		var topic, payload string
		var at time.Time
		if err := rows.Scan(&topic, &payload, &at); err != nil {
			return adminapi.Content{}, err
		}
		table.Rows = append(table.Rows, []adminapi.Cell{
			{Text: topic, Mono: true},
			{Text: truncate(payload, 80), Mono: true},
			{Text: at.Format(time.RFC3339)},
		})
	}
	if err := rows.Err(); err != nil {
		return adminapi.Content{}, err
	}
	return adminapi.Content{Table: table}, nil
}

// truncate shortens s to at most n runes, appending an ellipsis when cut (rune-safe
// so a multibyte payload never splits mid-character).
func truncate(s string, n int) string {
	r := []rune(s)
	if len(r) <= n {
		return s
	}
	return string(r[:n]) + "…"
}

// envInt reads key as an int, returning def when unset or unparseable. Local to
// this package per the repo convention of duplicating env helpers (no envutil).
func envInt(key string, def int) int {
	v := os.Getenv(key)
	if v == "" {
		return def
	}
	n, err := strconv.Atoi(v)
	if err != nil {
		return def
	}
	return n
}
