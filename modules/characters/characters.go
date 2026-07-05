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
	"os"
	"strings"

	"gamebackend/core"
	"gamebackend/edge"
	"gamebackend/modules/admin/adminapi"
	"gamebackend/modules/characters/charactersevents"
	"gamebackend/outbox"
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
	bus      *core.Bus
	store    *store
	svc      *service
	accounts accountsSvc

	// Edge, when non-nil, is the process-wide QUIC RPC server (constructed and
	// started by main() only in a split that hosts this module). Init registers
	// the "characters.ownerOf" handler on it so a peer's inventory can resolve
	// ownership over the wire. nil in the monolith — no edge exposure.
	Edge *edge.Server

	// relay drains the transactional outbox and delivers character events to any
	// remote subscribers (EVENTS_SUBSCRIBERS). It runs in EVERY process that hosts
	// this real module: in the monolith no subscribers are configured, so it just
	// marks rows sent (drained to nobody); in a split it POSTs to the peer's sink.
	relay *outbox.Relay
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
CREATE INDEX IF NOT EXISTS characters_player_idx ON characters.characters(player_id);

-- Transactional outbox: an event row is written in the SAME tx as the domain
-- change, so it is durable iff the change committed. The relay drains it to
-- remote subscribers; sent_at NULL = not yet delivered.
CREATE TABLE IF NOT EXISTS characters.outbox (
	id         bigserial   PRIMARY KEY,
	topic      text        NOT NULL,
	payload    jsonb       NOT NULL,
	created_at timestamptz NOT NULL DEFAULT now(),
	sent_at    timestamptz
);
CREATE INDEX IF NOT EXISTS outbox_unsent_idx ON characters.outbox(id) WHERE sent_at IS NULL;`

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

	m.svc = &service{store: m.store}
	ctx.Provide("characters", m.svc)
	ctx.Contribute(adminapi.Slot, adminapi.Item{Section: "Game Content", Label: "Characters", Render: m.adminSection})

	// Construct (no I/O — Init only wires) the outbox relay. Subscribers come from
	// EVENTS_SUBSCRIBERS (empty in the monolith). Start launches its drain loop.
	m.relay = outbox.NewRelay(m.store.db, "characters",
		outbox.ParseSubscribers(os.Getenv("EVENTS_SUBSCRIBERS")), m.log)

	// Split topology: expose OwnerOf over the shared QUIC edge server so a peer
	// process's inventory can resolve character ownership. Registering a handler
	// is pure wiring (no I/O); main() starts the listener after all Inits.
	if m.Edge != nil {
		m.Edge.Handle("characters.ownerOf", ownerOfEdgeHandler(m.svc))
		m.log.Info("edge handler registered", "method", "characters.ownerOf")
	}
	return nil
}

// Start launches the outbox relay's background drain loop (Starter).
func (m *Module) Start(ctx context.Context) error {
	if m.relay == nil {
		return nil
	}
	return m.relay.Start(ctx)
}

// Stop halts the outbox relay (Stopper), reverse of Start.
func (m *Module) Stop(ctx context.Context) error {
	if m.relay == nil {
		return nil
	}
	return m.relay.Stop(ctx)
}

// ownerOfReq/ownerOfResp are the wire DTOs for the "characters.ownerOf" edge
// RPC. The remote client in modules/remote mirrors these field tags — the only
// coupling between the two sides is this JSON shape + the method name.
type ownerOfReq struct {
	ID string `json:"id"`
}

type ownerOfResp struct {
	PlayerID string `json:"player_id"`
	Ok       bool   `json:"ok"`
}

// ownerOfEdgeHandler adapts the local OwnerOf capability to an edge.Handler: it
// decodes the request, calls the service, and encodes the reply. A store error
// is returned as the handler error, which the client surfaces as a transport-
// level err (→ 503 at the consumer) rather than a false "not found".
func ownerOfEdgeHandler(svc *service) edge.Handler {
	return func(reqPayload []byte) ([]byte, error) {
		var req ownerOfReq
		if err := json.Unmarshal(reqPayload, &req); err != nil {
			return nil, err
		}
		pid, ok, err := svc.OwnerOf(context.Background(), req.ID)
		if err != nil {
			return nil, err
		}
		return json.Marshal(ownerOfResp{PlayerID: pid, Ok: ok})
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
	payload, err := json.Marshal(evt)
	if err != nil {
		m.log.Error("create character: marshal event", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	if err := m.store.insertOutbox(r.Context(), tx, charactersevents.CreatedEvent.Topic(), payload); err != nil {
		m.log.Error("create character: outbox insert", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	if err := tx.Commit(); err != nil {
		m.log.Error("create character: commit", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	// S4: a crash HERE (after commit, before Emit) loses only the LOCAL co-located
	// delivery — the outbox row is already durable, so a remote subscriber still
	// gets it via the relay. This matches the bus's existing best-effort semantics.
	core.Emit(m.bus, charactersevents.CreatedEvent, evt)
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
	payload, err := json.Marshal(evt)
	if err != nil {
		m.log.Error("delete character: marshal event", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	if err := m.store.insertOutbox(r.Context(), tx, charactersevents.DeletedEvent.Topic(), payload); err != nil {
		m.log.Error("delete character: outbox insert", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	if err := tx.Commit(); err != nil {
		m.log.Error("delete character: commit", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	// S4: crash after commit before Emit loses only the local delivery (see create).
	core.Emit(m.bus, charactersevents.DeletedEvent, evt)
	w.WriteHeader(http.StatusNoContent)
}

// service is what other modules get from Require("characters").
type service struct{ store *store }

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
