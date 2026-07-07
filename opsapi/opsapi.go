// Package opsapi is the leaf that declares the vocabulary of internal
// operations plus the transport seam a generated RPC client calls through. It
// is a leaf in the strict sense: stdlib only, importable by everyone, importing
// no module.
//
// Two independent things live here, both foundational to the unified operation
// transport:
//
//   - Caller — the minimal transport a generated RPC client (Step 0.2 rpcgen)
//     calls through. The generated client targets Caller rather than a concrete
//     type, so it is transport-agnostic: it composes over *edge.Client directly
//     AND over remote's self-healing reconnecting edge conn. Depend on the
//     capability, not the package.
//
//   - Operation + Slot — the declaration seam. A module Contributes one
//     Operation per capability it exposes (into the "ops.operation" slot, the
//     same Contribute/Contributions mechanism admin uses for its items); the
//     gateway reads every contribution to build its HTTP route table. A module
//     lights up a route by contributing, never by the gateway importing it.
package opsapi

import "context"

// Caller is the minimal transport a generated RPC client calls through. Both
// *edge.Client and (after Step A1) modules/remote's reconnecting edge conn
// satisfy it structurally — the signature mirrors edge.Client.Call exactly, so
// no adapter is needed on the edge client side.
type Caller interface {
	Call(ctx context.Context, method string, req, resp any) error
}

// AuthReq states what identity guarantee an operation needs the gateway to
// establish before it dispatches. It is declared per operation so the auth
// requirement lives beside the route, not triplicated inline in each handler.
type AuthReq int

const (
	// AuthNone — the operation is public; the gateway dispatches without a
	// bearer (e.g. match/report, login/register, leaderboard).
	AuthNone AuthReq = iota
	// AuthPlayer — the gateway verifies the bearer token and injects the
	// resolved player_id, so the backend never reads a client-supplied identity.
	AuthPlayer
)

// Operation is one internal capability a module exposes, declared as a
// contribution the gateway reads to bind an HTTP route to an RPC method. A
// module Contributes one per operation; nothing else is wired.
type Operation struct {
	Method string  // the rpc method name, e.g. "characters.create"
	Verb   string  // HTTP verb the gateway binds, e.g. "POST"
	Path   string  // HTTP path pattern, e.g. "/characters" or "/characters/{id}"
	Auth   AuthReq // identity the gateway must establish before dispatch
}

// Slot is the contribution slot the gateway reads to build its route table. A
// module contributes with ctx.Contribute(opsapi.Slot, op); the gateway reads
// ctx.Contributions(opsapi.Slot). Same multi-value seam as adminapi.Slot.
const Slot = "ops.operation"
