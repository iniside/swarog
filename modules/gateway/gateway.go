// Package gateway is the front-door lifecycle module: it is present in every
// app.Run-based process (the monolith cmd/server and each split svc) and
// Contributes a front-handler to httpmw.FrontHandlerSlot. internal/app composes
// that front around ctx.Mux, so this module fronts the process's HTTP surface
// WITHOUT internal/app importing it — the leaf-slot seam (decision D4 of the
// unified-operation-transport plan). app stays topology-blind and arch-lint's
// `app: mayDependOn` gains no gateway edge.
//
// On Step B1 the front-handler is PURE PASSTHROUGH: it delegates every request to
// the wrapped mux unchanged, so there is ZERO behavior change — every existing
// route (player HTTP, /admin, /healthz, /events, ...) is served exactly as before
// by ctx.Mux. This step only proves the mount point. Later phases (B2/D) plug the
// operation route table + auth into the marked seam in frontHandler, intercepting
// the operations the gateway owns and passing everything else through.
//
// NOTE: this is a DIFFERENT package from the transport-level `gamebackend/gateway`
// (the QUIC prefix router + HTTP reverse proxy used by cmd/gateway-svc). That
// package holds routing/proxy helpers this module will reuse in a later phase;
// reconciling cmd/gateway-svc with this module is deferred (plan Phase C/F).
package gateway

import (
	"net/http"

	"gamebackend/httpmw"
	"gamebackend/lifecycle"
)

// Module is the front-door lifecycle module. It holds no state today (no route
// table yet), so a value receiver suffices.
type Module struct{}

func (Module) Name() string       { return "gateway" }
func (Module) Requires() []string { return nil }

// Init contributes the front-handler to the leaf slot internal/app reads. No I/O.
func (Module) Init(ctx *lifecycle.Context) error {
	ctx.Contribute(httpmw.FrontHandlerSlot, frontHandler)
	return nil
}

// frontHandler is the process's HTTP front, composed around ctx.Mux by
// internal/app. On Step B1 it is PURE PASSTHROUGH — every request goes straight to
// the wrapped mux, so behavior is identical to not having a front at all.
//
// SEAM (Phase B2/D): the operation route table plugs in HERE. A later phase will
// match r against the gateway's (currently empty) table and serve owned operations
// via the OperationBackend, falling through to next for everything else. Until
// then it only ever calls next.
func frontHandler(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		// SEAM: match r against the operation route table and dispatch owned
		// operations here. Empty on Step B1 ⇒ everything falls through to the mux.
		next.ServeHTTP(w, r)
	})
}
