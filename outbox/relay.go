// Package outbox is a transactional-outbox relay: a generic, domain-agnostic
// helper that drains rows a publishing module wrote (in the same DB transaction
// as its domain change) and delivers them to in-process local targets and to
// remote subscribers over HTTP. It is NOT part of core and imports no module
// implementation — it works purely off a schema name, an origin string, a
// topic→URLs config, and function-typed local targets, so the same relay serves
// any producer.
//
// Single-owner drain (BLOCKER-1 fix). Every process that shares one outbox table
// stamps its rows with a stable `origin`, and each relay drains ONLY its own
// origin's rows (`WHERE origin = $1 … FOR UPDATE SKIP LOCKED`). So a foreign
// process's relay can never mark-sent (and thus silently swallow) a row it does
// not subscribe to; the producing process alone owns delivery of its events.
//
// Delivery contract:
//   - At-least-once. A stable event id (`<schema>:<outbox.id>`) is sent with each
//     delivery (X-Event-Id header for remote, eventID arg for local) so an
//     idempotent subscriber (an inbox keyed on that id) dedups retries.
//   - Local delivery is unconditional. Local targets are always attempted,
//     independently of whether any remote URLs exist — the monolith (empty
//     EVENTS_SUBSCRIBERS) still delivers to its in-process subscribers.
//   - Per-(topic, target) ordering. Rows are delivered in ascending outbox id; on
//     the first failure to a given (topic, url) or (topic, local subscriber) the
//     relay stops advancing for THAT (topic, target) this tick (a later event of
//     the same topic can't overtake an earlier one — no delete-before-create). A
//     poison event of one topic therefore can't stall a different topic to the
//     same peer. A row is marked sent only once EVERY target — each local
//     subscriber and each remote URL — accepted it. A row with no local targets
//     and no subscribers is delivered to nobody = success, marked sent at once.
//   - The remote POST carries the topic in an X-Event-Topic header so the
//     receiver can route from a single `POST /events` endpoint.
package outbox

import (
	"bytes"
	"context"
	"database/sql"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"regexp"
	"strings"
	"time"
)

// defaultInterval is how often the relay drains the outbox.
const defaultInterval = 500 * time.Millisecond

// identRe guards the schema name interpolated into SQL (there is no bind
// parameter for an identifier). Only a plain SQL identifier is accepted, so the
// interpolation below can never carry attacker-controlled input.
var identRe = regexp.MustCompile(`^[a-zA-Z_][a-zA-Z0-9_]*$`)

// LocalTarget is an in-process delivery target. Deliver is invoked (with the
// stable eventID) for every drained row whose delivery this target owns; it must
// be idempotent (dedup on eventID) since delivery is at-least-once. Subscriber is
// a stable name used only to key the per-(topic, subscriber) block gate so one
// failing local subscriber cannot stall another. The relay stays domain-agnostic:
// LocalTarget is a plain function type, no module import.
type LocalTarget struct {
	Subscriber string
	Deliver    func(ctx context.Context, topic string, payload []byte, eventID string) error
}

// Relay drains a schema's outbox table (only rows of its own origin) and delivers
// each row to every local target and every remote HTTP subscriber.
type Relay struct {
	db           *sql.DB
	schema       string
	origin       string // this process's stable identity; drains only rows stamped with it
	subscribers  map[string][]string // topic -> subscriber URLs
	localTargets []LocalTarget       // in-process delivery targets, always attempted
	client       *http.Client
	interval     time.Duration
	log          *slog.Logger

	kick   chan struct{} // capacity-1 wake signal; Kick coalesces NOTIFY into an immediate drain
	cancel context.CancelFunc
	done   chan struct{}
}

// NewRelay builds a relay for the given schema's outbox table. It drains only
// rows stamped with origin (the writing process's stable identity), so processes
// sharing one outbox never mark-sent each other's rows. subscribers maps each
// event topic to the URLs to POST it to; localTargets are in-process delivery
// targets attempted unconditionally (empty subscribers + empty localTargets =
// delivered to nobody = marked sent at once). It panics on a non-identifier
// schema — a wiring bug, loud at startup.
func NewRelay(db *sql.DB, schema, origin string, subscribers map[string][]string, localTargets []LocalTarget, log *slog.Logger) *Relay {
	if !identRe.MatchString(schema) {
		panic(fmt.Sprintf("outbox: invalid schema name %q", schema))
	}
	if subscribers == nil {
		subscribers = map[string][]string{}
	}
	return &Relay{
		db:           db,
		schema:       schema,
		origin:       origin,
		subscribers:  subscribers,
		localTargets: localTargets,
		client:       &http.Client{Timeout: 5 * time.Second},
		interval:     defaultInterval,
		log:          log,
		kick:         make(chan struct{}, 1),
	}
}

// Kick requests an immediate drain, coalescing bursts: a NOTIFY from the outbox
// insert trigger calls this so a freshly-written row is delivered promptly rather
// than waiting up to a full ticker interval. It never blocks (capacity-1 buffer +
// default), so the LISTEN loop is never stalled by the drain loop, and NOTIFY
// stays a pure latency optimization on top of the ticker's correctness floor. Safe
// to call before Start (nil-safe: a nil channel simply takes the default).
func (r *Relay) Kick() {
	select {
	case r.kick <- struct{}{}:
	default:
	}
}

// Start launches the drain loop in the background and returns immediately. The
// passed ctx bounds only startup (there is no I/O here); the loop runs under its
// own background context until Stop cancels it — so a short Start deadline can't
// kill the relay.
//
//nolint:contextcheck // intentional: the drain loop's lifetime is bounded by Stop, not Start's ctx.
func (r *Relay) Start(_ context.Context) error {
	// The loop must outlive the Start deadline, so it deliberately roots a fresh
	// background context rather than deriving from the passed one.
	runCtx, cancel := context.WithCancel(context.Background())
	r.cancel = cancel
	r.done = make(chan struct{})
	go func() {
		defer close(r.done)
		r.run(runCtx)
	}()
	return nil
}

// Stop cancels the drain loop and waits for it to exit (bounded by ctx).
func (r *Relay) Stop(ctx context.Context) error {
	if r.cancel != nil {
		r.cancel()
	}
	if r.done == nil {
		return nil
	}
	select {
	case <-r.done:
	case <-ctx.Done():
	}
	return nil
}

func (r *Relay) run(ctx context.Context) {
	t := time.NewTicker(r.interval)
	defer t.Stop()
	// Drain once immediately so a monolith isn't left with a startup backlog for
	// up to a full interval.
	if err := r.drainOnce(ctx); err != nil && ctx.Err() == nil {
		r.log.Error("outbox drain failed", "schema", r.schema, "err", err)
	}
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			if err := r.drainOnce(ctx); err != nil && ctx.Err() == nil {
				r.log.Error("outbox drain failed", "schema", r.schema, "err", err)
			}
		case <-r.kick:
			// A NOTIFY woke us — drain immediately instead of waiting for the tick.
			if err := r.drainOnce(ctx); err != nil && ctx.Err() == nil {
				r.log.Error("outbox drain failed", "schema", r.schema, "err", err)
			}
		}
	}
}

// outRow is one unsent outbox row.
type outRow struct {
	id      int64
	topic   string
	payload []byte
}

// drainOnce reads every unsent row of this relay's origin in id order, delivers
// them (per-(topic, target) ordering enforced by deliver), and marks the
// fully-delivered ones sent.
//
// Tx boundary: the locking SELECT (FOR UPDATE SKIP LOCKED in pending) and the
// markSent UPDATEs run in ONE transaction per drain. Postgres releases row locks
// only at commit/rollback, so markSent MUST share the tx that took the locks —
// otherwise the FOR UPDATE lock would already be gone and offer no protection.
// Holding the locks across the batch (including the delivery I/O) is deliberate
// belt-and-suspenders: SKIP LOCKED means any concurrent same-origin drainer skips
// these rows rather than double-delivering them. A markSent failure poisons the
// tx, so we abort and let the next tick redeliver (at-least-once; the inbox dedups).
func (r *Relay) drainOnce(ctx context.Context) error {
	tx, err := r.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer func() { _ = tx.Rollback() }() // no-op after a successful Commit
	pending, err := r.pending(ctx, tx)
	if err != nil {
		return err
	}
	if len(pending) == 0 {
		return tx.Commit() // nothing locked; release cleanly
	}
	sent := r.deliver(ctx, pending, func(ctx context.Context, url, topic, eventID string, payload []byte) error {
		return r.post(ctx, url, topic, eventID, payload)
	})
	for _, id := range sent {
		if err := r.markSent(ctx, tx, id); err != nil {
			// The tx is now poisoned; abort and redeliver next tick.
			return fmt.Errorf("outbox mark sent id %d: %w", id, err)
		}
	}
	return tx.Commit()
}

// deliver decides which rows are fully delivered given a batch of unsent rows in
// ascending id order. Each row is delivered to EVERY local target (unconditional,
// independent of remote URLs — the monolith path) AND every remote URL for its
// topic; post handles the remote leg. It enforces per-(topic, target) ordering:
// once delivery to a (topic, url) or (topic, local subscriber) fails, no further
// row of THAT topic is delivered to THAT target this batch, so an earlier event is
// never overtaken by a later one — and a poison event of one topic can't stall a
// different topic to the same peer. A row is returned (to be marked sent) only
// when every target accepted it; a row with no targets at all is delivered to
// nobody and counts as sent. Pure over post + the LocalTarget Deliver funcs —
// unit-tested without Postgres or HTTP.
func (r *Relay) deliver(ctx context.Context, pending []outRow, post func(ctx context.Context, url, topic, eventID string, payload []byte) error) []int64 {
	blocked := map[string]bool{} // (topic, target) keys that failed this batch
	var sent []int64
	for _, row := range pending {
		eventID := fmt.Sprintf("%s:%d", r.schema, row.id)
		allOK := true
		// Local targets first, unconditionally (independent of remote URLs).
		for _, lt := range r.localTargets {
			key := "L\x00" + lt.Subscriber + "\x00" + row.topic
			if blocked[key] {
				allOK = false // can't skip ahead of an earlier undelivered row
				continue
			}
			if err := lt.Deliver(ctx, row.topic, row.payload, eventID); err != nil {
				r.log.Warn("outbox local delivery failed", "subscriber", lt.Subscriber, "topic", row.topic, "event_id", eventID, "err", err)
				blocked[key] = true
				allOK = false
			}
		}
		// Then remote subscribers for this topic.
		for _, url := range r.subscribers[row.topic] {
			key := "R\x00" + url + "\x00" + row.topic
			if blocked[key] {
				allOK = false
				continue
			}
			if err := post(ctx, url, row.topic, eventID, row.payload); err != nil {
				r.log.Warn("outbox delivery failed", "url", url, "topic", row.topic, "event_id", eventID, "err", err)
				blocked[key] = true
				allOK = false
			}
		}
		if allOK {
			sent = append(sent, row.id)
		}
	}
	return sent
}

// pending reads this relay's own unsent rows, locking them FOR UPDATE SKIP LOCKED
// so a concurrent same-origin drainer skips (never double-drains) them. The lock
// is held until the caller's tx commits/rolls back — see drainOnce's tx boundary.
func (r *Relay) pending(ctx context.Context, tx *sql.Tx) ([]outRow, error) {
	// #nosec G201 -- schema is validated as a bare SQL identifier in NewRelay.
	q := fmt.Sprintf(`SELECT id, topic, payload FROM %s.outbox WHERE sent_at IS NULL AND origin = $1 ORDER BY id FOR UPDATE SKIP LOCKED`, r.schema)
	rows, err := tx.QueryContext(ctx, q, r.origin)
	if err != nil {
		return nil, err
	}
	defer func() { _ = rows.Close() }()
	var out []outRow
	for rows.Next() {
		var row outRow
		if err := rows.Scan(&row.id, &row.topic, &row.payload); err != nil {
			return nil, err
		}
		out = append(out, row)
	}
	return out, rows.Err()
}

// markSent must run in the SAME tx as pending's locking SELECT (see drainOnce).
func (r *Relay) markSent(ctx context.Context, tx *sql.Tx, id int64) error {
	// #nosec G201 -- schema is validated as a bare SQL identifier in NewRelay.
	q := fmt.Sprintf(`UPDATE %s.outbox SET sent_at = now() WHERE id = $1`, r.schema)
	_, err := tx.ExecContext(ctx, q, id)
	return err
}

// post delivers one row to one subscriber. The stable event id rides in the
// X-Event-Id header so the subscriber can dedup, and the topic rides in the
// X-Event-Topic header so the receiver can route from a single POST /events
// endpoint; the body is the raw event JSON exactly as the producer stored it. Any
// non-2xx (or transport error) is a failure, which stops advancing for this
// (topic, subscriber) (retried next tick).
func (r *Relay) post(ctx context.Context, url, topic, eventID string, payload []byte) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(payload))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("X-Event-Id", eventID)
	req.Header.Set("X-Event-Topic", topic)
	resp, err := r.client.Do(req)
	if err != nil {
		return err
	}
	defer func() { _ = resp.Body.Close() }()
	_, _ = io.Copy(io.Discard, resp.Body) // drain for connection reuse
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("outbox: subscriber %s returned %d", url, resp.StatusCode)
	}
	return nil
}

// ParseSubscribers parses the EVENTS_SUBSCRIBERS env value into a topic→URLs map.
//
// Shape: semicolon-separated entries, each `topic=url` (URLs may be
// comma-separated for multiple subscribers, and a topic may repeat — both
// append). Whitespace around tokens is trimmed; blank entries are skipped.
// Empty/unset input yields an empty map (the monolith: no remote subscribers).
//
// Example:
//
//	character.created=http://localhost:8081/events/character-created;character.deleted=http://localhost:8081/events/character-deleted
func ParseSubscribers(raw string) map[string][]string {
	out := map[string][]string{}
	for entry := range strings.SplitSeq(raw, ";") {
		entry = strings.TrimSpace(entry)
		if entry == "" {
			continue
		}
		topic, urls, ok := strings.Cut(entry, "=")
		topic = strings.TrimSpace(topic)
		if !ok || topic == "" {
			continue
		}
		for u := range strings.SplitSeq(urls, ",") {
			if u = strings.TrimSpace(u); u != "" {
				out[topic] = append(out[topic], u)
			}
		}
	}
	return out
}
