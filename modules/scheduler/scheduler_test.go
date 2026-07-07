package scheduler

import (
	"context"
	"database/sql"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"sync"
	"testing"
	"time"

	_ "github.com/jackc/pgx/v5/stdlib"

	"gamebackend/bus"
	"gamebackend/modules/scheduler/schedulerevents"
	"gamebackend/outbox"
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
// is immediately due. It registers cleanup of the schedule and its outbox rows.
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
		_, _ = db.Exec(`DELETE FROM scheduler.outbox WHERE payload->>'Name' = $1`, name)
		_, _ = db.Exec(`DELETE FROM scheduler.schedules WHERE name = $1`, name)
	})
}

func outboxCount(t *testing.T, db *sql.DB, name string) int {
	t.Helper()
	var n int
	if err := db.QueryRow(`SELECT count(*) FROM scheduler.outbox WHERE payload->>'Name' = $1`, name).Scan(&n); err != nil {
		t.Fatalf("outbox count: %v", err)
	}
	return n
}

func newModule(db *sql.DB) *Module {
	return &Module{db: db, bus: bus.NewBus(discardLog()), log: discardLog()}
}

// TestFireExactlyOnceUnderConcurrency drives two concurrent fire attempts against
// one due schedule on the same DB (standing in for two horizontal replicas of the
// scheduler). The advisory lock + double-check must yield exactly ONE outbox row
// and one last_fired bump.
func TestFireExactlyOnceUnderConcurrency(t *testing.T) {
	db := testDB(t)
	defer func() { _ = db.Close() }()

	name := uniqueName(t, db)
	seedSchedule(t, db, name, 3600) // due (epoch), won't re-arm within the test

	m := newModule(db)
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

	if n := outboxCount(t, db, name); n != 1 {
		t.Fatalf("expected exactly 1 outbox row after concurrent fire, got %d", n)
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
	db := testDB(t)
	defer func() { _ = db.Close() }()

	name := uniqueName(t, db)
	seedSchedule(t, db, name, 1) // 1s interval

	m := newModule(db)
	ctx := context.Background()

	if err := m.fire(ctx, name); err != nil {
		t.Fatalf("first fire: %v", err)
	}
	if n := outboxCount(t, db, name); n != 1 {
		t.Fatalf("after first fire want 1 outbox row, got %d", n)
	}

	// Immediately not due — second fire is a no-op.
	if err := m.fire(ctx, name); err != nil {
		t.Fatalf("second (immediate) fire: %v", err)
	}
	if n := outboxCount(t, db, name); n != 1 {
		t.Fatalf("immediate refire should be a no-op, got %d outbox rows", n)
	}

	// After the interval it is due again.
	time.Sleep(1200 * time.Millisecond)
	if err := m.fire(ctx, name); err != nil {
		t.Fatalf("third fire: %v", err)
	}
	if n := outboxCount(t, db, name); n != 2 {
		t.Fatalf("after interval want 2 outbox rows, got %d", n)
	}
}

// TestRelayDeliversFiredToSink wires a real outbox relay against a fake HTTP sink
// and asserts a fired event is POSTed with the correct scheduler.fired payload —
// the split delivery path (scheduler-svc → audit sink).
func TestRelayDeliversFiredToSink(t *testing.T) {
	db := testDB(t)
	defer func() { _ = db.Close() }()

	name := uniqueName(t, db)
	seedSchedule(t, db, name, 3600)

	var mu sync.Mutex
	var bodies [][]byte
	sink := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		b, _ := io.ReadAll(r.Body)
		mu.Lock()
		bodies = append(bodies, b)
		mu.Unlock()
		w.WriteHeader(http.StatusOK)
	}))
	defer sink.Close()

	m := newModule(db)
	ctx := context.Background()
	if err := m.fire(ctx, name); err != nil {
		t.Fatalf("fire: %v", err)
	}

	relay := outbox.NewRelay(db, "scheduler",
		map[string][]string{schedulerevents.FiredEvent.Topic(): {sink.URL}}, discardLog())
	if err := relay.Start(ctx); err != nil {
		t.Fatalf("relay start: %v", err)
	}
	defer func() { _ = relay.Stop(ctx) }()

	// Poll for our event among delivered bodies (the relay also drains any other
	// unsent rows on the shared DB, so match by name).
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		mu.Lock()
		for _, b := range bodies {
			var f schedulerevents.Fired
			if err := json.Unmarshal(b, &f); err == nil && f.Name == name {
				mu.Unlock()
				return // delivered with the right payload
			}
		}
		mu.Unlock()
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatalf("relay did not deliver scheduler.fired for %q within timeout", name)
}
