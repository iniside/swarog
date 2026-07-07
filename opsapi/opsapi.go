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

import (
	"context"
	"errors"
)

// Caller is the minimal transport a generated RPC client calls through. Both
// *edge.Client and (after Step A1) modules/remote's reconnecting edge conn
// satisfy it structurally — the signature mirrors edge.Client.Call exactly, so
// no adapter is needed on the edge client side.
type Caller interface {
	Call(ctx context.Context, method string, req, resp any) error
}

// Status is the operation error taxonomy carried through a generated RPC
// response envelope. edge's transport carries only a bare error string, which
// cannot distinguish a 404 from a 403 from a 503; a generated response envelope
// carries a Status so the gateway (later phase) can map a domain failure onto
// the right HTTP status instead of collapsing everything to 500. A handler
// returns a typed *Error; the generated server adapter records its Status in the
// envelope and the generated client reconstitutes an *Error from a non-OK Status.
type Status int

const (
	// StatusOK is the success status; a response carrying it has no error.
	StatusOK Status = iota
	// StatusNotFound — the addressed entity does not exist (→ HTTP 404).
	StatusNotFound
	// StatusForbidden — the caller is not permitted (→ HTTP 403).
	StatusForbidden
	// StatusInvalid — the request was malformed or failed validation (→ HTTP 400).
	StatusInvalid
	// StatusUnavailable — a dependency was unreachable; retry may succeed (→ HTTP 503).
	StatusUnavailable
	// StatusInternal — an unclassified server failure (→ HTTP 500). This is the
	// fallback StatusOf assigns to any error that is not an *Error.
	StatusInternal
)

// Error is a typed operation error a handler returns to select the Status that
// rides the response envelope. A plain (untyped) error maps to StatusInternal
// via StatusOf, so a handler opts into a specific status only when it wants one.
type Error struct {
	Status Status
	Msg    string
}

func (e *Error) Error() string { return e.Msg }

// StatusOf extracts the operation Status an error should map to: StatusOK for a
// nil error, the carried Status for an *Error, and StatusInternal for any other
// (plain) error. The generated server adapter calls it to fill the envelope.
func StatusOf(err error) Status {
	if err == nil {
		return StatusOK
	}
	var opErr *Error
	if errors.As(err, &opErr) {
		return opErr.Status
	}
	return StatusInternal
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

// LocalInvoker calls an operation's provider IN-PROCESS: it type-asserts req to
// the operation's concrete request type, invokes the provider (which the invoker
// has already resolved from the registry), and fills resp — a pointer to the
// concrete response type — with the result. NO serialization happens on this
// path (the monolith path, decision D3): req and resp cross the call as the
// exact decoded/allocated structs, never bytes. A domain failure is returned as
// an *Error carrying the Status; a nil error means StatusOK.
type LocalInvoker func(ctx context.Context, req, resp any) error

// LocalOp pairs an operation's method name with its in-process invoker. In Phase
// D the rpcgen-generated glue Contributes one to LocalSlot (the invoker closes
// over the provider service it resolved from ctx.Registry); the gateway's
// LocalBackend dispatches on Method. It is kept SEPARATE from Operation (the
// HTTP binding, contributed to Slot) so Operation stays pure, comparable data
// while the invoker — a func, non-comparable — rides its own slot.
type LocalOp struct {
	Method string
	Invoke LocalInvoker
}

// LocalSlot is the contribution slot the gateway reads to build its in-process
// dispatch table for LocalBackend. A provider contributes with
// ctx.Contribute(opsapi.LocalSlot, LocalOp{...}); the gateway reads
// ctx.Contributions(opsapi.LocalSlot). Empty until Phase D wires the first op.
const LocalSlot = "ops.local"
