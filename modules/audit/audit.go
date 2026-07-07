// Package audit keeps an append-only ledger of domain events for GameOps
// visibility. It owns schema "audit" and touches no other module's tables.
//
// It listens to the bus GENERICALLY, by topic string — audit never imports a
// domain's payload types, it just json.Marshal's the raw event Data. The cost of
// that decoupling is that the topic list below is a conscious, REQUIRED edit
// point when a new event should be logged (the bus has no wildcard subscribe);
// generic-subscribe only avoids importing the payload type (and its apidiff
// coupling), not the edit itself.
//
// "Exactly one path per topology" (the inventory pattern): in the monolith the
// producer is co-located and the bus.Subscribe handlers fire (the outbox drains
// to nobody). In a split the cross-process event arrives at the synchronous
// /events/audit-* sink (deduped via an inbox) instead — never both, so no double
// counting and no shared table.
//
// Retention is enforced by REACTING to scheduler.fired{Name:"audit-prune"} — the
// scheduler is a decoupled event source, audit does the pruning in its own schema.
package audit

import (
	"context"
	"database/sql"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"os"
	"strconv"
	"strings"
	"time"

	"gamebackend/bus"
	"gamebackend/lifecycle"
	"gamebackend/modules/admin/adminapi"
	"gamebackend/modules/scheduler/schedulerevents"
)

// domainTopics is the catalogue of domain events audit logs. It is the ONE
// coupling point: add a topic string here to start logging a new event. Keep it
// in sync with the producers' <module>events packages — the anti-drift test
// (audit_test.go) asserts this set equals the domain events' declared topics, so
// a topic rename on either side fails the build (topiccheck can't see generic
// Subscribe, this test is the guard). scheduler.fired is DELIBERATELY absent: it
// is CONSUMED (typed bus.On for prune), not logged, so listing it here would fail
// the anti-drift test.
var domainTopics = []string{
	"player.registered",
	"character.created",
	"character.deleted",
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
func (*Module) Requires() []string { return nil } // reacts via the bus/sinks — depends on nobody

const schemaDDL = `
CREATE SCHEMA IF NOT EXISTS audit;

CREATE TABLE IF NOT EXISTS audit.log (
	id      bigserial   PRIMARY KEY,
	topic   text        NOT NULL,
	payload jsonb       NOT NULL,
	at      timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS log_at_idx ON audit.log(at);

-- Inbox: idempotency ledger for the synchronous event sinks (split topology).
-- event_id is the relay's stable key (<schema>:<outbox.id>); a duplicate delivery
-- conflicts and is a committed no-op, so the effect runs at most once. Same
-- pattern as inventory.inbox.
CREATE TABLE IF NOT EXISTS audit.inbox (
	event_id     text        PRIMARY KEY,
	processed_at timestamptz NOT NULL DEFAULT now()
);`

// Migrate creates this module's own schema. Idempotent.
func (*Module) Migrate(_ context.Context, db *sql.DB) error {
	_, err := db.Exec(schemaDDL)
	return err
}

// Init only wires up — no DB I/O (constraint #8). For each domain topic it
// registers BOTH a generic bus subscription (monolith path) and a synchronous
// sink with inbox dedup (split path); it reacts to scheduler.fired for pruning
// (bus.On + a sink), and contributes the admin viewer.
func (m *Module) Init(ctx *lifecycle.Context) error {
	m.log = ctx.Log
	m.db = ctx.DB
	m.retention = envInt("AUDIT_RETENTION_DAYS", defaultRetentionDays)

	for _, topic := range domainTopics {
		topic := topic // one binding per handler (belt-and-braces on the closures)
		// Monolith path: the co-located producer Emits on the in-process bus. We
		// subscribe by raw string — no payload-type import — and marshal e.Data.
		ctx.Bus.Subscribe(topic, func(e bus.Event) { m.record(topic, e.Data) })
		// Split path: a peer's outbox relay POSTs the raw event JSON here. Deduped
		// via inbox, inserted verbatim. Harmless in the monolith (nothing POSTs).
		ctx.Mux.HandleFunc("POST /events/audit-"+slug(topic), m.eventSink(topic))
	}

	// Prune retention as a REACTION to scheduler.fired (not a job closure). Two
	// paths, exactly one per topology: monolith → bus.On (scheduler co-located);
	// split → the /events/scheduler-fired sink (scheduler-svc's relay POSTs here,
	// the path run.ps1/run.sh route to). Using bus.On also makes topiccheck green
	// for scheduler.fired without an allow-unsubscribed directive.
	bus.On(ctx.Bus, schedulerevents.FiredEvent, func(f schedulerevents.Fired) {
		if f.Name != pruneScheduleName {
			return
		}
		if err := m.prune(context.Background(), m.db); err != nil {
			m.log.Error("audit prune failed", "err", err)
		}
	})
	ctx.Mux.HandleFunc("POST /events/scheduler-fired", m.handleSchedulerFired)

	ctx.Contribute(adminapi.Slot, adminapi.Item{ID: adminItemID, Section: "Platform", Label: adminLabel, Render: m.adminRender})
	ctx.Mux.HandleFunc("GET /admin-data/"+adminItemID, m.handleAdminData)
	return nil
}

// record appends one event to the ledger, best-effort. The bus is fire-and-forget,
// so a marshal or insert failure is logged and swallowed — audit must never become
// the reason "you can't add a field to an event" (a payload with unexported fields
// marshals to {} rather than blocking).
func (m *Module) record(topic string, data any) {
	b, err := json.Marshal(data)
	if err != nil {
		m.log.Error("audit marshal failed", "topic", topic, "err", err)
		return
	}
	if _, err := m.db.Exec(`INSERT INTO audit.log (topic, payload) VALUES ($1, $2::jsonb)`, topic, b); err != nil {
		m.log.Error("audit insert failed", "topic", topic, "err", err)
	}
}

// eventSink is the synchronous sink for one domain topic (split path). It dedups
// on X-Event-Id and inserts the raw event body verbatim, returning 200 only after
// the insert commits so the relay retries on failure.
func (m *Module) eventSink(topic string) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		eventID := r.Header.Get("X-Event-Id")
		if eventID == "" {
			http.Error(w, "missing event id", http.StatusBadRequest)
			return
		}
		body, err := io.ReadAll(r.Body)
		if err != nil {
			http.Error(w, "read error", http.StatusBadRequest)
			return
		}
		if !json.Valid(body) {
			http.Error(w, "invalid json", http.StatusBadRequest)
			return
		}
		if err := m.consume(r.Context(), eventID, func(ctx context.Context, tx *sql.Tx) error {
			_, err := tx.ExecContext(ctx, `INSERT INTO audit.log (topic, payload) VALUES ($1, $2::jsonb)`, topic, body)
			return err
		}); err != nil {
			m.log.Error("audit sink insert failed", "event_id", eventID, "topic", topic, "err", err)
			http.Error(w, "internal error", http.StatusInternalServerError)
			return
		}
		w.WriteHeader(http.StatusOK)
	}
}

// handleSchedulerFired is the split-path sink for scheduler.fired: scheduler-svc's
// relay POSTs here (run.ps1/run.sh route scheduler.fired to this exact path). The
// prune runs at most once per event via the inbox; a non-prune schedule name is a
// deduped no-op.
func (m *Module) handleSchedulerFired(w http.ResponseWriter, r *http.Request) {
	eventID := r.Header.Get("X-Event-Id")
	if eventID == "" {
		http.Error(w, "missing event id", http.StatusBadRequest)
		return
	}
	var f schedulerevents.Fired
	if err := json.NewDecoder(r.Body).Decode(&f); err != nil {
		http.Error(w, "invalid json", http.StatusBadRequest)
		return
	}
	if err := m.consume(r.Context(), eventID, func(ctx context.Context, tx *sql.Tx) error {
		if f.Name != pruneScheduleName {
			return nil // marked processed in the inbox; nothing to do
		}
		return m.prune(ctx, tx)
	}); err != nil {
		m.log.Error("audit sink prune failed", "event_id", eventID, "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	w.WriteHeader(http.StatusOK)
}

// execer is the shared surface of *sql.DB and *sql.Tx used by prune, so the same
// query runs on the pool (bus path) or inside the sink's tx (split path).
type execer interface {
	ExecContext(ctx context.Context, query string, args ...any) (sql.Result, error)
}

// prune deletes ledger rows older than the retention window. Idempotent — a
// dropped scheduler.fired tick (best-effort on the bus) is caught by the next one.
func (m *Module) prune(ctx context.Context, q execer) error {
	_, err := q.ExecContext(ctx,
		`DELETE FROM audit.log WHERE at < now() - make_interval(days => $1)`, m.retention)
	return err
}

// consume runs effect exactly once for eventID (inbox dedup in one tx). Identical
// contract to inventory.consume: a first delivery runs effect before commit, a
// duplicate is a committed no-op, any error rolls back so the relay retries.
func (m *Module) consume(ctx context.Context, eventID string, effect func(context.Context, *sql.Tx) error) error {
	tx, err := m.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer func() { _ = tx.Rollback() }() // no-op after a successful Commit

	res, err := tx.ExecContext(ctx,
		`INSERT INTO audit.inbox (event_id) VALUES ($1) ON CONFLICT DO NOTHING`, eventID)
	if err != nil {
		return err
	}
	if n, _ := res.RowsAffected(); n == 0 {
		return tx.Commit() // already processed — idempotent no-op
	}
	if err := effect(ctx, tx); err != nil {
		return err
	}
	return tx.Commit()
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

// slug turns a dotted topic into a URL path segment: "character.created" ->
// "character-created" (the /events/audit-<slug> sink path).
func slug(topic string) string { return strings.ReplaceAll(topic, ".", "-") }

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
