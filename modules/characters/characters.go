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

	"gamebackend/bus"
	"gamebackend/edge"
	"gamebackend/lifecycle"
	"gamebackend/modules/admin/adminapi"
	"gamebackend/modules/characters/charactersapi"
	"gamebackend/modules/characters/charactersevents"
	"gamebackend/modules/characters/charactersrpc"
	"gamebackend/registry"
)

// accountsSvc is the slice of the accounts service we need (consumer-defined
// interface — we depend on a capability, not the package). VerifySession returns
// an error so a transport failure (accounts hosted in a peer process, reached
// over the QUIC edge) surfaces distinctly from a genuine "not a valid session".
type accountsSvc interface {
	VerifySession(ctx context.Context, token string) (playerID string, ok bool, err error)
}

type Module struct {
	log      *slog.Logger
	bus      *bus.Bus
	store    *store
	svc      *service
	accounts accountsSvc

	// Edge, when non-nil, is the process-wide QUIC RPC server (constructed and
	// started by main() only in a split that hosts this module). Init registers
	// the "characters.ownerOf" handler on it so a peer's inventory can resolve
	// ownership over the wire. nil in the monolith — no edge exposure.
	Edge *edge.Server
}

func (*Module) Name() string       { return "characters" }
func (*Module) Requires() []string { return []string{"accounts", "messaging"} }

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

// Register constructs the store-backed service and offers it to other modules.
// It runs in Build's phase 1, before any Init, so a dependent's Require resolves
// regardless of registration order. It touches only ctx.DB (available now); the
// service closes over m.store alone, never a Required dependency.
func (m *Module) Register(ctx *lifecycle.Context) error {
	m.store = &store{db: ctx.DB, log: ctx.Log}
	m.svc = &service{store: m.store}
	registry.Provide(ctx.Registry, "characters", m.svc)
	return nil
}

func (m *Module) Init(ctx *lifecycle.Context) error {
	m.log = ctx.Log
	m.bus = ctx.Bus
	m.accounts = registry.Require[accountsSvc](ctx.Registry, "accounts")

	ctx.Mux.HandleFunc("POST /characters", m.handleCreate)
	ctx.Mux.HandleFunc("GET /characters", m.handleList)
	ctx.Mux.HandleFunc("DELETE /characters/{id}", m.handleDelete)

	// The characters service was Provided in Register (phase 1); m.store/m.svc are set.
	ctx.Contribute(adminapi.Slot, adminapi.Item{ID: adminItemID, Section: adminSectionName, Label: adminLabel, Render: m.adminSection})
	// GET /admin-data/characters: the same content, served over HTTP so a remote
	// admin process can fetch it. In the monolith the admin uses the closure above.
	ctx.Mux.HandleFunc("GET /admin-data/"+adminItemID, m.handleAdminData)

	// Split topology: expose OwnerOf over the shared QUIC edge server so a peer
	// process's inventory can resolve character ownership. Registering a handler
	// is pure wiring (no I/O); main() starts the listener after all Inits.
	if m.Edge != nil {
		// The OwnerOf edge glue (envelope + adapter) is rpcgen-generated from
		// charactersapi.Ownership — one RegisterServer call replaces the hand
		// adapter + mirrored DTOs + wire_contract_test that used to live here.
		// Hand the service to the glue AS the pure contract interface (not the
		// concrete *service): the glue depends on the capability, never the impl.
		var own charactersapi.Ownership = m.svc
		charactersrpc.RegisterServer(m.Edge, own)
		m.log.Info("edge handler registered", "method", charactersrpc.MethodOwnerOf)
		m.Edge.Handle("characters.list", charactersListEdgeHandler(m.svc))
		m.log.Info("edge handler registered", "method", "characters.list")
	}
	return nil
}

// listReq/listResp are the wire DTOs for the "characters.list" edge RPC. A
// gateway routes a player's character list here; the only coupling to a
// caller is this JSON shape + the method name.
type listReq struct {
	PlayerID string `json:"player_id"`
}

type listResp struct {
	Characters []Character `json:"characters"`
}

// charactersListEdgeHandler adapts the local ListByPlayer capability to an
// edge.Handler: it decodes the request, calls the service, and encodes the
// reply. A store error is returned as the handler error, which the client
// surfaces as a transport-level err rather than a false empty list.
func charactersListEdgeHandler(svc *service) edge.Handler {
	return func(reqPayload []byte) ([]byte, error) {
		var req listReq
		if err := json.Unmarshal(reqPayload, &req); err != nil {
			return nil, err
		}
		list, err := svc.ListByPlayer(context.Background(), req.PlayerID)
		if err != nil {
			return nil, err
		}
		return json.Marshal(listResp{Characters: list})
	}
}

func (m *Module) handleCreate(w http.ResponseWriter, r *http.Request) {
	pid, ok := m.authed(w, r)
	if !ok {
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

	// Domain write + outbox row in ONE tx: the event is durable iff the character
	// is. The character id (needed in the payload) comes from the INSERT RETURNING.
	tx, err := m.store.db.BeginTx(r.Context(), nil)
	if err != nil {
		m.log.Error("create character: begin tx", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	defer func() { _ = tx.Rollback() }() // no-op after a successful Commit

	c, err := m.store.createTx(r.Context(), tx, pid, in.Name, class)
	if err != nil {
		m.log.Error("create character failed", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	evt := charactersevents.Created{CharacterID: c.ID, PlayerID: c.PlayerID, Name: c.Name, Class: c.Class}
	if err := bus.EmitTx(m.bus, tx, charactersevents.CreatedEvent, evt); err != nil {
		m.log.Error("create character: emit event", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	if err := tx.Commit(); err != nil {
		m.log.Error("create character: commit", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	writeJSON(w, http.StatusCreated, c)
}

func (m *Module) handleList(w http.ResponseWriter, r *http.Request) {
	pid, ok := m.authed(w, r)
	if !ok {
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
	pid, ok := m.authed(w, r)
	if !ok {
		return
	}
	id := r.PathValue("id")

	tx, err := m.store.db.BeginTx(r.Context(), nil)
	if err != nil {
		m.log.Error("delete character: begin tx", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	defer func() { _ = tx.Rollback() }() // no-op after a successful Commit

	deleted, err := m.store.deleteOwnedTx(r.Context(), tx, id, pid)
	if err != nil {
		m.log.Error("delete character failed", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	if !deleted {
		// Nothing deleted → no event. Rollback (deferred) and 404.
		http.Error(w, "not found", http.StatusNotFound)
		return
	}
	evt := charactersevents.Deleted{CharacterID: id, PlayerID: pid}
	if err := bus.EmitTx(m.bus, tx, charactersevents.DeletedEvent, evt); err != nil {
		m.log.Error("delete character: emit event", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	if err := tx.Commit(); err != nil {
		m.log.Error("delete character: commit", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

// service is what other modules get from Require("characters").
type service struct{ store *store }

// service is the impl behind the generated OwnerOf edge glue: it satisfies the
// pure charactersapi.Ownership contract rpcgen reads. This assertion fails to
// compile if the service's OwnerOf drifts from the generated server adapter.
var _ charactersapi.Ownership = (*service)(nil)

// OwnerOf returns the owning player of a character. A genuine "no such
// character" is ("", false, nil); a store failure now propagates as a non-nil
// error instead of being swallowed as ok=false (B2), so a consumer can tell a
// real 404 apart from an infrastructure failure.
func (s *service) OwnerOf(ctx context.Context, characterID string) (playerID string, ok bool, err error) {
	c, found, err := s.store.get(ctx, characterID)
	if err != nil {
		return "", false, err
	}
	if !found {
		return "", false, nil
	}
	return c.PlayerID, true, nil
}

func (s *service) ListByPlayer(ctx context.Context, playerID string) ([]Character, error) {
	return s.store.listByPlayer(ctx, playerID)
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
