package inventory

import (
	"context"
	"database/sql"
	"io"
	"log/slog"
	"os"
	"strconv"
	"sync"
	"testing"
	"time"

	_ "github.com/jackc/pgx/v5/stdlib"

	"gamebackend/bus"
	"gamebackend/modules/characters/charactersevents"
	"gamebackend/modules/config/configevents"
)

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

func newUUID(t *testing.T, db *sql.DB) string {
	t.Helper()
	var s string
	if err := db.QueryRow(`SELECT gen_random_uuid()::text`).Scan(&s); err != nil {
		t.Fatal(err)
	}
	return s
}

func qtyOf(holdings []Holding, itemID string) int {
	for _, h := range holdings {
		if h.ItemID == itemID {
			return h.Quantity
		}
	}
	return 0
}

func TestInventoryGrantStacks(t *testing.T) {
	db := testDB(t)
	defer func() { _ = db.Close() }()
	s := &store{db: db, log: slog.New(slog.NewTextHandler(io.Discard, nil))}
	ctx := context.Background()

	owner := Owner{Type: "player", ID: newUUID(t, db)}
	if err := s.grant(ctx, owner, "coin", 5); err != nil {
		t.Fatal(err)
	}
	if err := s.grant(ctx, owner, "coin", 3); err != nil {
		t.Fatal(err)
	}
	list, err := s.list(ctx, owner)
	if err != nil {
		t.Fatal(err)
	}
	if got := qtyOf(list, "coin"); got != 8 {
		t.Fatalf("coin quantity = %d, want 8 (grants should stack)", got)
	}
}

// TestInventoryReactsToCharacterLifecycle is the modularity payoff: inventory
// grants a starter item on character.created and wipes holdings on
// character.deleted, driven only by the event handlers — characters never calls
// inventory and there is no cross-module foreign key.
func TestInventoryReactsToCharacterLifecycle(t *testing.T) {
	db := testDB(t)
	defer func() { _ = db.Close() }()
	s := &store{db: db, log: slog.New(slog.NewTextHandler(io.Discard, nil))}
	m := &Module{store: s, log: slog.New(slog.NewTextHandler(io.Discard, nil)), cfg: &fakeConfig{}}
	ctx := context.Background()

	charID := newUUID(t, db)
	owner := Owner{Type: "character", ID: charID}

	m.onCharacterCreated(charactersevents.Created{CharacterID: charID, Name: "Test", Class: "novice"})
	list, err := s.list(ctx, owner)
	if err != nil {
		t.Fatal(err)
	}
	if qtyOf(list, starterItem) != 1 {
		t.Fatalf("after create: starter item not granted; holdings=%+v", list)
	}

	m.onCharacterDeleted(charactersevents.Deleted{CharacterID: charID})
	list, err = s.list(ctx, owner)
	if err != nil {
		t.Fatal(err)
	}
	if len(list) != 0 {
		t.Fatalf("after delete: holdings not cleaned up; got %+v", list)
	}
}

// fakeConfig is a contract-only stand-in for the "config" service: it satisfies
// inventory's configReader (GetString/GetInt) against an in-memory map. Using a
// fake keeps this test inside inventory's architectural boundary — it depends only
// on the configevents CONTRACT and never imports the config module implementation
// (go-arch-lint forbids module-impl → module-impl imports, tests included). The
// other half of the chain (Set -> pg_notify -> listener -> config.changed) is
// covered by config's own test; here we prove inventory's half via the real bus.
type fakeConfig struct {
	mu   sync.RWMutex
	vals map[string]string // "namespace:key" -> value
}

func (f *fakeConfig) set(ns, key, val string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.vals == nil {
		f.vals = map[string]string{}
	}
	f.vals[ns+":"+key] = val
}

func (f *fakeConfig) GetString(ns, key, def string) string {
	f.mu.RLock()
	defer f.mu.RUnlock()
	if v, ok := f.vals[ns+":"+key]; ok {
		return v
	}
	return def
}

func (f *fakeConfig) GetInt(ns, key string, def int) int {
	f.mu.RLock()
	defer f.mu.RUnlock()
	if v, ok := f.vals[ns+":"+key]; ok {
		if n, err := strconv.Atoi(v); err == nil {
			return n
		}
	}
	return def
}

// TestInventoryStarterLiveReloadFromConfig is the PUSH/materialized-consumer
// payoff for inventory's half of the chain: a config.changed event on the real bus
// drives inventory.onConfigChanged -> starter-spec rebuild -> the next grant uses
// the new item. The materialized spec never re-pulls on its own, so ONLY the
// load-bearing bus subscription can flip the granted item — which is exactly what
// this asserts. (Set -> pg_notify -> listener -> config.changed is tested in
// config's own package; joined by the shared configevents contract, the two halves
// cover the full live-reload path without inventory importing the config impl.)
func TestInventoryStarterLiveReloadFromConfig(t *testing.T) {
	db := testDB(t)
	defer func() { _ = db.Close() }()

	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	b := bus.NewBus(log)
	defer b.Close()

	fake := &fakeConfig{}
	s := &store{db: db, log: log}
	m := &Module{store: s, log: log, cfg: fake}
	bus.On(b, configevents.ChangedEvent, m.onConfigChanged)

	// Materialize the spec to the constant default BEFORE the edit. Once loaded,
	// starterSpec never re-pulls, so from here only an onConfigChanged event can
	// change the granted item — this is what makes the flip prove the PUSH path.
	if item, qty := m.starterSpec(); item != starterItem || qty != starterQty {
		t.Fatalf("pre-edit spec = (%s,%d), want (%s,%d)", item, qty, starterItem, starterQty)
	}

	// A wrong-namespace event must NOT touch inventory's spec (proves the filter).
	bus.Emit(b, configevents.ChangedEvent, configevents.Changed{Namespace: "other", Key: "starter_item", Value: "coin"})

	// The operator edits config; the push arrives on the bus.
	fake.set("inventory", "starter_item", "health_potion")
	bus.Emit(b, configevents.ChangedEvent, configevents.Changed{Namespace: "inventory", Key: "starter_item", Value: "health_potion"})

	// The bus delivers asynchronously — wait until onConfigChanged has rebuilt the
	// spec, then assert a fresh grant uses the new item (and only the new item).
	deadline := time.Now().Add(2 * time.Second)
	for {
		if item, _ := m.starterSpec(); item == "health_potion" {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("starter spec did not live-reload to health_potion via config.changed")
		}
		time.Sleep(10 * time.Millisecond)
	}

	charID := newUUID(t, db)
	owner := Owner{Type: "character", ID: charID}
	m.onCharacterCreated(charactersevents.Created{CharacterID: charID, Name: "Reload", Class: "novice"})
	list, err := s.list(context.Background(), owner)
	if err != nil {
		t.Fatal(err)
	}
	if qtyOf(list, "health_potion") != 1 {
		t.Fatalf("after reload: new starter not granted; holdings=%+v", list)
	}
	if qtyOf(list, starterItem) != 0 {
		t.Fatalf("after reload: old starter still granted; holdings=%+v", list)
	}
}
