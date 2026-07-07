// Command gateway-svc is the player-facing front door for the microservices
// split. It hosts NO module (no DB, no bus, no lifecycle) and therefore does NOT
// use internal/app.Run: it is a pure transport process. It is the SINGLE gateway —
// external player requests enter here and are dispatched to the owning backend
// over the mutually-authenticated QUIC edge as typed OPERATIONS (via
// gateway.RemoteBackend), NOT HTTP-reverse-proxied to the backends' own gateway
// front-handlers. That collapses the former double-layer (gateway-svc HTTP proxy →
// backend front-handler → op) into ONE hop: gateway-svc → backend edge op.
//
// It builds its operation route table STATICALLY, without any module, from each
// split-hosted player module's generated impl-free RouteBindings()
// (charactersplayerrpc / accountsauthrpc / inventoryrpc). For a matched route it
// authenticates ONCE — an AuthPlayer op's bearer is verified over the edge to the
// accounts peer (accountsrpc.Client) and the resolved player_id rides the op
// envelope — then dispatches the op via RemoteBackend to the owning peer, keyed by
// method prefix: characters.* / accounts.* → characters-svc (accounts is
// co-hosted there), inventory.* → inventory-svc.
//
// The HTTP-NATIVE routes that are not operations stay HTTP reverse-proxy to the
// backend that owns them: admin HTML (/admin* → inventory-svc) and the Epic OAuth
// start/callback (/accounts/epic/* → characters-svc). It also keeps its own
// /healthz+/readyz and per-IP rate limiting, plus the player-facing QUIC edge
// front (:9100, native-client scope) that prefix-routes to the backends.
//
// leaderboard/match ops are monolith-only (no split service hosts them), so
// gateway-svc does NOT route them — their rpc packages are not imported here.
package main

import (
	"context"
	"errors"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"time"

	"golang.org/x/time/rate"

	"gamebackend/edge"
	transport "gamebackend/gateway"
	"gamebackend/httpmw"
	"gamebackend/modules/accounts/accountsauthrpc"
	"gamebackend/modules/accounts/accountsrpc"
	"gamebackend/modules/characters/charactersplayerrpc"
	gwmod "gamebackend/modules/gateway"
	"gamebackend/modules/inventory/inventoryrpc"
	"gamebackend/opsapi"
)

// env returns the trimmed value of key, or def when unset/blank.
func env(key, def string) string {
	if v := strings.TrimSpace(os.Getenv(key)); v != "" {
		return v
	}
	return def
}

// envFloat reads key as a float64, returning def when unset or unparseable.
func envFloat(key string, def float64) float64 {
	v := strings.TrimSpace(os.Getenv(key))
	if v == "" {
		return def
	}
	f, err := strconv.ParseFloat(v, 64)
	if err != nil {
		return def
	}
	return f
}

// envInt reads key as an int, returning def when unset or unparseable.
func envInt(key string, def int) int {
	v := strings.TrimSpace(os.Getenv(key))
	if v == "" {
		return def
	}
	n, err := strconv.Atoi(v)
	if err != nil {
		return def
	}
	return n
}

// normalizeAddr accepts both ":8082" and "8082" forms and returns ":8082".
func normalizeAddr(port string) string {
	port = strings.TrimSpace(port)
	if port == "" {
		return ":8082"
	}
	if strings.HasPrefix(port, ":") {
		return port
	}
	return ":" + port
}

// provider derives the owning service from a wire method name: the segment before
// the first "." (e.g. "characters.create" → "characters"). gateway-svc routes an
// op to its peer by this prefix.
func provider(method string) string {
	if i := strings.IndexByte(method, '.'); i >= 0 {
		return method[:i]
	}
	return method
}

func main() {
	log := slog.New(slog.NewTextHandler(os.Stdout, nil))

	httpAddr := normalizeAddr(os.Getenv("PORT"))
	gatewayEdgeAddr := env("GATEWAY_EDGE_ADDR", ":9100")
	charsEdgeAddr := env("CHARACTERS_EDGE_ADDR", "localhost:9000")
	invEdgeAddr := env("INVENTORY_EDGE_ADDR", "localhost:9001")
	// accounts is co-hosted in characters-svc, so its edge defaults to the
	// characters peer; ACCOUNTS_EDGE_ADDR overrides it if the two ever split.
	accEdgeAddr := env("ACCOUNTS_EDGE_ADDR", charsEdgeAddr)
	charsHTTP := env("CHARACTERS_HTTP_ADDR", "localhost:8080")
	invHTTP := env("INVENTORY_HTTP_ADDR", "localhost:8081")

	// One self-healing relay per backend peer, shared across BOTH the op-dispatch
	// front door (RemoteBackend, below) and the QUIC prefix router. accRouted is
	// the characters peer unless accounts is addressed separately.
	charsRouted := transport.NewRoutedBackend(charsEdgeAddr)
	invRouted := transport.NewRoutedBackend(invEdgeAddr)
	accRouted := charsRouted
	if accEdgeAddr != charsEdgeAddr {
		accRouted = transport.NewRoutedBackend(accEdgeAddr)
	}

	// --- HTTP operation front door (the single gateway over the edge) ---------
	// Collect the impl-free route bindings from every split-hosted player module.
	// This couples gateway-svc to the operation vocabulary (acceptable — it is the
	// front door). accounts register/login/me + characters + inventory ops only;
	// leaderboard/match are monolith-only and deliberately absent.
	var routes []opsapi.RouteBinding
	routes = append(routes, charactersplayerrpc.RouteBindings()...)
	routes = append(routes, accountsauthrpc.RouteBindings()...)
	routes = append(routes, inventoryrpc.RouteBindings()...)

	// One RemoteBackend per peer, sharing that peer's self-healing edge conn. An op
	// is dispatched to the peer that owns its method prefix.
	charsBackend := gwmod.NewRemoteBackendRelay(charsRouted.ForwardID)
	invBackend := gwmod.NewRemoteBackendRelay(invRouted.ForwardID)
	backendFor := func(op opsapi.Operation) gwmod.OperationBackend {
		if provider(op.Method) == "inventory" {
			return invBackend
		}
		return charsBackend // characters.* and accounts.* are co-hosted in characters-svc
	}

	// Auth-once over the edge: an AuthPlayer op's bearer is verified by calling
	// accounts.verifySession on the accounts peer through the SAME self-healing edge
	// conn used for accounts op dispatch. accountsrpc.Client satisfies
	// gateway.SessionVerifier structurally.
	var sessions gwmod.SessionVerifier = accountsrpc.NewClient(accRouted)

	opsMux := gwmod.NewOpsMux(routes, backendFor, sessions)

	// HTTP-native passthrough (NOT operations): admin HTML lives in inventory-svc;
	// the Epic OAuth start/callback are HTTP-native and live in characters-svc. These
	// cannot be edge ops (they are HTTP-shaped: HTML, browser redirects), so they
	// stay HTTP reverse-proxy to the owning backend. /characters and /inventory are
	// NO LONGER proxied — they are edge ops now.
	proxy := transport.NewHTTPProxy(map[string]string{
		"/admin":         invHTTP,
		"/accounts/epic": charsHTTP,
	})
	mux := http.NewServeMux()
	mux.HandleFunc("GET /healthz", func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("ok"))
	})
	// The gateway has no DB and hosts no module, so readiness here just means "the
	// process is up and serving" — the same as liveness.
	mux.HandleFunc("GET /readyz", func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("ok"))
	})
	// Anything the op table does not own falls through to the HTTP-native proxy.
	mux.Handle("/", proxy)

	// front: an operation route wins (single-hop edge dispatch); everything else
	// (health/readyz, admin HTML, OAuth) passes to the HTTP-native handler. This is
	// the same match-then-fallthrough shape the in-process gateway module uses.
	front := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if _, pattern := opsMux.Handler(r); pattern != "" {
			opsMux.ServeHTTP(w, r)
			return
		}
		mux.ServeHTTP(w, r)
	})

	// --- player-facing QUIC edge front (native-client scope) ------------------
	// Prefix-route framed player calls to the backend owning each method family.
	// Nothing in the HTTP flow uses this; it is the future native-client transport.
	srv := edge.NewServer()
	srv.HandlePrefix("characters.", charsRouted.Forward)
	srv.HandlePrefix("inventory.", invRouted.Forward)

	// Mutual TLS on the gateway's own edge listener too, from the same process-
	// shared CA (EDGE_CA_CERT/EDGE_CA_KEY). The RoutedBackends above dial the
	// backends with a matching CA-signed client leaf (edge.ClientMTLS).
	ca, err := edge.SharedDevCA(log)
	if err != nil {
		log.Error("edge ca", "err", err)
		os.Exit(1)
	}
	tlsConf, err := ca.ServerTLS()
	if err != nil {
		log.Error("edge tls", "err", err)
		os.Exit(1)
	}
	if err := srv.ListenAddr(gatewayEdgeAddr, tlsConf); err != nil {
		log.Error("edge listen", "err", err)
		os.Exit(1)
	}
	log.Info("gateway edge listening", "addr", srv.Addr())

	// The gateway is the player-facing front door: it ALWAYS rate limits (default
	// 20 rps, burst 40), unlike internal/app where it is opt-in. Same SkipInfra so
	// /healthz is never throttled. Honors X-Forwarded-For only from trusted proxies.
	trusted, err := httpmw.ParseCIDRs(os.Getenv("TRUSTED_PROXY_CIDRS"))
	if err != nil {
		log.Error("parse TRUSTED_PROXY_CIDRS", "err", err)
		os.Exit(1)
	}
	rps := envFloat("RATE_LIMIT_RPS", 20)
	burst := envInt("RATE_LIMIT_BURST", 40)
	lim := httpmw.NewIPLimiter(rate.Limit(rps), burst)
	handler := httpmw.RateLimit(lim, httpmw.ClientIP(trusted), httpmw.SkipInfra, front)
	log.Info("gateway op front door enabled",
		"ops", len(routes), "chars_edge", charsEdgeAddr, "inv_edge", invEdgeAddr, "accounts_edge", accEdgeAddr,
		"rps", rps, "burst", burst, "trusted_cidrs", len(trusted))

	httpSrv := &http.Server{Addr: httpAddr, Handler: handler, ReadHeaderTimeout: 10 * time.Second}
	go func() {
		log.Info("gateway http listening", "addr", httpSrv.Addr)
		if err := httpSrv.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			log.Error("http stopped", "err", err)
			os.Exit(1)
		}
	}()

	// Graceful shutdown, mirroring app.go's order: stop HTTP first (no new
	// inbound), then close the edge listener (no new relayed calls), then the
	// backend relays.
	stop := make(chan os.Signal, 1)
	signal.Notify(stop, os.Interrupt)
	<-stop
	log.Info("shutting down")

	shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := httpSrv.Shutdown(shutdownCtx); err != nil {
		log.Error("http shutdown", "err", err)
	}
	if err := srv.Close(); err != nil {
		log.Error("edge shutdown", "err", err)
	}
	_ = charsRouted.Close()
	_ = invRouted.Close()
	if accRouted != charsRouted {
		_ = accRouted.Close()
	}
	log.Info("bye")
}
