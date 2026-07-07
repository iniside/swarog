package config

import (
	"context"
	"database/sql"
	"io"
	"log/slog"
	"os"
	"testing"
	"time"

	_ "github.com/jackc/pgx/v5/stdlib"

	"gamebackend/bus"
)

func discardLog() *slog.Logger {
	return slog.New(slog.NewTextHandler(io.Discard, nil))
}

func testDB(t *testing.T) *sql.DB {
	t.Helper()
	db, err := sql.Open("pgx", dsn())
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

func dsn() string {
	if v := os.Getenv("DATABASE_URL"); v != "" {
		return v
	}
	return defaultDSN
}

// newNS returns a fresh, unique namespace that is a VALID identifier (the UUID's
// hyphens are stripped, so it matches ^[a-z0-9_]+$) and registers cleanup of its
// rows so tests never pollute the shared config.settings table.
func newNS(t *testing.T, db *sql.DB) string {
	t.Helper()
	var ns string
	if err := db.QueryRow(`SELECT 'test_' || replace(gen_random_uuid()::text, '-', '')`).Scan(&ns); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if _, err := db.Exec(`DELETE FROM config.settings WHERE namespace = $1`, ns); err != nil {
			t.Logf("cleanup of namespace %q failed: %v", ns, err)
		}
	})
	return ns
}

// TestGetters is the PULL demonstration: typed getters over a preloaded cache,
// each hitting and falling back on a miss. No DB.
func TestGetters(t *testing.T) {
	svc := &service{cache: map[cacheKey]string{
		{"game", "name"}:        "arena",
		{"game", "max_players"}: "8",
		{"game", "hardcore"}:    "true",
	}}

	if got := svc.GetString("game", "name", "def"); got != "arena" {
		t.Errorf("GetString hit = %q, want arena", got)
	}
	if got := svc.GetString("game", "missing", "def"); got != "def" {
		t.Errorf("GetString miss = %q, want def", got)
	}
	if !svc.GetBool("game", "hardcore", false) {
		t.Error("GetBool hit = false, want true")
	}
	if svc.GetBool("game", "missing", false) {
		t.Error("GetBool miss = true, want false (def)")
	}
	if got := svc.GetInt("game", "max_players", 1); got != 8 {
		t.Errorf("GetInt hit = %d, want 8", got)
	}
	if got := svc.GetInt("game", "missing", 3); got != 3 {
		t.Errorf("GetInt miss = %d, want 3 (def)", got)
	}
	if got := svc.GetInt("game", "name", 5); got != 5 {
		t.Errorf("GetInt parse-fail = %d, want 5 (def)", got)
	}
}

// TestSetRejectsInvalidIdentifiers verifies Set validates ids BEFORE any DB work
// (so it needs no DB).
func TestSetRejectsInvalidIdentifiers(t *testing.T) {
	svc := &service{cache: map[cacheKey]string{}}
	ctx := context.Background()

	if err := svc.Set(ctx, "bad ns", "k", "v"); err == nil {
		t.Error("expected error for namespace with a space")
	}
	if err := svc.Set(ctx, "UPPER", "k", "v"); err == nil {
		t.Error("expected error for uppercase namespace")
	}
	if err := svc.Set(ctx, "ok", "Bad Key", "v"); err == nil {
		t.Error("expected error for invalid key")
	}
}

// TestSetLoad is the DB round-trip: Set persists, a fresh loadAll + replaceCache
// makes the value readable.
func TestSetLoad(t *testing.T) {
	db := testDB(t)
	defer func() { _ = db.Close() }()

	svc := &service{db: db, log: discardLog(), cache: map[cacheKey]string{}}
	ns := newNS(t, db)
	ctx := context.Background()

	if err := svc.Set(ctx, ns, "limit", "42"); err != nil {
		t.Fatalf("Set: %v", err)
	}
	all, err := svc.loadAll(ctx)
	if err != nil {
		t.Fatalf("loadAll: %v", err)
	}
	svc.replaceCache(all)

	if v, ok := svc.Get(ns, "limit"); !ok || v != "42" {
		t.Fatalf("Get(%s,limit) = %q,%v; want 42,true", ns, v, ok)
	}
	if got := svc.GetInt(ns, "limit", 0); got != 42 {
		t.Fatalf("GetInt = %d, want 42", got)
	}
}

// TestLiveReload exercises the REAL push path end-to-end: Set -> pg_notify ->
// listener -> cache refresh, observed by polling Get (never by calling the
// refresh directly).
func TestLiveReload(t *testing.T) {
	db := testDB(t)
	defer func() { _ = db.Close() }()

	log := discardLog()
	m := &Module{
		db:  db,
		log: log,
		bus: bus.NewBus(log),
		svc: &service{db: db, log: log, cache: map[cacheKey]string{}},
		dsn: dsn(),
	}
	ns := newNS(t, db)

	if err := m.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}
	defer func() {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		_ = m.Stop(ctx)
	}()

	if err := m.svc.Set(context.Background(), ns, "flag", "on"); err != nil {
		t.Fatalf("Set: %v", err)
	}

	deadline := time.Now().Add(2 * time.Second)
	for {
		if v, ok := m.svc.Get(ns, "flag"); ok && v == "on" {
			return // listener refreshed the cache
		}
		if time.Now().After(deadline) {
			t.Fatal("listener did not refresh cache within deadline")
		}
		time.Sleep(20 * time.Millisecond)
	}
}
