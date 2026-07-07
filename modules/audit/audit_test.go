package audit

import (
	"context"
	"database/sql"
	"encoding/json"
	"io"
	"log/slog"
	"os"
	"testing"
	"time"

	_ "github.com/jackc/pgx/v5/stdlib"

	"gamebackend/api/accounts/accountsevents"
	"gamebackend/api/characters/charactersevents"
	"gamebackend/api/config/configevents"
	"gamebackend/api/match/matchevents"
	"gamebackend/api/scheduler/schedulerevents"
	"gamebackend/bus"
	"gamebackend/lifecycle"
)

func discardLog() *slog.Logger { return slog.New(slog.NewTextHandler(io.Discard, nil)) }

// fakeTransport is a minimal in-memory bus.Transport standing in for the
// messaging module: it captures the durable handlers audit registers via
// bus.OnTxRaw / bus.OnTx (SubscribeTx) and lets a test drive delivery by invoking
// the handler inside a REAL *sql.Tx — mirroring messaging's per-subscriber
// inbox-dedup consume tx (minus the dedup, which is messaging's concern and
// covered by messaging's own tests). This keeps audit's unit tests inside its
// architectural boundary: they exercise the OnTx/OnTxRaw handler + ledger
// atomicity, not the transport internals. EnqueueTx is unused here (audit is a
// pure consumer).
type fakeTransport struct {
	db   *sql.DB
	subs map[string]func(context.Context, *sql.Tx, []byte) error
}

func (f *fakeTransport) EnqueueTx(*sql.Tx, string, []byte) error { return nil }

func (f *fakeTransport) SubscribeTx(topic, _ string, h func(context.Context, *sql.Tx, []byte) error) {
	if f.subs == nil {
		f.subs = map[string]func(context.Context, *sql.Tx, []byte) error{}
	}
	f.subs[topic] = h
}

// deliver runs the durable handler for topic inside a real tx and commits — the
// same shape as messaging's consume, so the ledger insert / prune commits
// atomically with the (would-be) inbox row.
func (f *fakeTransport) deliver(t *testing.T, topic string, v any) {
	t.Helper()
	h, ok := f.subs[topic]
	if !ok {
		t.Fatalf("no durable subscriber registered for topic %q", topic)
	}
	payload, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	tx, err := f.db.BeginTx(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	if err := h(context.Background(), tx, payload); err != nil {
		_ = tx.Rollback()
		t.Fatalf("durable handler for %q: %v", topic, err)
	}
	if err := tx.Commit(); err != nil {
		t.Fatal(err)
	}
}

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
	// Close via Cleanup, NOT the caller's defer: t.Cleanup runs LIFO, so this
	// (registered first) runs LAST — after the per-test DELETE cleanups, which
	// therefore still see an OPEN db. A plain defer would close first and the
	// error-ignored DELETEs would silently leak rows on the shared Postgres.
	t.Cleanup(func() { _ = db.Close() })
	return db
}

// initModule wires an audit Module against db via a real lifecycle.Context with a
// fakeTransport installed BEFORE Init, so the durable subscriptions (bus.OnTxRaw
// for domain topics, bus.OnTx for scheduler.fired) and the best-effort
// ctx.Bus.Subscribe handlers are registered exactly as in production. Returns the
// module, the context (its Bus drives best-effort events) and the transport (drive
// durable delivery via ft.deliver).
func initModule(t *testing.T, db *sql.DB) (*Module, *lifecycle.Context, *fakeTransport) {
	t.Helper()
	ctx := lifecycle.NewContext(discardLog())
	ctx.DB = db
	ft := &fakeTransport{db: db}
	ctx.Bus.SetTransport(ft)
	m := &Module{}
	if err := m.Init(ctx); err != nil {
		t.Fatalf("init: %v", err)
	}
	return m, ctx, ft
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

// TestBestEffortEventIsLogged emits a real typed best-effort event (match.finished)
// and asserts the in-process ctx.Bus.Subscribe handler writes a row with the right
// topic and a JSON payload.
func TestBestEffortEventIsLogged(t *testing.T) {
	db := testDB(t) // closed via testDB's t.Cleanup, after per-test DELETE cleanups

	_, ctx, _ := initModule(t, db)
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
}

// TestDurableCharacterEventsAreLogged drives the durable plane: a character.created
// and a character.deleted delivered through the fakeTransport (as messaging would)
// are recorded to the ledger verbatim, on the handed tx — no producer *events
// import needed by audit (OnTxRaw hands raw JSON).
func TestDurableCharacterEventsAreLogged(t *testing.T) {
	db := testDB(t) // closed via testDB's t.Cleanup, after per-test DELETE cleanups

	_, _, ft := initModule(t, db)
	charID := uniqueRun(t, db)
	t.Cleanup(func() { _, _ = db.Exec(`DELETE FROM audit.log WHERE payload->>'CharacterID' = $1`, charID) })

	ft.deliver(t, charactersevents.CreatedEvent.Topic(),
		charactersevents.Created{CharacterID: charID, Name: "Test", Class: "novice"})
	ft.deliver(t, charactersevents.DeletedEvent.Topic(),
		charactersevents.Deleted{CharacterID: charID})

	var created, deleted int
	if err := db.QueryRow(
		`SELECT
			count(*) FILTER (WHERE topic = $2),
			count(*) FILTER (WHERE topic = $3)
		 FROM audit.log WHERE payload->>'CharacterID' = $1`,
		charID, charactersevents.CreatedEvent.Topic(), charactersevents.DeletedEvent.Topic()).
		Scan(&created, &deleted); err != nil {
		t.Fatalf("count character rows: %v", err)
	}
	if created != 1 || deleted != 1 {
		t.Fatalf("durable character events not recorded: created=%d deleted=%d (want 1,1)", created, deleted)
	}
}

// TestPruneViaDurable reacts to scheduler.fired{audit-prune} on the durable plane
// (bus.OnTx, driven via the fakeTransport) and deletes rows past the retention
// window, keeping fresh ones. A non-prune schedule name is a no-op.
func TestPruneViaDurable(t *testing.T) {
	db := testDB(t) // closed via testDB's t.Cleanup, after per-test DELETE cleanups

	_, _, ft := initModule(t, db)
	oldTopic := uniqueTopic(t, db)
	freshTopic := uniqueTopic(t, db)
	insertAgedRow(t, db, oldTopic, 60)  // 60 days old — past default 30d retention
	insertAgedRow(t, db, freshTopic, 0) // now — safe

	// A non-prune schedule name must NOT prune (proves the Name filter).
	ft.deliver(t, schedulerevents.FiredEvent.Topic(), schedulerevents.Fired{Name: "some-other-job"})
	if n := countByTopic(t, db, oldTopic); n != 1 {
		t.Fatalf("non-prune schedule name pruned rows, got %d old rows", n)
	}

	ft.deliver(t, schedulerevents.FiredEvent.Topic(), schedulerevents.Fired{Name: pruneScheduleName})
	if n := countByTopic(t, db, oldTopic); n != 0 {
		t.Fatalf("old row survived prune, got %d rows", n)
	}
	if n := countByTopic(t, db, freshTopic); n != 1 {
		t.Fatalf("fresh row was pruned, got %d rows", n)
	}
}

// assertTopicSet is the anti-drift diff: got must equal want exactly, with no
// duplicates. Shared by the durable and best-effort set guards.
func assertTopicSet(t *testing.T, name string, list []string, want map[string]bool) {
	t.Helper()
	got := map[string]bool{}
	for _, tp := range list {
		if got[tp] {
			t.Fatalf("duplicate topic %q in %s", tp, name)
		}
		got[tp] = true
	}
	for tp := range want {
		if !got[tp] {
			t.Errorf("%s missing declared event topic %q", name, tp)
		}
	}
	for tp := range got {
		if !want[tp] {
			t.Errorf("%s has %q with no matching domain event (rename? stray topic?)", name, tp)
		}
	}
}

// TestDurableTopicsMatchEvents is the anti-drift guard for the durable set: it must
// equal exactly the durable producers' declared topics (character create/delete).
// It imports the domain events packages and diffs the sets, so a topic rename on
// either side fails the build.
func TestDurableTopicsMatchEvents(t *testing.T) {
	assertTopicSet(t, "durableTopics", durableTopics, map[string]bool{
		charactersevents.CreatedEvent.Topic(): true,
		charactersevents.DeletedEvent.Topic(): true,
	})
}

// TestBestEffortTopicsMatchEvents is the anti-drift guard for the best-effort set:
// it must equal exactly the best-effort producers' declared topics (player
// registered, config changed, match finished) — the producers that emit plain
// bus.Emit with no outbox.
func TestBestEffortTopicsMatchEvents(t *testing.T) {
	assertTopicSet(t, "bestEffortTopics", bestEffortTopics, map[string]bool{
		accountsevents.PlayerRegisteredEvent.Topic(): true,
		configevents.ChangedEvent.Topic():            true,
		matchevents.FinishedEvent.Topic():            true,
	})
}

// TestTopicSetsAreDisjoint guards that no topic is both durable and best-effort —
// a topic must be logged through exactly one plane.
func TestTopicSetsAreDisjoint(t *testing.T) {
	durable := map[string]bool{}
	for _, tp := range durableTopics {
		durable[tp] = true
	}
	for _, tp := range bestEffortTopics {
		if durable[tp] {
			t.Errorf("topic %q is in BOTH durableTopics and bestEffortTopics", tp)
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
