// Package inventory owns item holdings for any owner — a player (e.g. IAP) or a
// character. It depends on accounts (auth) and characters (ownership checks), and
// REACTS to character lifecycle events: granting a starter item on creation and
// wiping holdings on deletion. characters has no idea inventory exists.
package inventory

import (
	"context"
	"database/sql"
	"log/slog"
	"os"
	"strings"
	"sync"

	"gamebackend/api/admin/adminapi"
	"gamebackend/api/characters/charactersevents"
	"gamebackend/api/config/configevents"
	"gamebackend/api/inventory/inventoryapi"
	"gamebackend/api/inventory/inventoryrpc"
	"gamebackend/bus"
	"gamebackend/edge"
	"gamebackend/lifecycle"
	"gamebackend/opsapi"
	"gamebackend/registry"
)

// starterItem / starterQty are the per-key default starter spec, used when
// inventory/starter_item or inventory/starter_qty is absent from
// config.settings. config itself is a mandatory dependency (Requires), so
// there is no "config isn't hosted" case to fall back for.
const (
	starterItem = "starter_sword"
	starterQty  = 1
)

// configReader is the consumer-defined slice of the "config" service inventory
// needs: just the two getters used to resolve the starter spec. Depending on a
// capability (not the config package's concrete type) keeps the coupling to a
// pair of method signatures.
type configReader interface {
	GetString(namespace, key, def string) string
	GetInt(namespace, key string, def int) int
}

// charactersSvc is the consumer-defined slice we depend on for the ownership
// check in ListCharacter. It returns an error so a transport failure (characters
// hosted in a peer process, reached over the QUIC edge) surfaces as 503 rather
// than a false 404. The local, co-hosted implementation returns a nil error.
// Bearer verification is no longer inventory's concern — the gateway authenticates
// once at the front door and injects the player_id into ctx — so the accounts
// consumer dependency is gone.
type charactersSvc interface {
	OwnerOf(ctx context.Context, characterID string) (playerID string, ok bool, err error)
}

type Module struct {
	log   *slog.Logger
	store *store
	svc   *service

	// cfg is the mandatory "config" service, resolved via registry.Require in
	// Init. config is hosted in every binary that hosts inventory (a hard
	// dependency, declared in Requires()) — never nil once Init has run.
	cfg configReader

	// The starter spec is MATERIALIZED: resolved once (lazily under starterMu) and
	// rebuilt ONLY by onConfigChanged on a config.changed event — the push/live-
	// reload path. Grants read this cached spec rather than re-pulling from config.
	starterMu     sync.RWMutex
	starterName   string // resolved starter item id
	starterAmount int    // resolved starter quantity
	starterLoaded bool

	// Edge, when non-nil, is the process-wide QUIC RPC server (constructed and
	// started by main() only in a split that hosts this module). Init registers the
	// inventory player-operation handlers (listMine/listCharacter/grant) on it so a
	// front gateway in a peer can route player-facing inventory operations to this
	// peer. nil in the monolith — no edge exposure.
	Edge *edge.Server
}

func (*Module) Name() string       { return "inventory" }
func (*Module) Requires() []string { return []string{"characters", "config", "messaging"} }

const schemaDDL = `
CREATE SCHEMA IF NOT EXISTS inventory;

CREATE TABLE IF NOT EXISTS inventory.items (
	id   text PRIMARY KEY,
	name text NOT NULL,
	kind text NOT NULL
);
INSERT INTO inventory.items (id, name, kind) VALUES
	('coin','Coin','currency'),
	('starter_sword','Starter Sword','weapon'),
	('health_potion','Health Potion','consumable')
ON CONFLICT (id) DO NOTHING;

CREATE TABLE IF NOT EXISTS inventory.holdings (
	owner_type text NOT NULL,                 -- 'player' | 'character'
	owner_id   uuid NOT NULL,                 -- ref player/character id, no cross-module FK
	item_id    text NOT NULL REFERENCES inventory.items(id),
	quantity   int  NOT NULL CHECK (quantity >= 0),
	PRIMARY KEY (owner_type, owner_id, item_id)
);
CREATE INDEX IF NOT EXISTS holdings_owner_idx ON inventory.holdings(owner_type, owner_id);`

func (*Module) Migrate(_ context.Context, db *sql.DB) error {
	_, err := db.Exec(schemaDDL)
	return err
}

// Register constructs the store-backed service and offers it to other modules.
// It runs in Build's phase 1, before any Init, so a dependent's Require resolves
// regardless of registration order. It touches only ctx.DB (available now); the
// service closes over m.store alone, never a Required dependency.
func (m *Module) Register(ctx *lifecycle.Context) error {
	m.store = &store{db: ctx.DB, log: ctx.Log}
	m.svc = &service{store: m.store}
	registry.Provide(ctx.Registry, "inventory", m.svc)
	return nil
}

func (m *Module) Init(ctx *lifecycle.Context) error {
	m.log = ctx.Log
	// The characters ownership capability backs ListCharacter's authorization; it
	// resolves to the real service (monolith) or the generated edge client (split).
	// Wire it onto the service Provided in Register — the service, not the Module,
	// now owns the ownership check.
	m.svc.characters = registry.Require[charactersSvc](ctx.Registry, "characters")

	// React to character lifecycle — integrity without a cross-module FK. These
	// are DURABLE subscriptions on the messaging plane: the transport runs each
	// effect inside a per-(event_id,"inventory") inbox-dedup tx, in BOTH
	// topologies (monolith = local in-tx delivery, split = HTTP POST /events).
	// messaging owns the inbox, the dedup, and the HTTP receive — inventory just
	// declares the handler and runs the effect on the HANDED tx (never m.store.db)
	// so the grant/wipe commits atomically with the dedup row.
	bus.OnTx(ctx.Bus, charactersevents.CreatedEvent, "inventory", func(ctx context.Context, tx *sql.Tx, e charactersevents.Created) error {
		return m.grantStarter(ctx, tx, e.CharacterID)
	})
	bus.OnTx(ctx.Bus, charactersevents.DeletedEvent, "inventory", func(ctx context.Context, tx *sql.Tx, e charactersevents.Deleted) error {
		return m.wipeCharacter(ctx, tx, e.CharacterID)
	})

	// HARD dependency on config (declared in Requires()): every binary that
	// hosts inventory also hosts config, so this fails loud at boot (via
	// validateRequires) rather than silently degrading to the starter consts.
	m.cfg = registry.Require[configReader](ctx.Registry, "config")
	// The subscription is load-bearing: onConfigChanged is the ONLY path that
	// rebuilds the materialized starter spec, so editing inventory/starter_item
	// in /admin flows config.changed -> here -> the next grant uses the new item.
	bus.On(ctx.Bus, configevents.ChangedEvent, m.onConfigChanged)

	// Player operations: contribute each op's HTTP binding + in-process invoker so
	// the gateway fronts GET /inventory/me + GET /inventory/character/{id} (and,
	// dev-gated, POST /inventory/me/grant), authenticates once, and dispatches to
	// m.svc with the verified player_id in ctx. This REPLACES the deleted
	// handleMine/handleCharacter/handleGrant + their inline bearer auth.
	devGrant := envBool("INVENTORY_DEV_GRANT", true)
	if devGrant {
		ctx.Log.Warn("INVENTORY_DEV_GRANT is ON — POST /inventory/me/grant (simulated IAP) is enabled; turn OFF in production")
	}
	registerPlayerOps(ctx, m.svc, devGrant)

	// The inventory service was Provided in Register (phase 1); m.store is set.
	ctx.Contribute(adminapi.Slot, adminapi.Item{ID: adminItemID, Section: adminSectionName, Label: adminLabel, Render: m.adminSection})

	// Split topology: expose the inventory player capabilities over the shared QUIC
	// edge server so a front gateway in a peer can route player-facing inventory
	// operations to this peer. The edge face is rpcgen-generated from the pure
	// inventoryapi contract: RegisterServer installs identity-aware adapters (they
	// read the request envelope's Identity into ctx via opsapi.WithPlayerID).
	// Registering handlers is pure wiring (no I/O); main() starts the listener
	// after all Inits.
	if m.Edge != nil {
		var holdings inventoryapi.Holdings = m.svc
		inventoryrpc.RegisterServer(m.Edge, holdings)
		m.log.Info("edge handlers registered", "methods",
			[]string{inventoryrpc.MethodListMine, inventoryrpc.MethodListCharacter, inventoryrpc.MethodGrant})
	}
	return nil
}

// loadStarterLocked resolves the starter spec into the materialized fields. The
// caller MUST hold starterMu for writing. config is a mandatory dependency, so
// this always reads inventory/starter_item + inventory/starter_qty, falling
// back to the starterItem/starterQty consts only when a key is absent.
func (m *Module) loadStarterLocked() {
	m.starterName = m.cfg.GetString("inventory", "starter_item", starterItem)
	m.starterAmount = m.cfg.GetInt("inventory", "starter_qty", starterQty)
	m.starterLoaded = true
}

// starterSpec returns the materialized starter item + quantity. It lazily loads
// on first use under the mutex (order-independent — no reliance on config.Start
// running before inventory.Start), then serves from the cached fields until
// onConfigChanged rebuilds them.
func (m *Module) starterSpec() (item string, qty int) {
	m.starterMu.RLock()
	if m.starterLoaded {
		item, qty = m.starterName, m.starterAmount
		m.starterMu.RUnlock()
		return item, qty
	}
	m.starterMu.RUnlock()

	m.starterMu.Lock()
	defer m.starterMu.Unlock()
	if !m.starterLoaded { // double-check: another goroutine may have loaded it
		m.loadStarterLocked()
	}
	return m.starterName, m.starterAmount
}

// onConfigChanged rebuilds the materialized starter spec when a relevant config
// key changes. This is the ONLY spec-refresh path, so the subscription in Init is
// load-bearing: without this event a running inventory would never see an edit.
func (m *Module) onConfigChanged(e configevents.Changed) {
	if e.Namespace != "inventory" || (e.Key != "starter_item" && e.Key != "starter_qty") {
		return
	}
	m.starterMu.Lock()
	m.loadStarterLocked()
	item, qty := m.starterName, m.starterAmount
	m.starterMu.Unlock()
	m.log.Info("inventory starter reloaded from config", "item", item, "qty", qty)
}

// grantStarter gives a brand-new character its starter item. q is the messaging
// transport's per-subscriber inbox-dedup tx (never m.store.db), so the grant
// commits atomically with the (event_id,"inventory") dedup row. The item +
// quantity come from the materialized spec (config-sourced, live-reloaded).
func (m *Module) grantStarter(ctx context.Context, q rowQuerier, characterID string) error {
	item, qty := m.starterSpec()
	return m.store.grantExec(ctx, q, Owner{Type: "character", ID: characterID}, item, qty)
}

// wipeCharacter removes a deleted character's holdings. Same querier contract as
// grantStarter — the messaging inbox-dedup tx, so the wipe is atomic with dedup.
func (m *Module) wipeCharacter(ctx context.Context, q rowQuerier, characterID string) error {
	_, err := m.store.clearOwnerExec(ctx, q, Owner{Type: "character", ID: characterID})
	return err
}

// service backs both the "inventory" registry service AND the player operations
// (the generated edge face + the gateway's in-process invokers). It reads the
// caller identity from ctx (opsapi.PlayerID) — NEVER an argument — so a client
// cannot act on another player's inventory; the gateway established the identity
// once at the front door.
type service struct {
	store *store
	// characters authorizes ListCharacter (owner-check). Resolved in Init from the
	// registry to the real service (monolith) or the generated edge client (split).
	characters charactersSvc
}

// Compile-time proof the service satisfies the generated player contract — the
// single source of truth for the wire + gateway invoker shapes.
var _ inventoryapi.Holdings = (*service)(nil)

// ListMine returns the caller's own player-owned holdings (player_id from ctx).
func (s *service) ListMine(ctx context.Context) ([]Holding, error) {
	pid, ok := opsapi.PlayerID(ctx)
	if !ok {
		return nil, &opsapi.Error{Status: opsapi.StatusInvalid, Msg: "missing player identity"}
	}
	return s.store.list(ctx, Owner{Type: "player", ID: pid})
}

// ListCharacter returns a character's holdings, but only if the caller owns the
// character. The differentiated outcomes mirror the deleted handler's HTTP
// statuses, now typed: an ownership-lookup transport failure → StatusUnavailable
// (503), an unknown character → StatusNotFound (404), a character owned by
// someone else → StatusForbidden (403).
func (s *service) ListCharacter(ctx context.Context, characterID string) ([]Holding, error) {
	pid, ok := opsapi.PlayerID(ctx)
	if !ok {
		return nil, &opsapi.Error{Status: opsapi.StatusInvalid, Msg: "missing player identity"}
	}
	owner, found, err := s.characters.OwnerOf(ctx, characterID)
	if err != nil {
		// characters may be hosted in a peer process; a transport failure is an
		// infrastructure problem, not a missing character.
		return nil, &opsapi.Error{Status: opsapi.StatusUnavailable, Msg: "characters service unavailable"}
	}
	if !found {
		return nil, &opsapi.Error{Status: opsapi.StatusNotFound, Msg: "not found"}
	}
	if owner != pid {
		return nil, &opsapi.Error{Status: opsapi.StatusForbidden, Msg: "forbidden"}
	}
	return s.store.list(ctx, Owner{Type: "character", ID: characterID})
}

// Grant adds qty of itemID to the caller's own inventory (simulated IAP). A
// non-positive qty or an unknown item is a StatusInvalid (→ 400); a store failure
// is a plain error (→ StatusInternal → 500). It returns the caller's updated
// holdings, matching the old handler's respond-with-list behavior.
func (s *service) Grant(ctx context.Context, itemID string, qty int) ([]Holding, error) {
	pid, ok := opsapi.PlayerID(ctx)
	if !ok {
		return nil, &opsapi.Error{Status: opsapi.StatusInvalid, Msg: "missing player identity"}
	}
	if qty <= 0 {
		return nil, &opsapi.Error{Status: opsapi.StatusInvalid, Msg: "qty must be positive"}
	}
	exists, err := s.store.itemExists(ctx, itemID)
	if err != nil {
		return nil, err
	}
	if !exists {
		return nil, &opsapi.Error{Status: opsapi.StatusInvalid, Msg: "unknown item"}
	}
	owner := Owner{Type: "player", ID: pid}
	if err := s.store.grant(ctx, owner, itemID, qty); err != nil {
		return nil, err
	}
	return s.store.list(ctx, owner)
}

func envBool(key string, def bool) bool {
	v := os.Getenv(key)
	if v == "" {
		return def
	}
	return v == "1" || strings.EqualFold(v, "true") || strings.EqualFold(v, "on")
}
