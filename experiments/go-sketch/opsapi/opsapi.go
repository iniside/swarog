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

// playerIDKey is the private context key under which a caller's VERIFIED
// player_id is carried. It is unexported so the ONLY way to set or read it is
// via WithPlayerID / PlayerID below — a domain operation cannot fabricate an
// identity, and cannot accidentally read one from an untrusted place.
type playerIDKey struct{}

// WithPlayerID returns a child context carrying pid as the caller's verified
// player identity. It is set in exactly TWO trusted places and NOWHERE else:
//
//   - the gateway front-handler, AFTER it has verified the bearer token for an
//     AuthPlayer route (the in-process / LocalBackend path); and
//   - the generated RPC server adapter, from the mTLS-authenticated edge request
//     envelope's Identity field (the cross-process / RemoteBackend path).
//
// A domain operation reads it back with PlayerID. This is the whole trust
// boundary: identity is established ONCE at the edge of the system and flows
// inward through ctx, never re-derived from a client-supplied field mid-stack.
func WithPlayerID(ctx context.Context, pid string) context.Context {
	return context.WithValue(ctx, playerIDKey{}, pid)
}

// PlayerID returns the caller's verified player_id and whether one is present.
//
// TRUST BOUNDARY — a domain operation MUST take its caller identity ONLY from
// here. It must NEVER read a player_id from an HTTP header, a query param, a
// request body field, or any other client-supplied value: those are attacker-
// controlled. The value returned here was set by WithPlayerID at a trusted seam
// (gateway-after-bearer-verify, or a generated adapter from the mTLS-authed edge
// envelope). ok=false means no identity was established — an operation that
// requires one should return &Error{Status: StatusInvalid}, never proceed with
// an empty player_id.
func PlayerID(ctx context.Context) (string, bool) {
	pid, ok := ctx.Value(playerIDKey{}).(string)
	return pid, ok && pid != ""
}

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
	// StatusUnauthorized — the request lacked valid credentials (→ HTTP 401). It is
	// distinct from StatusForbidden (403, an AUTHENTICATED caller lacking permission)
	// and StatusInvalid (400, a malformed request): it is the outcome an AuthNone
	// auth operation returns when a password is wrong or a token is rejected, so the
	// gateway answers 401 exactly as the pre-migration inline handlers did.
	StatusUnauthorized
	// StatusConflict — the request conflicts with existing durable state (→ HTTP
	// 409), e.g. registering an email already taken. Preserves the 409 the accounts
	// register handler returned before it became an operation.
	StatusConflict
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
	Method  string  // the rpc method name, e.g. "characters.create"
	Verb    string  // HTTP verb the gateway binds, e.g. "POST"
	Path    string  // HTTP path pattern, e.g. "/characters" or "/characters/{id}"
	Auth    AuthReq // identity the gateway must establish before dispatch
	Success int     // HTTP status the gateway writes on a StatusOK outcome (e.g. 201/200/204)
}

// HTTPBind is the per-method HTTP-surface declaration rpcgen reads (from a
// `var HTTPBindings map[string]HTTPBind` in a provider's <module>api package,
// keyed by Go METHOD name) to GENERATE the gateway binding for that method. It is
// the single source that tells the generator how a capability method maps onto an
// HTTP route: the verb/path/auth/success (which become the Operation) plus where
// each method argument is sourced from — a path wildcard or the request body — so
// the generated Decode builds the SAME wire request envelope both LocalBackend and
// RemoteBackend consume. Declaring it in the transport-free api package keeps ONE
// source of truth; the generated glue makes Local == Remote by construction.
type HTTPBind struct {
	Verb    string  // HTTP verb, e.g. "POST"
	Path    string  // HTTP path pattern, e.g. "/characters/{id}"
	Auth    AuthReq // AuthNone or AuthPlayer
	Success int     // HTTP status on a StatusOK outcome — a plain int literal (201/200/204/202)
	// PathArgs maps an interface method PARAM NAME to the path-wildcard name it is
	// taken from, e.g. {"characterID": "id"} for Delete(ctx, characterID) bound to
	// "/characters/{id}". A param not listed here is a BODY arg.
	PathArgs map[string]string
	// BodyNames overrides the external JSON key of a BODY arg where it differs from
	// the param name, e.g. {"itemID": "item_id"} to keep the pre-migration public
	// body shape. An unlisted body arg uses its param name as the JSON key. This
	// key becomes the wire request envelope field's JSON tag, so a plain
	// json.Unmarshal of the external body populates it and a RemoteBackend
	// re-marshal reaches the peer in the identical shape.
	BodyNames map[string]string
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

// OpBinding carries the per-operation, topology-independent glue the gateway
// needs to translate an HTTP request into the operation's typed request and to
// allocate its typed response. The provider module contributes one per op; the
// gateway pairs it with the Operation (matched by Method) and the selected
// backend.
//
// It is deliberately transport-free (no net/http): the gateway extracts the raw
// request body and the matched path-wildcard values and hands them here, so the
// module owns ONLY the typed-shape knowledge (its request/response structs),
// never HTTP mechanics. This is what keeps the SAME binding usable whether the
// gateway then dispatches over LocalBackend (typed, in-process) or RemoteBackend
// (marshaled over the edge) — the decode happens once at the HTTP boundary.
type OpBinding struct {
	Method string
	// Decode builds the operation's WIRE REQUEST ENVELOPE from the raw HTTP body and
	// the matched path-wildcard values (e.g. {"id": "..."} for "/characters/{id}").
	// body is nil for a request with no body. A malformed body should be returned
	// as an *Error{Status: StatusInvalid}, which the gateway maps to HTTP 400. The
	// returned req is the concrete pointer the LocalInvoker type-asserts AND the
	// exact value RemoteBackend marshals over the edge — the SAME wire envelope both
	// topologies use, so a RemoteBackend re-marshal reaches the peer unchanged.
	Decode func(body []byte, path map[string]string) (req any, err error)
	// NewResp allocates a pointer to the operation's WIRE RESPONSE ENVELOPE (the
	// {status, err, <domain fields>} shape rpcgen generates) for the backend to
	// fill. It is ALWAYS non-nil — every operation, even a 204, has an envelope
	// carrying at least Status/Err. LocalBackend fills it via a typed call;
	// RemoteBackend unmarshals the peer's reply into it; both leave the SAME
	// envelope for Encode to read.
	NewResp func() any
	// Encode reduces a filled wire response envelope to the EXTERNAL HTTP body and
	// the operation Status. On a StatusOK outcome it returns the DOMAIN-ONLY body
	// bytes (dropping status/err — e.g. the bare Character, the []Character, or a
	// {player_id, token}) so the external HTTP contract is unchanged; body is nil
	// for a no-return op (204). On a non-OK outcome it returns (nil, status,
	// *Error{status}) so the gateway maps it to the right HTTP status. The Status
	// return lets a test assert the outcome without re-deriving it from err.
	Encode func(resp any) (body []byte, status Status, err error)
}

// BindingSlot is the contribution slot the gateway reads to pair each Operation
// with its HTTP↔typed translation. A provider contributes with
// ctx.Contribute(opsapi.BindingSlot, OpBinding{...}); the gateway reads
// ctx.Contributions(opsapi.BindingSlot). Contributed by the module in the SAME
// process as the Operation, so it is always present wherever the route is bound.
const BindingSlot = "ops.binding"

// OpSet bundles the three per-operation contributions rpcgen generates for a
// bound method — the Operation (route/auth/success), its OpBinding (Decode/
// NewResp/Encode), and the LocalOp (in-process invoker) — so a module's ops.go
// contributes them in one loop instead of hand-writing each. rpcgen emits a
// `func Operations(impl I) map[string]OpSet` (keyed by wire method name) in the
// <module>rpc package; the module reads it and contributes each set to the three
// slots, selecting which methods to expose when they are conditionally enabled.
type OpSet struct {
	Operation Operation
	Binding   OpBinding
	Local     LocalOp
}

// RouteBinding is the IMPL-FREE subset of an operation's gateway wiring: the
// static Operation (route/auth/success) paired with its OpBinding (the HTTP↔wire
// Decode/NewResp/Encode). Unlike OpSet it carries NO LocalOp — so it needs no
// provider impl to construct, because it only ever dispatches an operation
// REMOTELY (over a RemoteBackend to the owning peer's edge).
//
// This is what cmd/gateway-svc — the dedicated split front-door process that
// hosts NO module and therefore has no ctx.Contributions(opsapi.Slot) to read and
// no service to bind a LocalOp to — builds its route table from. rpcgen emits a
// `func RouteBindings() []RouteBinding` per <module>rpc package (the impl-free
// twin of Operations(impl)); gateway-svc collects them across the split-hosted
// player modules and dispatches each over the edge. In-process hosts keep using
// Operations(impl) (which additionally carries the LocalOp for LocalBackend).
type RouteBinding struct {
	Operation Operation
	Binding   OpBinding
}
