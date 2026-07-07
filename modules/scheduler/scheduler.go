// Package scheduler is a data-driven event source: it owns a catalogue of named
// schedules (name + interval), and on each tick emits scheduler.fired{Name} for
// every schedule whose interval has elapsed. It runs NO job closures — a closure
// can't cross a process boundary, which would make the scheduler the one module
// that couldn't be split out. Instead it publishes through the same bus→outbox→
// sink seam every domain module uses, so a consumer (e.g. audit's prune) reacts
// in its OWN process and the scheduler is fully decoupled and independently
// deployable (see cmd/scheduler-svc).
//
// Schedules are DATA, not code: the target way to add one is a runtime INSERT
// into scheduler.schedules (via ops/admin), not an edit here. The migration
// seeds only a minimal bootstrap row.
package scheduler

import (
	"context"
	"database/sql"
	"errors"
	"hash/fnv"
	"log/slog"
	"os"
	"strconv"
	"time"

	"gamebackend/api/admin/adminapi"
	"gamebackend/api/scheduler/schedulerevents"
	"gamebackend/bus"
	"gamebackend/lifecycle"
)

// tickInterval is how often the emission loop scans for due schedules. It bounds
// firing latency (a schedule fires within ~1s of becoming due), not accuracy —
// last_fired is authoritative, so a slow tick never double-fires.
const tickInterval = time.Second

// unlockTimeout bounds the advisory-unlock in fire's defer. It uses a fresh
// context so a cancelled loop ctx during shutdown can't skip releasing the lock.
const unlockTimeout = 5 * time.Second

type Module struct {
	log *slog.Logger
	bus *bus.Bus
	db  *sql.DB

	enabled bool

	cancel context.CancelFunc
	done   chan struct{}
}

func (*Module) Name() string       { return "scheduler" }
func (*Module) Requires() []string { return []string{"messaging"} } // durable emitter — needs the transport

const schemaDDL = `
CREATE SCHEMA IF NOT EXISTS scheduler;

-- schedules is the catalogue: one row per named cadence. last_fired defaults to
-- the epoch so a fresh schedule is immediately due on first tick.
CREATE TABLE IF NOT EXISTS scheduler.schedules (
	name             text        PRIMARY KEY,
	interval_seconds int         NOT NULL,
	last_fired       timestamptz NOT NULL DEFAULT to_timestamp(0)
);

-- Minimal bootstrap seed. Adding a schedule is normally a runtime data INSERT,
-- not a code change; this one row (the audit prune cadence) lets the wired-up
-- system do something out of the box. The producer knowing the consumer's name
-- ('audit-prune') is coupling-through-a-string, pushed to data, not eliminated.
INSERT INTO scheduler.schedules (name, interval_seconds)
	VALUES ('audit-prune', 86400)
	ON CONFLICT (name) DO NOTHING;`

// Migrate creates this module's own schema and seeds the bootstrap row. Idempotent.
func (*Module) Migrate(_ context.Context, db *sql.DB) error {
	_, err := db.Exec(schemaDDL)
	return err
}

// Init only wires up — no DB I/O (constraint #8). It stores handles, reads the
// enable gate, and contributes a read-only admin view. The emission loop starts
// in Start.
func (m *Module) Init(ctx *lifecycle.Context) error {
	m.log = ctx.Log
	m.bus = ctx.Bus
	m.db = ctx.DB

	m.enabled = envBool("SCHEDULER_ENABLED", true)
	if !m.enabled {
		m.log.Warn("scheduler DISABLED (SCHEDULER_ENABLED=false) — no schedules will fire")
	}

	ctx.Contribute(adminapi.Slot, adminapi.Item{
		ID:      "scheduler",
		Section: "Platform",
		Label:   "Schedules",
		Render:  m.adminRender,
	})

	return nil
}

// Start launches (unless disabled) the emission loop. Like config it roots a
// fresh background context so a short Start deadline can't kill the loop; Stop
// cancels it.
//
//nolint:contextcheck // intentional: the emission loop's lifetime is bounded by Stop, not Start's ctx.
func (m *Module) Start(_ context.Context) error {
	if !m.enabled {
		return nil
	}
	runCtx, cancel := context.WithCancel(context.Background())
	m.cancel = cancel
	m.done = make(chan struct{})
	go func() {
		defer close(m.done)
		m.loop(runCtx)
	}()
	return nil
}

// Stop cancels the emission loop and waits for it (bounded by ctx).
func (m *Module) Stop(ctx context.Context) error {
	if m.cancel != nil {
		m.cancel()
	}
	if m.done != nil {
		select {
		case <-m.done:
		case <-ctx.Done():
		}
	}
	return nil
}

// loop scans for due schedules every tick until ctx is cancelled.
func (m *Module) loop(ctx context.Context) {
	t := time.NewTicker(tickInterval)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			if err := m.tick(ctx); err != nil && ctx.Err() == nil {
				m.log.Error("scheduler tick failed", "err", err)
			}
		}
	}
}

// tick finds every due schedule and tries to fire each. A per-schedule failure is
// logged and does not abort the others.
func (m *Module) tick(ctx context.Context) error {
	due, err := m.dueSchedules(ctx)
	if err != nil {
		return err
	}
	for _, name := range due {
		if err := m.fire(ctx, name); err != nil && ctx.Err() == nil {
			m.log.Error("scheduler fire failed", "schedule", name, "err", err)
		}
	}
	return nil
}

// dueSchedules returns the names whose interval has elapsed. last_fired is the
// authority: a name reported here may still turn out not-due once fire re-checks
// under the advisory lock (another replica fired it between this scan and the
// lock), which is exactly why fire double-checks.
func (m *Module) dueSchedules(ctx context.Context) ([]string, error) {
	rows, err := m.db.QueryContext(ctx,
		`SELECT name FROM scheduler.schedules
		 WHERE now() - last_fired >= make_interval(secs => interval_seconds)`)
	if err != nil {
		return nil, err
	}
	defer func() { _ = rows.Close() }()
	var names []string
	for rows.Next() {
		var n string
		if err := rows.Scan(&n); err != nil {
			return nil, err
		}
		names = append(names, n)
	}
	return names, rows.Err()
}

// fire emits scheduler.fired for one due schedule exactly once across horizontal
// replicas of this module. The whole sequence — advisory lock, re-check, UPDATE
// last_fired, bus.EmitTx (durable outbox write), commit, unlock — runs on ONE
// short-lived connection
// taken from the pool, because a session-level advisory lock is held only by the
// connection that took it, and the transaction that relies on the lock must share
// that session. Commit happens BEFORE unlock so the next replica to win the lock
// always observes the moved last_fired.
//
// NOTE (#10): explicit pg_advisory_unlock is mandatory. sql.Conn.Close() returns
// the physical connection to the pool WITHOUT necessarily dropping session locks,
// so skipping the unlock could strand the key held on a pooled connection.
func (m *Module) fire(ctx context.Context, name string) error {
	conn, err := m.db.Conn(ctx)
	if err != nil {
		return err
	}
	defer func() { _ = conn.Close() }()

	key := lockKey(name)
	var locked bool
	if err := conn.QueryRowContext(ctx, `SELECT pg_try_advisory_lock($1)`, key).Scan(&locked); err != nil {
		return err
	}
	if !locked {
		// Another replica holds this key (or a colliding one) and is firing now.
		return nil
	}
	// The unlock deliberately roots a fresh context: during shutdown the fire ctx
	// may already be cancelled, and a cancelled ctx would abort the unlock, which
	// must always run (see #10 above).
	//nolint:contextcheck // intentional: releasing the lock must not use the (possibly-cancelled) fire ctx.
	defer func() {
		unlockCtx, cancel := context.WithTimeout(context.Background(), unlockTimeout)
		defer cancel()
		if _, err := conn.ExecContext(unlockCtx, `SELECT pg_advisory_unlock($1)`, key); err != nil {
			m.log.Error("scheduler advisory unlock failed", "schedule", name, "err", err)
		}
	}()

	// Re-check UNDER the lock: a replica that held the lock just before us may
	// already have fired this schedule and moved last_fired. Without this
	// double-check two replicas would both emit for one due window.
	var stillDue bool
	err = conn.QueryRowContext(ctx,
		`SELECT now() - last_fired >= make_interval(secs => interval_seconds)
		 FROM scheduler.schedules WHERE name = $1`, name).Scan(&stillDue)
	if errors.Is(err, sql.ErrNoRows) {
		return nil // schedule deleted between the scan and here
	}
	if err != nil {
		return err
	}
	if !stillDue {
		return nil
	}

	// last_fired bump + durable outbox write commit together, on the locked connection.
	tx, err := conn.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer func() { _ = tx.Rollback() }() // no-op after a successful Commit

	if _, err := tx.ExecContext(ctx,
		`UPDATE scheduler.schedules SET last_fired = now() WHERE name = $1`, name); err != nil {
		return err
	}
	if err := bus.EmitTx(m.bus, tx, schedulerevents.FiredEvent, schedulerevents.Fired{Name: name}); err != nil {
		return err
	}
	return tx.Commit()
}

// lockKey derives a stable 64-bit advisory-lock key from a schedule name via
// FNV-1a. Two different names CAN hash to the same key: they then share one lock,
// which merely serializes their firing — it never breaks exactly-once, because
// the re-check under the lock is per-name against that name's own last_fired.
//
//nolint:gosec // G115: the uint64→int64 wrap is intentional; pg advisory keys use the full bigint range.
func lockKey(name string) int64 {
	h := fnv.New64a()
	_, _ = h.Write([]byte(name))
	return int64(h.Sum64())
}

// adminRender is the read-only "Schedules" admin view: the catalogue with each
// schedule's interval and last-fired time.
func (m *Module) adminRender(ctx context.Context) (adminapi.Content, error) {
	rows, err := m.db.QueryContext(ctx,
		`SELECT name, interval_seconds, last_fired FROM scheduler.schedules ORDER BY name`)
	if err != nil {
		return adminapi.Content{}, err
	}
	defer func() { _ = rows.Close() }()

	table := &adminapi.Table{Columns: []string{"Schedule", "Interval (s)", "Last fired"}}
	for rows.Next() {
		var name string
		var interval int
		var lastFired time.Time
		if err := rows.Scan(&name, &interval, &lastFired); err != nil {
			return adminapi.Content{}, err
		}
		table.Rows = append(table.Rows, []adminapi.Cell{
			{Text: name, Mono: true},
			{Text: strconv.Itoa(interval)},
			{Text: lastFired.Format(time.RFC3339)},
		})
	}
	if err := rows.Err(); err != nil {
		return adminapi.Content{}, err
	}
	return adminapi.Content{Table: table}, nil
}

// envBool reads key as a bool, returning def when unset or unparseable. Local to
// this package per the repo convention of duplicating env helpers (no envutil).
func envBool(key string, def bool) bool {
	v := os.Getenv(key)
	if v == "" {
		return def
	}
	b, err := strconv.ParseBool(v)
	if err != nil {
		return def
	}
	return b
}
