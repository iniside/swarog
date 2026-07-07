package scheduler

import (
	"context"
	"database/sql"
	"encoding/json"
	"io"
	"log/slog"
	"os"
	"sync"
	"testing"
	"time"

	_ "github.com/jackc/pgx/v5/stdlib"

	"gamebackend/api/scheduler/schedulerevents"
	"gamebackend/bus"
)

func discardLog() *slog.Logger { return slog.New(slog.NewTextHandler(io.Discard, nil)) }

func testDB(t *testing.T) *sql.DB {
	t.Helper()
	dsn := os.Getenv("DATABASE_URL")
	if dsn == "" {
		dsn = "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable"
	}
	db, err := sql.Open("pgx", dsn)
	if err != nil {
		t.Skipf("no postgres: %v", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	if err := db.PingContext(ctx); err != nil {
		_ = db.Close()
		t.Skipf("postgres unreachable: %v", err)
	}
	if _, err := db.Exec(schemaDDL); err != nil {
		t.Fatalf("migrate: %v", err)
	}
	// Close via Cleanup, NOT the caller's defer: t.Cleanup runs registered funcs
	// LIFO, and this is registered before any seedSchedule cleanup, so it runs
	// LAST — the per-test DELETE cleanups still see an OPEN db. A plain
	// `defer db.Close()` in the test would close it first and the DELETEs
	// (error-ignored) would silently no-op, leaking rows on the shared Postgres.
	t.Cleanup(func() { _ = db.Close() })
	return db
}

// uniqueName returns a schedule name unique to this test run so tests never
// collide on the shared local Postgres.
func uniqueName(t *testing.T, db *sql.DB) string {
	t.Helper()
	var s string
	if err := db.QueryRow(`SELECT 'test-' || gen_random_uuid()::text`).Scan(&s); err != nil {
		t.Fatal(err)
	}
	return s
}

// seedSchedule inserts (or resets) a schedule with last_fired at the epoch, so it
// is immediately due. It registers cleanup of the schedule row.
func seedSchedule(t *testing.T, db *sql.DB, name string, intervalSeconds int) {
	t.Helper()
	if _, err := db.Exec(
		`INSERT INTO scheduler.schedules (name, interval_seconds, last_fired)
		 VALUES ($1, $2, to_timestamp(0))
		 ON CONFLICT (name) DO UPDATE SET interval_seconds = $2, last_fired = to_timestamp(0)`,
		name, intervalSeconds); err != nil {
		t.Fatalf("seed schedule: %v", err)
	}
	t.Cleanup(func() {
		_, _ = db.Exec(`DELETE FROM scheduler.schedules WHERE name = $1`, name)
	})
}

// fakeTransport is a minimal in-memory bus.Transport standing in for the
// messaging module in these unit tests, so fire's bus.EmitTx call (the durable
// emit) has a transport to write into without pulling in a live messaging
// module (which would also cross a module boundary these tests shouldn't
// need). It only records enqueued payloads — these tests exercise the
// producer side (fire), not durable delivery, which messaging's own tests
// cover.
type fakeTransport struct {
	mu   sync.Mutex
	rows []fakeRow
}

type fakeRow struct {
	topic   string
	payload []byte
}

func (f *fakeTransport) EnqueueTx(_ *sql.Tx, topic string, payload []byte) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.rows = append(f.rows, fakeRow{topic: topic, payload: payload})
	return nil
}

func (f *fakeTransport) SubscribeTx(string, string, func(context.Context, *sql.Tx, []byte) error) {}

// countByName returns how many enqueued rows carry the given schedule name —
// the fakeTransport-backed stand-in for the old outboxCount(db, name) query.
func (f *fakeTransport) countByName(name string) int {
	f.mu.Lock()
	defer f.mu.Unlock()
	n := 0
	for _, r := range f.rows {
		var fired schedulerevents.Fired
		if err := json.Unmarshal(r.payload, &fired); err == nil && fired.Name == name {
			n++
		}
	}
	return n
}

func newModule(db *sql.DB) (*Module, *fakeTransport) {
	b := bus.NewBus(discardLog())
	ft := &fakeTransport{}
	b.SetTransport(ft)
	return &Module{db: db, bus: b, log: discardLog()}, ft
}

// TestFireExactlyOnceUnderConcurrency drives two concurrent fire attempts against
// one due schedule on the same DB (standing in for two horizontal replicas of the
// scheduler). The advisory lock + double-check must yield exactly ONE outbox row
// and one last_fired bump.
func TestFireExactlyOnceUnderConcurrency(t *testing.T) {
	db := testDB(t) // closed via testDB's t.Cleanup, after per-test DELETE cleanups

	name := uniqueName(t, db)
	seedSchedule(t, db, name, 3600) // due (epoch), won't re-arm within the test

	m, ft := newModule(db)
	ctx := context.Background()

	var wg sync.WaitGroup
	for i := 0; i < 2; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			if err := m.fire(ctx, name); err != nil {
				t.Errorf("fire: %v", err)
			}
		}()
	}
	wg.Wait()

	if n := ft.countByName(name); n != 1 {
		t.Fatalf("expected exactly 1 durable emit after concurrent fire, got %d", n)
	}

	// last_fired moved off the epoch exactly once (now not due).
	due, err := m.dueSchedules(ctx)
	if err != nil {
		t.Fatal(err)
	}
	for _, n := range due {
		if n == name {
			t.Fatalf("schedule %q still due after firing", name)
		}
	}
}

// TestFiresAgainAfterInterval verifies a schedule re-arms: an immediate second
// fire is a no-op (not due), but after the interval elapses it fires again.
func TestFiresAgainAfterInterval(t *testing.T) {
	db := testDB(t) // closed via testDB's t.Cleanup, after per-test DELETE cleanups

	name := uniqueName(t, db)
	seedSchedule(t, db, name, 1) // 1s interval

	m, ft := newModule(db)
	ctx := context.Background()

	if err := m.fire(ctx, name); err != nil {
		t.Fatalf("first fire: %v", err)
	}
	if n := ft.countByName(name); n != 1 {
		t.Fatalf("after first fire want 1 durable emit, got %d", n)
	}

	// Immediately not due — second fire is a no-op.
	if err := m.fire(ctx, name); err != nil {
		t.Fatalf("second (immediate) fire: %v", err)
	}
	if n := ft.countByName(name); n != 1 {
		t.Fatalf("immediate refire should be a no-op, got %d durable emits", n)
	}

	// After the interval it is due again.
	time.Sleep(1200 * time.Millisecond)
	if err := m.fire(ctx, name); err != nil {
		t.Fatalf("third fire: %v", err)
	}
	if n := ft.countByName(name); n != 2 {
		t.Fatalf("after interval want 2 durable emits, got %d", n)
	}
}
