// Package inventory owns item holdings for any owner — a player (e.g. IAP) or a
// character. It depends on accounts (auth) and characters (ownership checks), and
// REACTS to character lifecycle events: granting a starter item on creation and
// wiping holdings on deletion. characters has no idea inventory exists.
package inventory

import (
	"context"
	"database/sql"
	"encoding/json"
	"log/slog"
	"net/http"
	"os"
	"strings"

	"gamebackend/core"
	"gamebackend/modules/admin/adminapi"
	"gamebackend/modules/characters/charactersevents"
)

const starterItem = "starter_sword"

// accountsSvc and charactersSvc are the consumer-defined slices we depend on.
// Both return an error so a transport failure (the provider hosted in a peer
// process, reached over the QUIC edge) surfaces as 503 rather than a false 401
// or 404. The local, co-hosted implementations return a nil error.
type accountsSvc interface {
	VerifySession(ctx context.Context, token string) (playerID string, ok bool, err error)
}

type charactersSvc interface {
	OwnerOf(ctx context.Context, characterID string) (playerID string, ok bool, err error)
}

type Module struct {
	log        *slog.Logger
	store      *store
	accounts   accountsSvc
	characters charactersSvc
}

func (*Module) Name() string        { return "inventory" }
func (*Module) DependsOn() []string { return []string{"accounts", "characters"} }

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

func (m *Module) Init(ctx *core.Context) error {
	m.log = ctx.Log
	m.store = &store{db: ctx.DB, log: ctx.Log}
	m.accounts = ctx.Require("accounts").(accountsSvc)
	m.characters = ctx.Require("characters").(charactersSvc)

	// React to character lifecycle — integrity without a cross-module FK.
	core.On(ctx.Bus, charactersevents.CreatedEvent, m.onCharacterCreated)
	core.On(ctx.Bus, charactersevents.DeletedEvent, m.onCharacterDeleted)

	ctx.Mux.HandleFunc("GET /inventory/me", m.handleMine)
	ctx.Mux.HandleFunc("GET /inventory/character/{id}", m.handleCharacter)

	if envBool("INVENTORY_DEV_GRANT", true) {
		ctx.Log.Warn("INVENTORY_DEV_GRANT is ON — POST /inventory/me/grant (simulated IAP) is enabled; turn OFF in production")
		ctx.Mux.HandleFunc("POST /inventory/me/grant", m.handleGrant)
	}

	ctx.Provide("inventory", &service{store: m.store})
	ctx.Contribute(adminapi.Slot, adminapi.Item{Section: "Game Content", Label: "Inventory", Render: m.adminSection})
	return nil
}

// onCharacterCreated gives a brand-new character a starter item.
func (m *Module) onCharacterCreated(e charactersevents.Created) {
	if err := m.store.grant(context.Background(), Owner{Type: "character", ID: e.CharacterID}, starterItem, 1); err != nil {
		m.log.Error("starter grant failed", "character", e.CharacterID, "err", err)
	}
}

// onCharacterDeleted wipes a deleted character's inventory.
func (m *Module) onCharacterDeleted(e charactersevents.Deleted) {
	if _, err := m.store.clearOwner(context.Background(), Owner{Type: "character", ID: e.CharacterID}); err != nil {
		m.log.Error("inventory cleanup failed", "character", e.CharacterID, "err", err)
	}
}

func (m *Module) handleMine(w http.ResponseWriter, r *http.Request) {
	pid, ok := m.authed(w, r)
	if !ok {
		return
	}
	m.respondList(w, r, Owner{Type: "player", ID: pid})
}

func (m *Module) handleCharacter(w http.ResponseWriter, r *http.Request) {
	pid, ok := m.authed(w, r)
	if !ok {
		return
	}
	id := r.PathValue("id")
	owner, found, err := m.characters.OwnerOf(r.Context(), id)
	if err != nil {
		// Characters may be hosted in a peer process; a transport failure is an
		// infrastructure problem, not a missing character (B2).
		m.log.Error("ownership lookup failed", "character", id, "err", err)
		http.Error(w, "characters service unavailable", http.StatusServiceUnavailable)
		return
	}
	if !found {
		http.Error(w, "not found", http.StatusNotFound)
		return
	}
	if owner != pid {
		http.Error(w, "forbidden", http.StatusForbidden)
		return
	}
	m.respondList(w, r, Owner{Type: "character", ID: id})
}

func (m *Module) handleGrant(w http.ResponseWriter, r *http.Request) {
	pid, ok := m.authed(w, r)
	if !ok {
		return
	}
	var in struct {
		ItemID string `json:"item_id"`
		Qty    int    `json:"qty"`
	}
	if err := json.NewDecoder(r.Body).Decode(&in); err != nil {
		http.Error(w, "invalid json", http.StatusBadRequest)
		return
	}
	if in.Qty <= 0 {
		http.Error(w, "qty must be positive", http.StatusBadRequest)
		return
	}
	exists, err := m.store.itemExists(r.Context(), in.ItemID)
	if err != nil {
		m.log.Error("item check failed", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	if !exists {
		http.Error(w, "unknown item", http.StatusBadRequest)
		return
	}
	if err := m.store.grant(r.Context(), Owner{Type: "player", ID: pid}, in.ItemID, in.Qty); err != nil {
		m.log.Error("grant failed", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	m.respondList(w, r, Owner{Type: "player", ID: pid})
}

func (m *Module) respondList(w http.ResponseWriter, r *http.Request, owner Owner) {
	holdings, err := m.store.list(r.Context(), owner)
	if err != nil {
		m.log.Error("list inventory failed", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	writeJSON(w, http.StatusOK, holdings)
}

// service is what other modules get from Require("inventory").
type service struct{ store *store }

func (s *service) Grant(ctx context.Context, owner Owner, itemID string, qty int) error {
	ok, err := s.store.itemExists(ctx, itemID)
	if err != nil {
		return err
	}
	if !ok {
		return ErrUnknownItem
	}
	return s.store.grant(ctx, owner, itemID, qty)
}

func (s *service) List(ctx context.Context, owner Owner) ([]Holding, error) {
	return s.store.list(ctx, owner)
}

func (m *Module) auth(r *http.Request) (playerID string, ok bool, err error) {
	token := bearer(r)
	if token == "" {
		return "", false, nil
	}
	return m.accounts.VerifySession(r.Context(), token)
}

// authed verifies the bearer token and writes the right failure response: 503 if
// the accounts service (possibly a peer reached over the edge) is unreachable,
// 401 if the token is missing or invalid. Returns ok=false once it responds.
func (m *Module) authed(w http.ResponseWriter, r *http.Request) (playerID string, ok bool) {
	pid, ok, err := m.auth(r)
	if err != nil {
		m.log.Error("session verify failed", "err", err)
		http.Error(w, "auth service unavailable", http.StatusServiceUnavailable)
		return "", false
	}
	if !ok {
		http.Error(w, "unauthorized", http.StatusUnauthorized)
		return "", false
	}
	return pid, true
}

func bearer(r *http.Request) string {
	if after, found := strings.CutPrefix(r.Header.Get("Authorization"), "Bearer "); found {
		return after
	}
	return ""
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}

func envBool(key string, def bool) bool {
	v := os.Getenv(key)
	if v == "" {
		return def
	}
	return v == "1" || strings.EqualFold(v, "true") || strings.EqualFold(v, "on")
}
