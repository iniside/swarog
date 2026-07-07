// Package characters owns player characters: a player has N characters. It
// emits lifecycle events that other modules (e.g. inventory) react to — it never
// knows who. Its player-facing operations (create/list/delete a player's own
// characters) are exposed as opsapi Operations: the gateway fronts the HTTP
// routes, authenticates ONCE, and dispatches to the service with the verified
// caller player_id injected into ctx (opsapi.PlayerID). The service never reads a
// client-supplied identity — the trust boundary lives in ctx.
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
	"gamebackend/modules/characters/charactersplayerrpc"
	"gamebackend/modules/characters/charactersrpc"
	"gamebackend/opsapi"
	"gamebackend/registry"
)

type Module struct {
	log   *slog.Logger
	bus   *bus.Bus
	store *store
	svc   *service

	// Edge, when non-nil, is the process-wide QUIC RPC server (constructed and
	// started by main() only in a split that hosts this module). Init registers the
	// "characters.ownerOf" handler (for a peer's inventory) AND the player-operation
	// handlers (characters.create/list/delete, for a front gateway in a peer) on it.
	// nil in the monolith — no edge exposure.
	Edge *edge.Server
}

func (*Module) Name() string       { return "characters" }
func (*Module) Requires() []string { return []string{"messaging"} }

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
// service's bus + logger (needed for the create/delete write+event tx) are wired
// in Init, before any request can arrive.
func (m *Module) Register(ctx *lifecycle.Context) error {
	m.store = &store{db: ctx.DB, log: ctx.Log}
	m.svc = &service{store: m.store}
	registry.Provide(ctx.Registry, "characters", m.svc)
	return nil
}

func (m *Module) Init(ctx *lifecycle.Context) error {
	m.log = ctx.Log
	m.bus = ctx.Bus

	// Finish wiring the service Provided in Register: it needs the bus + logger to
	// run the domain write + outbox event in one tx (the logic that used to live in
	// the HTTP handlers, now behind the gateway-fronted operations).
	m.svc.bus = m.bus
	m.svc.log = m.log

	// Player operations: contribute each op's HTTP binding + in-process invoker so
	// the gateway fronts POST/GET/DELETE /characters, authenticates once, and
	// dispatches to m.svc with the verified player_id in ctx. This REPLACES the
	// deleted handleCreate/handleList/handleDelete + their inline bearer auth.
	registerPlayerOps(ctx, m.svc)

	// The characters service was Provided in Register (phase 1); m.store/m.svc are set.
	ctx.Contribute(adminapi.Slot, adminapi.Item{ID: adminItemID, Section: adminSectionName, Label: adminLabel, Render: m.adminSection})
	// GET /admin-data/characters: the same content, served over HTTP so a remote
	// admin process can fetch it. In the monolith the admin uses the closure above.
	ctx.Mux.HandleFunc("GET /admin-data/"+adminItemID, m.handleAdminData)

	// Split topology: expose the characters capabilities over the shared QUIC edge
	// server so a peer process can resolve ownership (inventory) or front the player
	// operations (a gateway). Registering handlers is pure wiring (no I/O); main()
	// starts the listener after all Inits.
	if m.Edge != nil {
		// Both edge faces are rpcgen-generated from the pure charactersapi contracts:
		// one RegisterServer per interface installs identity-aware adapters (they read
		// the request envelope's Identity into ctx via opsapi.WithPlayerID).
		var own charactersapi.Ownership = m.svc
		charactersrpc.RegisterServer(m.Edge, own)
		var player charactersapi.Player = m.svc
		charactersplayerrpc.RegisterServer(m.Edge, player)
		m.log.Info("edge handlers registered", "methods",
			[]string{charactersrpc.MethodOwnerOf, charactersplayerrpc.MethodCreate, charactersplayerrpc.MethodList, charactersplayerrpc.MethodDelete})
	}
	return nil
}

// service is what other modules get from Require("characters"); it also backs
// both generated edge faces (Ownership + Player) and the gateway's in-process
// operation invokers.
type service struct {
	store *store
	bus   *bus.Bus
	log   *slog.Logger
}

// These assertions fail to compile if the service drifts from either generated
// contract — the single source of truth for the wire + gateway shapes.
var (
	_ charactersapi.Ownership = (*service)(nil)
	_ charactersapi.Player    = (*service)(nil)
)

// OwnerOf returns the owning player of a character. A genuine "no such
// character" is ("", false, nil); a store failure propagates as a non-nil error
// so a consumer can tell a real 404 apart from an infrastructure failure.
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

// Create adds a character owned by the caller (player_id read from ctx, NEVER an
// argument — the gateway injected it after verifying the bearer). The domain
// write + the character.created outbox row commit in ONE tx: the event is durable
// iff the character is. A missing identity or empty name is a StatusInvalid
// (→ 400); the character id comes from INSERT RETURNING.
func (s *service) Create(ctx context.Context, name, class string) (Character, error) {
	pid, ok := opsapi.PlayerID(ctx)
	if !ok {
		return Character{}, &opsapi.Error{Status: opsapi.StatusInvalid, Msg: "missing player identity"}
	}
	if strings.TrimSpace(name) == "" {
		return Character{}, &opsapi.Error{Status: opsapi.StatusInvalid, Msg: "name is required"}
	}
	if class == "" {
		class = "novice"
	}

	tx, err := s.store.db.BeginTx(ctx, nil)
	if err != nil {
		s.log.Error("create character: begin tx", "err", err)
		return Character{}, err
	}
	defer func() { _ = tx.Rollback() }() // no-op after a successful Commit

	c, err := s.store.createTx(ctx, tx, pid, name, class)
	if err != nil {
		s.log.Error("create character failed", "err", err)
		return Character{}, err
	}
	evt := charactersevents.Created{CharacterID: c.ID, PlayerID: c.PlayerID, Name: c.Name, Class: c.Class}
	if err := bus.EmitTx(s.bus, tx, charactersevents.CreatedEvent, evt); err != nil {
		s.log.Error("create character: emit event", "err", err)
		return Character{}, err
	}
	if err := tx.Commit(); err != nil {
		s.log.Error("create character: commit", "err", err)
		return Character{}, err
	}
	return c, nil
}

// List returns the caller's own characters (player_id from ctx).
func (s *service) List(ctx context.Context) ([]Character, error) {
	pid, ok := opsapi.PlayerID(ctx)
	if !ok {
		return nil, &opsapi.Error{Status: opsapi.StatusInvalid, Msg: "missing player identity"}
	}
	return s.store.listByPlayer(ctx, pid)
}

// Delete removes one of the caller's characters. Deleting a character the caller
// does not own (or one that does not exist) is a StatusNotFound (→ 404) — the
// same 404 the old handler returned, now typed. The delete + the
// character.deleted outbox row commit atomically.
func (s *service) Delete(ctx context.Context, characterID string) error {
	pid, ok := opsapi.PlayerID(ctx)
	if !ok {
		return &opsapi.Error{Status: opsapi.StatusInvalid, Msg: "missing player identity"}
	}

	tx, err := s.store.db.BeginTx(ctx, nil)
	if err != nil {
		s.log.Error("delete character: begin tx", "err", err)
		return err
	}
	defer func() { _ = tx.Rollback() }() // no-op after a successful Commit

	deleted, err := s.store.deleteOwnedTx(ctx, tx, characterID, pid)
	if err != nil {
		s.log.Error("delete character failed", "err", err)
		return err
	}
	if !deleted {
		// Nothing deleted (not found or not owned) → no event, 404.
		return &opsapi.Error{Status: opsapi.StatusNotFound, Msg: "character not found"}
	}
	evt := charactersevents.Deleted{CharacterID: characterID, PlayerID: pid}
	if err := bus.EmitTx(s.bus, tx, charactersevents.DeletedEvent, evt); err != nil {
		s.log.Error("delete character: emit event", "err", err)
		return err
	}
	if err := tx.Commit(); err != nil {
		s.log.Error("delete character: commit", "err", err)
		return err
	}
	return nil
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}
