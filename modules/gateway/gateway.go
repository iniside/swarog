// Package gateway is the front-door lifecycle module: it is present in every
// app.Run-based process (the monolith cmd/server and each split svc) and
// Contributes a front-handler to httpmw.FrontHandlerSlot. internal/app composes
// that front around ctx.Mux, so this module fronts the process's HTTP surface
// WITHOUT internal/app importing it — the leaf-slot seam (decision D4 of the
// unified-operation-transport plan). app stays topology-blind and arch-lint's
// `app: mayDependOn` gains no gateway edge.
//
// The front-handler builds an operation route table from opsapi.Slot
// contributions (paired with their opsapi.OpBinding HTTP translation). For a
// matched route it: (1) authenticates once — for an AuthPlayer op it verifies the
// bearer via the accounts Sessions capability and injects the resolved player_id
// into ctx (opsapi.WithPlayerID); (2) decodes the HTTP body + path values into the
// operation's typed request via the module-supplied binding; (3) dispatches
// through the topology-correct OperationBackend (LocalBackend direct call, or
// RemoteBackend over the edge — identity rides ctx locally / the envelope on the
// wire); (4) maps the opsapi.Status outcome onto the HTTP status and encodes the
// typed response. Everything the table does not match falls through to ctx.Mux
// unchanged (HTTP-native routes: OAuth, admin HTML, webui, health/metrics).
//
// NOTE: this is a DIFFERENT package from the transport-level `gamebackend/gateway`
// (the QUIC prefix router + HTTP reverse proxy used by cmd/gateway-svc, imported
// here as `transport` for RemoteBackend's self-healing relay). Reconciling
// cmd/gateway-svc with this module is deferred (plan Phase C/F).
package gateway

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"strings"
	"sync"

	"gamebackend/httpmw"
	"gamebackend/lifecycle"
	"gamebackend/opsapi"
	"gamebackend/registry"
)

// maxBodyBytes caps the request body the gateway reads before decoding an
// operation, so a hostile client cannot make the front-handler buffer without
// bound. 1 MiB is comfortably above any player operation's request.
const maxBodyBytes = 1 << 20

// Module is the front-door lifecycle module. It is stateless: everything it needs
// (the route table, the Sessions verifier) is read from the lifecycle Context the
// first time a request arrives.
type Module struct{}

func (*Module) Name() string       { return "gateway" }
func (*Module) Requires() []string { return nil }

// sessionVerifier is the slice of the accounts capability the gateway needs to
// authenticate a bearer (consumer-defined interface — CLAUDE.md rule 4; it does
// not import accountsapi).
type sessionVerifier interface {
	VerifySession(ctx context.Context, token string) (playerID string, ok bool, err error)
}

// Init contributes the front-handler to the leaf slot internal/app reads. No I/O.
//
// The operation route table is built LAZILY, on the first request — NOT here —
// because Init runs in module-registration order and the gateway is registered
// first, so the provider modules have not yet contributed their Operations when
// gateway.Init runs. By the first request every module's Init (where the
// contributions happen) has completed, so the slots are fully populated. Three
// slots feed the substrate:
//   - opsapi.Slot        — one Operation per exposed capability (verb/path/auth/success).
//   - opsapi.BindingSlot — one OpBinding per op (HTTP body/path → typed req, resp alloc).
//   - opsapi.LocalSlot   — one LocalOp per in-process capability (the typed invoker
//     LocalBackend calls).
func (*Module) Init(ctx *lifecycle.Context) error {
	ctx.Contribute(httpmw.FrontHandlerSlot, frontHandler(ctx))
	return nil
}

// localInvokers collects the in-process operation invokers contributed to
// opsapi.LocalSlot into a method-name map for LocalBackend.
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

// opBindings collects the per-operation HTTP↔typed bindings contributed to
// opsapi.BindingSlot into a method-name map.
func opBindings(ctx *lifecycle.Context) map[string]opsapi.OpBinding {
	m := map[string]opsapi.OpBinding{}
	for _, c := range ctx.Contributions(opsapi.BindingSlot) {
		b, ok := c.(opsapi.OpBinding)
		if !ok {
			continue
		}
		m[b.Method] = b
	}
	return m
}

// buildOpsMux builds the operation route table as an *http.ServeMux keyed by each
// Operation's "VERB /path" pattern. net/http's own pattern matcher gives us
// {id}-style path params for free (the same "DELETE /characters/{id}" shape the
// hand-written handlers used), so the gateway matches routes exactly as the mux
// behind it would. Each route is paired with its OpBinding (HTTP translation) and
// the topology-correct backend; an Operation with no binding is a provider wiring
// bug and is skipped rather than bound to an undecodable route.
func buildOpsMux(ctx *lifecycle.Context, local *LocalBackend, sessions sessionVerifier) *http.ServeMux {
	bindings := opBindings(ctx)
	mux := http.NewServeMux()
	for _, c := range ctx.Contributions(opsapi.Slot) {
		op, ok := c.(opsapi.Operation)
		if !ok {
			continue
		}
		binding, ok := bindings[op.Method]
		if !ok {
			continue
		}
		backend := selectBackend(ctx, op, local)
		mux.Handle(op.Verb+" "+op.Path, newOpHandler(op, binding, backend, sessions))
	}
	return mux
}

// selectBackend picks the topology-correct backend for an operation: LocalBackend
// when the operation's provider service is registered in THIS process (monolith,
// or a split peer that co-hosts it — a direct typed call, zero network hop), else
// a RemoteBackend to the owning peer.
//
// SEAM (Phase C/D): peerAddrFor resolves the owning peer's edge address (from the
// split's peer/ROLES config). No peer addressing is wired yet; in every current
// topology the provider of a bound op is co-hosted with its gateway front, so the
// LocalBackend branch is always taken and this remote branch is wired in shape only.
func selectBackend(ctx *lifecycle.Context, op opsapi.Operation, local *LocalBackend) OperationBackend {
	provider := providerOf(op.Method)
	if _, ok := registry.TryRequire[any](ctx.Registry, provider); ok {
		return local
	}
	return NewRemoteBackend(peerAddrFor(provider))
}

// providerOf derives the provider service name from a method name: the segment
// before the first "." (e.g. "characters.create" → "characters"), which is the
// name the provider registered its service under.
func providerOf(method string) string {
	if i := strings.IndexByte(method, '.'); i >= 0 {
		return method[:i]
	}
	return method
}

// peerAddrFor returns the QUIC edge address of the peer that owns provider.
// SEAM (Phase C/D): split peer addressing is not wired yet; it returns "" so the
// (never-reached, co-hosted) remote branch has a defined, non-fabricated value.
func peerAddrFor(_ string) string { return "" }

// newOpHandler is the per-operation HTTP handler the route table binds: it
// authenticates (for AuthPlayer), decodes the request, dispatches through the
// backend, and encodes the outcome — the integration point that turns a typed
// operation into an HTTP endpoint.
func newOpHandler(op opsapi.Operation, binding opsapi.OpBinding, backend OperationBackend, sessions sessionVerifier) http.Handler {
	wildcards := pathWildcards(op.Path)
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		ctx := r.Context()

		// (1) Authenticate once, at the front door. For an AuthPlayer op the
		// verified player_id is injected into ctx (opsapi.WithPlayerID); the backend
		// (and, over the wire, the generated adapter) reads it from ctx — the domain
		// operation never sees a client-supplied identity.
		if op.Auth == opsapi.AuthPlayer {
			pid, ok := authenticate(w, r, sessions)
			if !ok {
				return
			}
			ctx = opsapi.WithPlayerID(ctx, pid)
		}

		// (2) Decode: raw body (bounded) + matched path wildcard values → typed req.
		body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, maxBodyBytes))
		if err != nil {
			http.Error(w, "request body too large", http.StatusRequestEntityTooLarge)
			return
		}
		var req any
		if binding.Decode != nil {
			path := make(map[string]string, len(wildcards))
			for _, name := range wildcards {
				path[name] = r.PathValue(name)
			}
			if req, err = binding.Decode(body, path); err != nil {
				writeOpError(w, err)
				return
			}
		}

		// (3) Dispatch through the topology-correct backend.
		var resp any
		if binding.NewResp != nil {
			resp = binding.NewResp()
		}
		if err := backend.Invoke(ctx, op, req, resp); err != nil {
			writeOpError(w, err)
			return
		}

		// (4) Encode the success outcome: the op's success code, plus the typed
		// response body when the op has one (a 204 op contributes a nil NewResp).
		if resp != nil {
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(op.Success)
			_ = json.NewEncoder(w).Encode(resp)
			return
		}
		w.WriteHeader(op.Success)
	})
}

// authenticate verifies the request's bearer token via the Sessions capability,
// returning the resolved player_id. It writes the failure response itself (503 if
// the verifier is absent/unreachable, 401 if the token is missing or invalid) and
// returns ok=false once it has responded.
func authenticate(w http.ResponseWriter, r *http.Request, sessions sessionVerifier) (string, bool) {
	if sessions == nil {
		http.Error(w, "auth service unavailable", http.StatusServiceUnavailable)
		return "", false
	}
	token := bearer(r)
	if token == "" {
		http.Error(w, "unauthorized", http.StatusUnauthorized)
		return "", false
	}
	pid, ok, err := sessions.VerifySession(r.Context(), token)
	if err != nil {
		http.Error(w, "auth service unavailable", http.StatusServiceUnavailable)
		return "", false
	}
	if !ok {
		http.Error(w, "unauthorized", http.StatusUnauthorized)
		return "", false
	}
	return pid, true
}

// writeOpError maps an operation error's opsapi.Status onto the HTTP status and
// writes it. A plain (non-*Error) error maps to StatusInternal → 500.
func writeOpError(w http.ResponseWriter, err error) {
	http.Error(w, err.Error(), httpStatus(opsapi.StatusOf(err)))
}

// httpStatus maps the operation error taxonomy onto HTTP status codes.
func httpStatus(s opsapi.Status) int {
	switch s {
	case opsapi.StatusOK:
		return http.StatusOK
	case opsapi.StatusNotFound:
		return http.StatusNotFound
	case opsapi.StatusForbidden:
		return http.StatusForbidden
	case opsapi.StatusInvalid:
		return http.StatusBadRequest
	case opsapi.StatusUnavailable:
		return http.StatusServiceUnavailable
	case opsapi.StatusInternal:
		return http.StatusInternalServerError
	default:
		return http.StatusInternalServerError
	}
}

// bearer extracts the token from an "Authorization: Bearer <token>" header, or ""
// if absent — the same shape the deleted per-module inline auth used, now in ONE
// place (the front door).
func bearer(r *http.Request) string {
	if after, found := strings.CutPrefix(r.Header.Get("Authorization"), "Bearer "); found {
		return after
	}
	return ""
}

// pathWildcards extracts the wildcard names from a net/http route pattern, e.g.
// "/characters/{id}" → ["id"] (a trailing "..." matcher is stripped). The gateway
// reads each via r.PathValue and hands them to the op's binding.
func pathWildcards(pattern string) []string {
	var names []string
	for {
		i := strings.IndexByte(pattern, '{')
		if i < 0 {
			break
		}
		rest := pattern[i+1:]
		j := strings.IndexByte(rest, '}')
		if j < 0 {
			break
		}
		names = append(names, strings.TrimSuffix(rest[:j], "..."))
		pattern = rest[j+1:]
	}
	return names
}

// frontHandler returns the process's HTTP front, composed around ctx.Mux by
// internal/app. The operation route table is built once, lazily, on the first
// request (sync.Once) — reading the now-fully-populated slots from ctx — so it
// captures every provider module's Operation regardless of Init order. It then
// matches each request against that table and serves a matched operation via its
// backend; everything else falls through to the wrapped mux unchanged
// (HTTP-native routes: OAuth, admin HTML, webui, health/metrics).
func frontHandler(ctx *lifecycle.Context) func(http.Handler) http.Handler {
	var (
		once sync.Once
		ops  *http.ServeMux
	)
	build := func() {
		sessions, _ := registry.TryRequire[sessionVerifier](ctx.Registry, "accounts")
		local := NewLocalBackend(localInvokers(ctx))
		ops = buildOpsMux(ctx, local, sessions)
	}
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			once.Do(build)
			// ServeMux.Handler reports the matched pattern ("" when nothing matches).
			// A non-empty pattern means the gateway owns this route; otherwise pass
			// through to the wrapped mux.
			if _, pattern := ops.Handler(r); pattern != "" {
				ops.ServeHTTP(w, r)
				return
			}
			next.ServeHTTP(w, r)
		})
	}
}
