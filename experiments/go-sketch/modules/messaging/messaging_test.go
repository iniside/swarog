package messaging

import (
	"context"
	"database/sql"
	"io"
	"log/slog"
	"os"
	"sync/atomic"
	"testing"
	"time"

	_ "github.com/jackc/pgx/v5/stdlib"

	"gamebackend/bus"
	"gamebackend/lifecycle"
	"gamebackend/registry"
)

func discardLog() *slog.Logger { return slog.New(slog.NewTextHandler(io.Discard, nil)) }

// testDB opens the local Postgres (per the repo convention), migrates the
// messaging schema, and skips gracefully when it's unreachable.
func testDB(t *testing.T) *sql.DB {
	t.Helper()
	dsn := os.Getenv("DATABASE_URL")
	if dsn == "" {
		dsn = defaultDSN
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

// freshEventID returns a unique event id so each test's inbox rows never collide
// with a previous run's (the ledger is shared across runs until housekeeping).
func freshEventID(t *testing.T, db *sql.DB) string {
	t.Helper()
	var s string
	if err := db.QueryRow(`SELECT 'messaging:test:' || gen_random_uuid()::text`).Scan(&s); err != nil {
		t.Fatal(err)
	}
	return s
}

// TestRegisterInstallsTransportBeforeInit proves the [R3] boot ordering: Register
// (phase 1) allocates localHandlers AND installs the transport, so a consumer that
// SubscribeTx's during its phase-2 Init — BEFORE messaging.Init — does not
// nil-map-panic and the subscription is recorded.
func TestRegisterInstallsTransportBeforeInit(t *testing.T) {
	m := &Module{}
	lctx := lifecycle.NewContext(discardLog())

	if err := m.Register(lctx); err != nil {
		t.Fatalf("Register: %v", err)
	}

	// A consumer's Init calls bus.OnTx -> Transport.SubscribeTx. This runs before
	// messaging.Init; it must not panic on a nil map.
	et := bus.Define[struct {
		X int `json:"x"`
	}]("test.topic")
	bus.OnTx(lctx.Bus, et, "consumer", func(context.Context, *sql.Tx, struct {
		X int `json:"x"`
	}) error {
		return nil
	})

	m.mu.Lock()
	got := len(m.localHandlers["test.topic"])
	m.mu.Unlock()
	if got != 1 {
		t.Fatalf("SubscribeTx before Init recorded %d handlers, want 1", got)
	}

	// The marker is Provided under "messaging" for validateRequires's boot check.
	if _, ok := registry.TryRequire[Service](lctx.Registry, "messaging"); !ok {
		t.Fatal("messaging marker not Provided under \"messaging\"")
	}
}

// TestConsumeDedup proves per-subscriber exactly-once: delivering the SAME
// (event_id, subscriber) twice runs the handler exactly once.
func TestConsumeDedup(t *testing.T) {
	db := testDB(t)
	defer func() { _ = db.Close() }()
	m := &Module{db: db, log: discardLog(), localHandlers: map[string][]localSub{}}
	ctx := context.Background()
	eventID := freshEventID(t, db)

	var calls atomic.Int32
	h := func(context.Context, *sql.Tx, []byte) error {
		calls.Add(1)
		return nil
	}

	for range 2 {
		if err := m.consume(ctx, "sub-a", eventID, []byte(`{}`), h); err != nil {
			t.Fatalf("consume: %v", err)
		}
	}
	if got := calls.Load(); got != 1 {
		t.Fatalf("handler ran %d times, want 1 (inbox dedup)", got)
	}
}

// TestConsumeFateIsolation proves [R6]: two subscribers of the same event, one
// whose handler errors — the other's effect + inbox row still commit, because each
// subscriber gets its OWN tx and its OWN (event_id, subscriber) inbox row.
func TestConsumeFateIsolation(t *testing.T) {
	db := testDB(t)
	defer func() { _ = db.Close() }()
	m := &Module{db: db, log: discardLog(), localHandlers: map[string][]localSub{}}
	ctx := context.Background()
	eventID := freshEventID(t, db)

	// Subscriber "good" writes a marker into a temp table inside its tx.
	if _, err := db.Exec(`CREATE TEMP TABLE IF NOT EXISTS fate_marker (who text primary key)`); err != nil {
		t.Fatalf("temp table: %v", err)
	}
	good := func(ctx context.Context, tx *sql.Tx, _ []byte) error {
		_, err := tx.ExecContext(ctx, `INSERT INTO fate_marker (who) VALUES ('good') ON CONFLICT DO NOTHING`)
		return err
	}
	bad := func(context.Context, *sql.Tx, []byte) error {
		return io.ErrUnexpectedEOF // any error
	}

	if err := m.consume(ctx, "good", eventID, []byte(`{}`), good); err != nil {
		t.Fatalf("good consume unexpectedly failed: %v", err)
	}
	if err := m.consume(ctx, "bad", eventID, []byte(`{}`), bad); err == nil {
		t.Fatal("bad consume should have returned the handler error")
	}

	// good's effect committed (marker present) and its inbox row is present.
	var markerN int
	if err := db.QueryRow(`SELECT count(*) FROM fate_marker WHERE who='good'`).Scan(&markerN); err != nil {
		t.Fatal(err)
	}
	if markerN != 1 {
		t.Fatalf("good effect not committed; marker count=%d", markerN)
	}
	var goodInbox, badInbox int
	if err := db.QueryRow(`SELECT count(*) FROM messaging.inbox WHERE event_id=$1 AND subscriber='good'`, eventID).Scan(&goodInbox); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow(`SELECT count(*) FROM messaging.inbox WHERE event_id=$1 AND subscriber='bad'`, eventID).Scan(&badInbox); err != nil {
		t.Fatal(err)
	}
	if goodInbox != 1 {
		t.Fatalf("good inbox row missing (count=%d) — a failing subscriber rolled back a healthy one", goodInbox)
	}
	if badInbox != 0 {
		t.Fatalf("bad inbox row present (count=%d) — a failed handler must roll back its own dedup marker so it redelivers", badInbox)
	}
}

// TestEnqueueTxStampsOrigin proves EnqueueTx writes the row on the caller's tx with
// the module's origin and does not commit it (the caller owns commit/rollback).
func TestEnqueueTxStampsOrigin(t *testing.T) {
	db := testDB(t)
	defer func() { _ = db.Close() }()
	m := &Module{db: db, log: discardLog(), origin: "test-origin", localHandlers: map[string][]localSub{}}
	ctx := context.Background()

	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		t.Fatal(err)
	}
	if err := m.EnqueueTx(tx, "test.enqueue", []byte(`{"a":1}`)); err != nil {
		t.Fatalf("EnqueueTx: %v", err)
	}
	var id int64
	if err := tx.QueryRowContext(ctx,
		`SELECT id FROM messaging.outbox WHERE origin='test-origin' AND topic='test.enqueue' ORDER BY id DESC LIMIT 1`).Scan(&id); err != nil {
		t.Fatalf("row not visible inside tx: %v", err)
	}
	// Roll back — nothing should persist, proving EnqueueTx did not commit.
	if err := tx.Rollback(); err != nil {
		t.Fatal(err)
	}
	var n int
	if err := db.QueryRow(`SELECT count(*) FROM messaging.outbox WHERE id=$1`, id).Scan(&n); err != nil {
		t.Fatal(err)
	}
	if n != 0 {
		t.Fatalf("row survived rollback (count=%d) — EnqueueTx must not commit the caller's tx", n)
	}
}
