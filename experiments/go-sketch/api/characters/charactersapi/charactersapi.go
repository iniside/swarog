// Package charactersapi is the characters module's pure, transport-free
// capability contract: the canonical Ownership interface a peer resolves over
// the edge. It imports only context — no edge, no transport — so it is the clean
// codegen INPUT for rpcgen, which reads it to generate the transport glue in the
// sibling charactersrpc package.
//
// Domain consumers (e.g. inventory) do NOT import this package: they keep their
// own local structural interface (CLAUDE.md rule 4). It is reached only by the
// generated glue (charactersrpc) + the remote stub — the provider-owned contract
// surface, same precedent as each module's <module>events package.
//
//go:generate go run gamebackend/tools/rpcgen -iface Ownership -prefix characters -out ../charactersrpc/charactersrpc_gen.go
//go:generate go run gamebackend/tools/rpcgen -iface Player -prefix characters -out ../charactersplayerrpc/charactersplayerrpc_gen.go
//go:generate go run gamebackend/tools/rpcgen -iface Admin -prefix characters -out ../charactersadminrpc/charactersadminrpc_gen.go
package charactersapi

import (
	"context"
	"time"

	"gamebackend/api/admin/adminapi"
	"gamebackend/opsapi"
)

// HTTPBindings declares the HTTP surface of the Player operations for rpcgen: it
// generates the gateway binding (route + Decode/Encode + invoker) for each method
// from this single source, so LocalBackend and RemoteBackend share one wire shape.
// Keyed by Go method name. Delete's id rides the {id} path wildcard into the
// wire field CharacterID; create/list carry a JSON body / no args.
var HTTPBindings = map[string]opsapi.HTTPBind{
	"Create": {Verb: "POST", Path: "/characters", Auth: opsapi.AuthPlayer, Success: 201},
	"List":   {Verb: "GET", Path: "/characters", Auth: opsapi.AuthPlayer, Success: 200},
	"Delete": {Verb: "DELETE", Path: "/characters/{id}", Auth: opsapi.AuthPlayer, Success: 204, PathArgs: map[string]string{"characterID": "id"}},
}

// Character is a player-owned character. PlayerID is a plain reference to
// accounts.players — no cross-module foreign key (logical isolation). It lives
// here (not in the impl package) because it is a return type of the Player
// capability, so the generated glue must be able to name it; the characters
// module aliases it as its own Character.
type Character struct {
	ID        string    `json:"id"`
	PlayerID  string    `json:"player_id"`
	Name      string    `json:"name"`
	Class     string    `json:"class"`
	CreatedAt time.Time `json:"created_at"`
}

// Ownership resolves a character's owning player. OwnerOf returns an error so a
// transport failure (the provider hosted in a peer process, reached over the
// QUIC edge) surfaces distinctly from a genuine "no such character"
// ("", false, nil). It matches the characters service's OwnerOf exactly.
type Ownership interface {
	OwnerOf(ctx context.Context, characterID string) (playerID string, ok bool, err error)
}

// Player is the character module's player-facing capability: the three
// operations a player performs on their OWN characters. Each takes its caller
// identity from ctx (opsapi.PlayerID, injected by the gateway after bearer
// verification), NEVER as an argument — so a client cannot act as another player.
// Create/Delete carry differentiated outcomes via opsapi.Status (a delete of a
// character the caller does not own is StatusNotFound). The characters service
// implements it exactly; the gateway/edge glue is generated from it.
type Player interface {
	Create(ctx context.Context, name, class string) (character Character, err error)
	List(ctx context.Context) (characters []Character, err error)
	Delete(ctx context.Context, characterID string) (err error)
}

// Admin is the characters module's admin fan-out capability: a peer's admin
// portal calls AdminData to render this module's page (KPIs + table) over the
// unified edge transport, exactly like ownerOf — no player identity, no bespoke
// HTTP endpoint. It returns the same adminapi.ItemData the in-process closure
// produces. The characters service implements it; the edge glue is generated.
type Admin interface {
	AdminData(ctx context.Context) (data adminapi.ItemData, err error)
}
