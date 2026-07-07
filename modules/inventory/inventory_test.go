package inventory

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
	"gamebackend/lifecycle"
	"gamebackend/modules/characters/charactersevents"
	"gamebackend/modules/config"
	"gamebackend/modules/config/configevents"
	"gamebackend/registry"
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
	m := &Module{store: s, log: slog.New(slog.NewTextHandler(io.Discard, nil))}
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

// configSvc is the test's view of the "config" service: the reader subset
// inventory depends on plus Set (to drive an edit). A value of this interface is
// assignable to the narrower configReader field on Module.
type configSvc interface {
	GetString(namespace, key, def string) string
	GetInt(namespace, key string, def int) int
	Set(ctx context.Context, namespace, key, value string) error
}

// TestInventoryStarterLiveReloadFromConfig is the PUSH/materialized-consumer
// payoff: editing inventory/starter_item in config flows the FULL chain
// Set -> pg_notify -> config listener -> config.changed (bus) ->
// inventory.onConfigChanged -> starter-spec rebuild -> next grant uses the new
// item. Nothing is called directly on the inventory side except the real bus
// subscription, so the load-bearing subscription is what makes the edit land.
func TestInventoryStarterLiveReloadFromConfig(t *testing.T) {
	db := testDB(t)
	// Defers run LIFO: close registered first (runs last), the namespace cleanup
	// registered second (runs first, while the pool is still open). The reactive
	// key MUST be inventory/starter_item (that is what onConfigChanged filters on),
	// so the namespace can't be randomized — clean it up explicitly.
	defer func() { _ = db.Close() }()
	defer func() {
		if _, err := db.Exec(`DELETE FROM config.settings WHERE namespace = 'inventory'`); err != nil {
			t.Logf("cleanup of config namespace inventory failed: %v", err)
		}
	}()

	log := slog.New(slog.NewTextHandler(io.Discard, nil))

	// Wire the REAL config module through its lifecycle (Register provides the
	// service, Init sets db/bus/dsn, Start launches the LISTEN/NOTIFY loop) on a
	// Context whose Bus inventory also subscribes to — the same in-process bus the
	// listener emits config.changed on.
	ctx := lifecycle.NewContext(log)
	ctx.DB = db
	cfgMod := &config.Module{}
	if err := cfgMod.Register(ctx); err != nil {
		t.Fatalf("config Register: %v", err)
	}
	if err := cfgMod.Migrate(context.Background(), db); err != nil {
		t.Fatalf("config Migrate: %v", err)
	}
	if err := cfgMod.Init(ctx); err != nil {
		t.Fatalf("config Init: %v", err)
	}
	if err := cfgMod.Start(context.Background()); err != nil {
		t.Fatalf("config Start: %v", err)
	}
	defer func() {
		stopCtx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		_ = cfgMod.Stop(stopCtx)
	}()

	cfg, ok := registry.TryRequire[configSvc](ctx.Registry, "config")
	if !ok {
		t.Fatal("config service not registered / wrong shape")
	}

	// Construct inventory the way Init would for the config path: hold cfg and
	// subscribe onConfigChanged on the shared bus (the ONLY starter-spec refresh
	// path). accounts/characters are omitted — this test exercises only the grant.
	s := &store{db: db, log: log}
	m := &Module{store: s, log: log, cfg: cfg}
	bus.On(ctx.Bus, configevents.ChangedEvent, m.onConfigChanged)

	// Pre-materialize the spec to the constant default BEFORE any edit. Once loaded
	// starterSpec never re-pulls from config, so from here ONLY an onConfigChanged
	// event can change the granted item — a silent loadAll cache refresh cannot.
	// This is what makes the assertion below prove the PUSH path, not a stray pull.
	if item, qty := m.starterSpec(); item != starterItem || qty != starterQty {
		t.Fatalf("pre-edit spec = (%s,%d), want (%s,%d)", item, qty, starterItem, starterQty)
	}

	// Poll the observable end effect: grant a brand-new character and assert its
	// starter flips to health_potion once the push chain has propagated. Each
	// iteration re-issues Set (a harmless idempotent upsert that re-fires
	// pg_notify) so a NOTIFY lost to a Set/LISTEN startup race is always recovered
	// by a later one — the flip then can only come via config.changed.
	deadline := time.Now().Add(5 * time.Second)
	for {
		if err := cfg.Set(context.Background(), "inventory", "starter_item", "health_potion"); err != nil {
			t.Fatalf("Set starter_item: %v", err)
		}
		charID := newUUID(t, db)
		owner := Owner{Type: "character", ID: charID}
		m.onCharacterCreated(charactersevents.Created{CharacterID: charID, Name: "Reload", Class: "novice"})
		list, err := s.list(context.Background(), owner)
		if err != nil {
			t.Fatal(err)
		}
		if qtyOf(list, "health_potion") == 1 {
			if qtyOf(list, starterItem) != 0 {
				t.Fatalf("granted both old and new starter; holdings=%+v", list)
			}
			return // live reload observed end-to-end via config.changed
		}
		if time.Now().After(deadline) {
			t.Fatalf("starter item did not live-reload to health_potion within deadline; last holdings=%+v", list)
		}
		time.Sleep(20 * time.Millisecond)
	}
}
