// Package characters owns player characters: a player has N characters. It
// depends on accounts (to authenticate the owning player) and emits lifecycle
// events that other modules (e.g. inventory) react to — it never knows who.
package characters

import (
	"context"
	"database/sql"
	"encoding/json"
	"log/slog"
	"net/http"
	"strings"

	"gamebackend/core"
	"gamebackend/modules/admin/adminapi"
	"gamebackend/modules/characters/charactersevents"
)

// accountsSvc is the slice of the accounts service we need (consumer-defined
// interface — we depend on a capability, not the package).
type accountsSvc interface {
	VerifySession(ctx context.Context, token string) (playerID string, ok bool)
}

type Module struct {
	log      *slog.Logger
	bus      *core.Bus
	store    *store
	accounts accountsSvc
}

func (*Module) Name() string        { return "characters" }
func (*Module) DependsOn() []string { return []string{"accounts"} }

const schemaDDL = `
CREATE SCHEMA IF NOT EXISTS characters;
CREATE TABLE IF NOT EXISTS characters.characters (
	id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	player_id  uuid        NOT NULL,            -- ref accounts.players, no cross-module FK
	name       text        NOT NULL,
	class      text        NOT NULL DEFAULT 'novice',
	created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS characters_player_idx ON characters.characters(player_id);`

func (*Module) Migrate(_ context.Context, db *sql.DB) error {
	_, err := db.Exec(schemaDDL)
	return err
}

func (m *Module) Init(ctx *core.Context) error {
	m.log = ctx.Log
	m.bus = ctx.Bus
	m.store = &store{db: ctx.DB, log: ctx.Log}
	m.accounts = ctx.Require("accounts").(accountsSvc)

	ctx.Mux.HandleFunc("POST /characters", m.handleCreate)
	ctx.Mux.HandleFunc("GET /characters", m.handleList)
	ctx.Mux.HandleFunc("DELETE /characters/{id}", m.handleDelete)

	ctx.Provide("characters", &service{store: m.store})
	ctx.Contribute(adminapi.Slot, adminapi.Section{Title: "Characters", Render: m.adminSection})
	return nil
}

func (m *Module) handleCreate(w http.ResponseWriter, r *http.Request) {
	pid, ok := m.auth(r)
	if !ok {
		http.Error(w, "unauthorized", http.StatusUnauthorized)
		return
	}
	var in struct {
		Name  string `json:"name"`
		Class string `json:"class"`
	}
	if err := json.NewDecoder(r.Body).Decode(&in); err != nil {
		http.Error(w, "invalid json", http.StatusBadRequest)
		return
	}
	if strings.TrimSpace(in.Name) == "" {
		http.Error(w, "name is required", http.StatusBadRequest)
		return
	}
	class := in.Class
	if class == "" {
		class = "novice"
	}

	c, err := m.store.create(r.Context(), pid, in.Name, class)
	if err != nil {
		m.log.Error("create character failed", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	core.Emit(m.bus, charactersevents.CreatedEvent, charactersevents.Created{
		CharacterID: c.ID, PlayerID: c.PlayerID, Name: c.Name, Class: c.Class,
	})
	writeJSON(w, http.StatusCreated, c)
}

func (m *Module) handleList(w http.ResponseWriter, r *http.Request) {
	pid, ok := m.auth(r)
	if !ok {
		http.Error(w, "unauthorized", http.StatusUnauthorized)
		return
	}
	list, err := m.store.listByPlayer(r.Context(), pid)
	if err != nil {
		m.log.Error("list characters failed", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	writeJSON(w, http.StatusOK, list)
}

func (m *Module) handleDelete(w http.ResponseWriter, r *http.Request) {
	pid, ok := m.auth(r)
	if !ok {
		http.Error(w, "unauthorized", http.StatusUnauthorized)
		return
	}
	id := r.PathValue("id")
	deleted, err := m.store.deleteOwned(r.Context(), id, pid)
	if err != nil {
		m.log.Error("delete character failed", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	if !deleted {
		http.Error(w, "not found", http.StatusNotFound)
		return
	}
	core.Emit(m.bus, charactersevents.DeletedEvent, charactersevents.Deleted{CharacterID: id, PlayerID: pid})
	w.WriteHeader(http.StatusNoContent)
}

// service is what other modules get from Require("characters").
type service struct{ store *store }

func (s *service) OwnerOf(ctx context.Context, characterID string) (playerID string, ok bool) {
	c, found, err := s.store.get(ctx, characterID)
	if err != nil || !found {
		return "", false
	}
	return c.PlayerID, true
}

func (s *service) ListByPlayer(ctx context.Context, playerID string) ([]Character, error) {
	return s.store.listByPlayer(ctx, playerID)
}

func (m *Module) auth(r *http.Request) (playerID string, ok bool) {
	token := bearer(r)
	if token == "" {
		return "", false
	}
	return m.accounts.VerifySession(r.Context(), token)
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
