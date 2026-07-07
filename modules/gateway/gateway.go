// Package gateway is the front-door lifecycle module: it is present in every
// app.Run-based process (the monolith cmd/server and each split svc) and
// Contributes a front-handler to httpmw.FrontHandlerSlot. internal/app composes
// that front around ctx.Mux, so this module fronts the process's HTTP surface
// WITHOUT internal/app importing it — the leaf-slot seam (decision D4 of the
// unified-operation-transport plan). app stays topology-blind and arch-lint's
// `app: mayDependOn` gains no gateway edge.
//
// On Step B2 the front-handler builds an operation route table from
// opsapi.Slot contributions — still EMPTY (no operation declares a binding yet),
// so the front is effectively PURE PASSTHROUGH: every request falls through to
// the wrapped mux unchanged and behavior is identical to not having a front at
// all. This step lands the operation-dispatch substrate (OperationBackend, its
// Local/Remote impls, the route table, topology selection) so a later phase (D)
// can migrate real operations onto it by contributing bindings + invokers, with
// no further change to this module's wiring.
//
// NOTE: this is a DIFFERENT package from the transport-level `gamebackend/gateway`
// (the QUIC prefix router + HTTP reverse proxy used by cmd/gateway-svc, imported
// here as `transport` for RemoteBackend's self-healing relay). Reconciling
// cmd/gateway-svc with this module is deferred (plan Phase C/F).
package gateway

import (
	"net/http"
	"strings"

	"gamebackend/httpmw"
	"gamebackend/lifecycle"
	"gamebackend/opsapi"
	"gamebackend/registry"
)

// Module is the front-door lifecycle module. It holds the operation route table +
// the local dispatch backend built in Init, so it uses a pointer receiver.
type Module struct{}

func (*Module) Name() string       { return "gateway" }
func (*Module) Requires() []string { return nil }

// Init builds the operation route table from the slots and contributes the
// front-handler to the leaf slot internal/app reads. No I/O.
//
// Two slots feed the substrate (both empty until Phase D):
//   - opsapi.Slot     — one Operation per exposed capability (the HTTP binding).
//   - opsapi.LocalSlot — one LocalOp per in-process capability (the typed invoker
//     LocalBackend calls). Read here into the LocalBackend's dispatch map.
func (*Module) Init(ctx *lifecycle.Context) error {
	local := NewLocalBackend(localInvokers(ctx))
	ops := buildOpsMux(ctx, local)
	ctx.Contribute(httpmw.FrontHandlerSlot, frontHandler(ops))
	return nil
}

// localInvokers collects the in-process operation invokers contributed to
// opsapi.LocalSlot into a method-name map for LocalBackend. Empty on Step B2.
func localInvokers(ctx *lifecycle.Context) map[string]opsapi.LocalInvoker {
	m := map[string]opsapi.LocalInvoker{}
	for _, c := range ctx.Contributions(opsapi.LocalSlot) {
		lo, ok := c.(opsapi.LocalOp)
		if !ok {
			continue
		}
		m[lo.Method] = lo.Invoke
	}
	return m
}

// buildOpsMux builds the operation route table as an *http.ServeMux keyed by the
// each Operation's "VERB /path" pattern. net/http's own pattern matcher gives us
// {id}-style path params for free (the same "DELETE /characters/{id}" shape the
// hand-written handlers use today), so the gateway matches routes exactly as the
// mux behind it would. EMPTY on Step B2 (no Operation contributed), so the
// front-handler never matches and always falls through.
func buildOpsMux(ctx *lifecycle.Context, local *LocalBackend) *http.ServeMux {
	mux := http.NewServeMux()
	for _, c := range ctx.Contributions(opsapi.Slot) {
		op, ok := c.(opsapi.Operation)
		if !ok {
			continue
		}
		backend := selectBackend(ctx, op, local)
		mux.Handle(op.Verb+" "+op.Path, newOpHandler(op, backend))
	}
	return mux
}

// selectBackend picks the topology-correct backend for an operation: LocalBackend
// when the operation's provider service is registered in THIS process (monolith,
// or a split peer that co-hosts it — a direct typed call, zero network hop), else
// a RemoteBackend to the owning peer.
//
// SEAM (Phase C/D): peerAddrFor resolves the owning peer's edge address (from the
// split's peer/ROLES config). No peer addressing exists yet, and with an empty
// route table this remote branch is never reached in B2 — it is wired in shape
// only, deliberately NOT hardcoding any peer address.
func selectBackend(ctx *lifecycle.Context, op opsapi.Operation, local *LocalBackend) OperationBackend {
	provider := providerOf(op.Method)
	if _, ok := registry.TryRequire[any](ctx.Registry, provider); ok {
		return local
	}
	return NewRemoteBackend(peerAddrFor(provider))
}

// providerOf derives the provider service name from a method name: the segment
// before the first "." (e.g. "characters.ownerOf" → "characters"), which is the
// name the provider registered its service under.
func providerOf(method string) string {
	if i := strings.IndexByte(method, '.'); i >= 0 {
		return method[:i]
	}
	return method
}

// peerAddrFor returns the QUIC edge address of the peer that owns provider.
// SEAM (Phase C/D): split peer addressing is not wired yet; it returns "" so the
// (never-reached, empty-table) remote branch has a defined, non-fabricated value.
func peerAddrFor(_ string) string { return "" }

// newOpHandler is the per-operation HTTP handler the route table binds. It is the
// integration point where Phase D wires the decode → backend.Invoke → encode
// flow (decode the HTTP body into the operation's typed request, invoke the
// backend, map an opsapi.Status error onto the HTTP status, encode the typed
// response). On Step B2 no operation is bound, so this is never invoked; it
// answers 501 to make an accidental early binding loud rather than silent.
func newOpHandler(op opsapi.Operation, backend OperationBackend) http.Handler {
	_ = backend // wired into the decode/encode flow in Phase D
	return http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		http.Error(w, "operation dispatch not wired yet (Phase D): "+op.Method, http.StatusNotImplemented)
	})
}

// frontHandler returns the process's HTTP front, composed around ctx.Mux by
// internal/app. It matches each request against the operation route table (ops)
// and serves a matched operation via its backend; everything else falls through
// to the wrapped mux unchanged. On Step B2 the table is EMPTY, so every request
// falls through — behavior is identical to not having a front at all.
func frontHandler(ops *http.ServeMux) func(http.Handler) http.Handler {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			// ServeMux.Handler reports the matched pattern ("" when nothing matches).
			// A non-empty pattern means the gateway owns this route; otherwise pass
			// through. Empty table on B2 ⇒ pattern is always "" ⇒ always next.
			if _, pattern := ops.Handler(r); pattern != "" {
				ops.ServeHTTP(w, r)
				return
			}
			next.ServeHTTP(w, r)
		})
	}
}
