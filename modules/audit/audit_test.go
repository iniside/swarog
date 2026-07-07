package audit

import (
	"context"
	"database/sql"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"testing"
	"time"

	_ "github.com/jackc/pgx/v5/stdlib"

	"gamebackend/bus"
	"gamebackend/lifecycle"
	"gamebackend/modules/accounts/accountsevents"
	"gamebackend/modules/characters/charactersevents"
	"gamebackend/modules/config/configevents"
	"gamebackend/modules/match/matchevents"
	"gamebackend/modules/scheduler/schedulerevents"
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

// initModule wires an audit Module against db via a real lifecycle.Context, so the
// generic bus subscriptions and the scheduler.fired bus.On are registered exactly
// as in production. Returns the module and the context (its Bus drives the events).
func initModule(t *testing.T, db *sql.DB) (*Module, *lifecycle.Context) {
	t.Helper()
	ctx := lifecycle.NewContext(discardLog())
	ctx.DB = db
	m := &Module{}
	if err := m.Init(ctx); err != nil {
		t.Fatalf("init: %v", err)
	}
	return m, ctx
}

// uniqueTopic returns a marker topic unique to this test run so assertions and
// cleanup never collide on the shared local Postgres.
func uniqueTopic(t *testing.T, db *sql.DB) string {
	t.Helper()
	var s string
	if err := db.QueryRow(`SELECT 'test.' || replace(gen_random_uuid()::text,'-','')`).Scan(&s); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _, _ = db.Exec(`DELETE FROM audit.log WHERE topic = $1`, s) })
	return s
}

func countByTopic(t *testing.T, db *sql.DB, topic string) int {
	t.Helper()
	var n int
	if err := db.QueryRow(`SELECT count(*) FROM audit.log WHERE topic = $1`, topic).Scan(&n); err != nil {
		t.Fatalf("count: %v", err)
	}
	return n
}

// TestBusEventIsLogged emits a real typed event and asserts the generic bus
// subscription writes a row with the right topic and a JSON payload.
func TestBusEventIsLogged(t *testing.T) {
	db := testDB(t)
	defer func() { _ = db.Close() }()

	m, ctx := initModule(t, db)
	matchID := uniqueRun(t, db)
	t.Cleanup(func() { _, _ = db.Exec(`DELETE FROM audit.log WHERE payload->>'MatchID' = $1`, matchID) })

	bus.Emit(ctx.Bus, matchevents.FinishedEvent,
		matchevents.Finished{MatchID: matchID, Winner: "alice", Loser: "bob"})
	ctx.Bus.Close() // drains every subscriber goroutine

	var topic, winner string
	err := db.QueryRow(
		`SELECT topic, payload->>'Winner' FROM audit.log WHERE payload->>'MatchID' = $1`, matchID).
		Scan(&topic, &winner)
	if err != nil {
		t.Fatalf("expected an audit row for the match: %v", err)
	}
	if topic != matchevents.FinishedEvent.Topic() {
		t.Fatalf("topic = %q, want %q", topic, matchevents.FinishedEvent.Topic())
	}
	if winner != "alice" {
		t.Fatalf("payload Winner = %q, want alice (payload not JSON-marshalled?)", winner)
	}
	_ = m
}

// TestPruneViaBus reacts to scheduler.fired{audit-prune} on the bus and deletes
// rows past the retention window, keeping fresh ones.
func TestPruneViaBus(t *testing.T) {
	db := testDB(t)
	defer func() { _ = db.Close() }()

	_, ctx := initModule(t, db)
	oldTopic := uniqueTopic(t, db)
	freshTopic := uniqueTopic(t, db)
	insertAgedRow(t, db, oldTopic, 60)  // 60 days old — past default 30d retention
	insertAgedRow(t, db, freshTopic, 0) // now — safe

	bus.Emit(ctx.Bus, schedulerevents.FiredEvent, schedulerevents.Fired{Name: pruneScheduleName})
	ctx.Bus.Close()

	if n := countByTopic(t, db, oldTopic); n != 0 {
		t.Fatalf("old row survived prune, got %d rows", n)
	}
	if n := countByTopic(t, db, freshTopic); n != 1 {
		t.Fatalf("fresh row was pruned, got %d rows", n)
	}
}

// TestSchedulerFiredSinkPrunesAndDedups exercises the split path: a POST to
// /events/scheduler-fired prunes; a second POST with the same X-Event-Id is an
// inbox-deduped no-op (so a freshly-aged row inserted between the two survives).
func TestSchedulerFiredSinkPrunesAndDedups(t *testing.T) {
	db := testDB(t)
	defer func() { _ = db.Close() }()

	m, _ := initModule(t, db)
	eventID := "test-evt-" + uniqueRun(t, db)
	t.Cleanup(func() { _, _ = db.Exec(`DELETE FROM audit.inbox WHERE event_id = $1`, eventID) })

	firstOld := uniqueTopic(t, db)
	insertAgedRow(t, db, firstOld, 60)

	post := func() *httptest.ResponseRecorder {
		w := httptest.NewRecorder()
		r := httptest.NewRequest(http.MethodPost, "/events/scheduler-fired",
			strings.NewReader(`{"Name":"`+pruneScheduleName+`"}`))
		r.Header.Set("X-Event-Id", eventID)
		m.handleSchedulerFired(w, r)
		return w
	}

	if w := post(); w.Code != http.StatusOK {
		t.Fatalf("first POST status = %d, want 200", w.Code)
	}
	if n := countByTopic(t, db, firstOld); n != 0 {
		t.Fatalf("first sink POST did not prune, got %d rows", n)
	}

	// Age a second row, then replay the SAME event id — inbox dedup means prune
	// must NOT run again, so the new old row survives.
	secondOld := uniqueTopic(t, db)
	insertAgedRow(t, db, secondOld, 60)
	if w := post(); w.Code != http.StatusOK {
		t.Fatalf("replay POST status = %d, want 200", w.Code)
	}
	if n := countByTopic(t, db, secondOld); n != 1 {
		t.Fatalf("replay (same X-Event-Id) re-ran prune — dedup failed, got %d rows", n)
	}
}

// TestDomainTopicsMatchEvents is the anti-drift guard: the audit topic list must
// equal exactly the domain events' declared topics. It imports the DOMAIN events
// packages (NOT schedulerevents — that is consumed via bus.On, not logged) and
// diffs the sets, so a topic rename on either side fails the build (topiccheck
// cannot see generic Subscribe).
func TestDomainTopicsMatchEvents(t *testing.T) {
	want := map[string]bool{
		accountsevents.PlayerRegisteredEvent.Topic(): true,
		charactersevents.CreatedEvent.Topic():        true,
		charactersevents.DeletedEvent.Topic():        true,
		configevents.ChangedEvent.Topic():            true,
		matchevents.FinishedEvent.Topic():            true,
	}
	got := map[string]bool{}
	for _, tp := range domainTopics {
		if got[tp] {
			t.Fatalf("duplicate topic %q in domainTopics", tp)
		}
		got[tp] = true
	}
	for tp := range want {
		if !got[tp] {
			t.Errorf("domainTopics missing declared event topic %q", tp)
		}
	}
	for tp := range got {
		if !want[tp] {
			t.Errorf("domainTopics has %q with no matching domain event (rename? stray topic?)", tp)
		}
	}
}

// insertAgedRow inserts a log row aged ageDays in the past under a marker topic.
func insertAgedRow(t *testing.T, db *sql.DB, topic string, ageDays int) {
	t.Helper()
	if _, err := db.Exec(
		`INSERT INTO audit.log (topic, payload, at) VALUES ($1, '{}'::jsonb, now() - make_interval(days => $2))`,
		topic, ageDays); err != nil {
		t.Fatalf("insert aged row: %v", err)
	}
}

// uniqueRun returns a run-unique hex string (for payload markers / event ids).
func uniqueRun(t *testing.T, db *sql.DB) string {
	t.Helper()
	var s string
	if err := db.QueryRow(`SELECT replace(gen_random_uuid()::text,'-','')`).Scan(&s); err != nil {
		t.Fatal(err)
	}
	return s
}
