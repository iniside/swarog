// Package app is the reusable boot sequence shared by every service
// entrypoint. Each cmd/<service>/main.go builds its OWN static list of modules
// (importing only that service's code — the Go linker then drops every module a
// binary never names) and hands it to Run. Run owns the machinery that used to
// live inline in cmd/server: open the DB, wire the Context, two-phase Build,
// Migrate, Start, the HTTP server, an optional QUIC edge listener, and graceful
// shutdown. It knows NOTHING about which modules exist — the entrypoint decides
// the topology by choosing what to import and pass in.
package app

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"time"

	_ "github.com/jackc/pgx/v5/stdlib" // registers the "pgx" database/sql driver

	"golang.org/x/time/rate"

	"gamebackend/edge"
	"gamebackend/httpmw"
	"gamebackend/lifecycle"
	"gamebackend/metrics"
)

const defaultDSN = "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable"

// Config is the process-level configuration Run needs. It is deliberately tiny:
// everything module-specific (EVENTS_SUBSCRIBERS, peer edge addrs, admin URLs,
// EPIC_*) is read by the module that owns it, not here.
type Config struct {
	DatabaseURL string // Postgres DSN
	ListenAddr  string // HTTP listen address, e.g. ":8080"
	EdgeAddr    string // QUIC edge listen address, e.g. ":9000" (only used when an edge server is passed to Run)
}

// ConfigFromEnv reads the standard process env (DATABASE_URL, PORT, EDGE_ADDR)
// into a Config, applying the same defaults the monolith used. Both ":8080" and
// "8080" forms of PORT are accepted.
func ConfigFromEnv() Config {
	dsn := os.Getenv("DATABASE_URL")
	if dsn == "" {
		dsn = defaultDSN
	}
	edgeAddr := strings.TrimSpace(os.Getenv("EDGE_ADDR"))
	if edgeAddr == "" {
		edgeAddr = ":9000"
	}
	return Config{
		DatabaseURL: dsn,
		ListenAddr:  normalizeAddr(os.Getenv("PORT")),
		EdgeAddr:    edgeAddr,
	}
}

// normalizeAddr accepts both ":8080" and "8080" forms and returns ":8080".
func normalizeAddr(port string) string {
	port = strings.TrimSpace(port)
	if port == "" {
		return ":8080"
	}
	if strings.HasPrefix(port, ":") {
		return port
	}
	return ":" + port
}

// Run boots a service from a static list of modules. It opens the DB, wires the
// lifecycle Context, mounts /healthz, then Build → validate Requires → Migrate →
// Start; if edgeServer is non-nil it also brings up the QUIC listener (after
// Build, so every module has registered its handlers). It blocks until SIGINT,
// then shuts down in order: HTTP → edge → drain bus → Stop modules.
//
// mods is the WHOLE topology of this process: the real modules it hosts plus any
// remote stubs standing in for peers. edgeServer is nil for an all-local process
// (the monolith) and non-nil only when this process exposes edge-backed services.
func Run(cfg Config, mods []lifecycle.Module, edgeServer *edge.Server) error {
	log := slog.New(slog.NewTextHandler(os.Stdout, nil))
	ctx := lifecycle.NewContext(log)

	db, err := sql.Open("pgx", cfg.DatabaseURL)
	if err != nil {
		return fmt.Errorf("open db: %w", err)
	}
	defer func() { _ = db.Close() }()
	pingCtx, cancelPing := context.WithTimeout(context.Background(), 5*time.Second)
	if err := db.PingContext(pingCtx); err != nil {
		cancelPing()
		return fmt.Errorf("db unreachable: %w", err)
	}
	cancelPing()
	ctx.DB = db

	// /healthz is pure liveness — infra, always mounted, in every service. It
	// answers 200 unconditionally with NO dependency check: a k8s liveness probe
	// restarts the process on failure, and restarting the process cannot fix a
	// down database, so pinging it here only causes a needless restart loop.
	// Registered before anything else so it's always up.
	ctx.Mux.HandleFunc("GET /healthz", healthzHandler)

	// /readyz is readiness — it DOES check dependencies, because readiness
	// controls whether a load balancer sends traffic here, not whether the
	// process is restarted. Baseline check is the DB ping; any module with its
	// own dependency can contribute a func(context.Context) error to
	// httpmw.ReadinessSlot and it's picked up here with no edit to this file.
	// Contributions are read lazily, per request — by request time every
	// module's Init (where Contribute calls happen) has already run in Build.
	ctx.Mux.HandleFunc("GET /readyz", readyzHandler(ctx, db))

	// /metrics exposes this process's own Prometheus registry (metrics.Handler),
	// isolated from other processes' — mounted on ctx.Mux like /healthz/readyz so
	// it goes through metrics.Middleware below (counts its own scrape, benign) but
	// is exempted from rate limiting by httpmw.SkipInfra.
	ctx.Mux.Handle("GET /metrics", metrics.Handler())

	// Fail loud if this process's module set is internally incoherent: every
	// module's Requires() must be satisfied by a provider (a real module OR a
	// remote stub) also present in mods. This replaces the old ROLES stub-planner
	// — the entrypoint now hand-picks the list, so we only assert it's complete.
	if err := validateRequires(mods); err != nil {
		return err
	}

	appl := lifecycle.NewApp(ctx)
	for _, m := range mods {
		appl.Add(m)
	}
	if err := appl.Build(); err != nil {
		return fmt.Errorf("startup failed: %w", err)
	}

	migCtx, cancelMig := context.WithTimeout(context.Background(), 30*time.Second)
	if err := appl.Migrate(migCtx, db); err != nil {
		cancelMig()
		return fmt.Errorf("migrate failed: %w", err)
	}
	cancelMig()

	startCtx, cancelStart := context.WithTimeout(context.Background(), 10*time.Second)
	if err := appl.Start(startCtx); err != nil {
		cancelStart()
		return fmt.Errorf("start failed: %w", err)
	}
	cancelStart()

	// Bring up the shared edge server AFTER every module Init has registered its
	// handlers (Init ran in appl.Build). One listener, all edge methods.
	if edgeServer != nil {
		// Mutual TLS: the server presents a CA-signed leaf AND requires the client to
		// present one too (SharedDevCA resolves the shared anchor from EDGE_CA_CERT/
		// EDGE_CA_KEY, or generates+warns in dev). This is what makes a later trusted-
		// identity envelope safe — an unauthenticated peer cannot reach an edge method.
		ca, err := edge.SharedDevCA(log)
		if err != nil {
			return fmt.Errorf("edge ca: %w", err)
		}
		tlsConf, err := ca.ServerTLS()
		if err != nil {
			return fmt.Errorf("edge tls: %w", err)
		}
		if err := edgeServer.ListenAddr(cfg.EdgeAddr, tlsConf); err != nil {
			return fmt.Errorf("edge listen: %w", err)
		}
		log.Info("edge listening (mutual TLS)", "addr", edgeServer.Addr())
	}

	// Cross-cutting HTTP middleware wraps the WHOLE mux after every module has
	// mounted. Rate limiting here is OPT-IN (default OFF): a split deployment runs
	// characters-svc/inventory-svc BEHIND the gateway, so limiting there would
	// double-count and collapse every client into the gateway's single bucket. The
	// gateway (front door) always limits; the monolith turns this on via env.
	// Front-handler slot: a module (the gateway) contributes a
	// func(http.Handler) http.Handler to httpmw.FrontHandlerSlot; each is composed
	// around ctx.Mux HERE, so the gateway fronts the process's HTTP surface WITHOUT
	// this package importing the gateway module — app reads only the leaf slot name
	// (mirrors the ReadinessSlot read in /readyz). Read lazily after Build, so every
	// module's Init (where Contribute happens) has run. Contributions are asserted to
	// the bare func type (stdlib-only slot value); a bad assertion is skipped, not
	// fatal — a misregistered contribution is that module's bug, not grounds to boot
	// without a front. The front is composed INNERMOST of the three wraps (front →
	// rate-limit → metrics, below): it wraps ctx.Mux directly, so once the gateway
	// intercepts routes (later phases) they are still rate-limited and metrics-counted
	// like any other request — metrics stays the outermost wrap counting final status,
	// consistent with today. On Step B1 the contributed front is pure passthrough, so
	// this is a byte-for-byte no-op.
	var handler http.Handler = ctx.Mux
	for _, contribution := range ctx.Contributions(httpmw.FrontHandlerSlot) {
		front, ok := contribution.(func(http.Handler) http.Handler)
		if !ok {
			continue
		}
		handler = front(handler)
	}
	if rps := envFloat("RATE_LIMIT_RPS", 0); rps > 0 {
		trusted, err := httpmw.ParseCIDRs(os.Getenv("TRUSTED_PROXY_CIDRS"))
		if err != nil {
			return fmt.Errorf("parse TRUSTED_PROXY_CIDRS: %w", err)
		}
		burst := envInt("RATE_LIMIT_BURST", 40)
		lim := httpmw.NewIPLimiter(rate.Limit(rps), burst)
		handler = httpmw.RateLimit(lim, httpmw.ClientIP(trusted), httpmw.SkipInfra, handler)
		log.Info("http rate limiting enabled", "rps", rps, "burst", burst, "trusted_cidrs", len(trusted))
	} else {
		log.Warn("http rate limiting disabled (RATE_LIMIT_RPS<=0) — expected for a service behind the gateway; set >0 on the monolith")
	}
	// Metrics wrap the OUTSIDE of the rate limiter — metrics(ratelimit(mux)) — so a
	// 429 the limiter issues is still counted as a request with that status. The
	// gateway does NOT get this middleware (stays lean, no prometheus import).
	handler = metrics.Middleware(handler)

	srv := &http.Server{Addr: cfg.ListenAddr, Handler: handler, ReadHeaderTimeout: 10 * time.Second}
	go func() {
		log.Info("listening", "addr", srv.Addr)
		if err := srv.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			log.Error("server stopped", "err", err)
			os.Exit(1)
		}
	}()

	// Graceful shutdown, in order:
	//   1. stop accepting HTTP (no new events get published),
	//   2. close the edge listener (no new cross-process calls),
	//   3. drain the bus (in-flight events finish while module resources are up),
	//   4. stop modules in reverse registration order (close goroutines/resources).
	//      In a process hosting the durable plane, `messaging` is registered LAST
	//      (cmd/*), so it is the FIRST module to Stop here: its relay + LISTEN
	//      loop halt delivery, and Stop blocks until any in-flight per-subscriber
	//      `consume` finishes, before any producer/consumer module's own Stop
	//      tears down the resources that consume might still be using.
	stop := make(chan os.Signal, 1)
	signal.Notify(stop, os.Interrupt)
	<-stop
	log.Info("shutting down")

	shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := srv.Shutdown(shutdownCtx); err != nil {
		log.Error("http shutdown", "err", err)
	}
	if edgeServer != nil {
		if err := edgeServer.Close(); err != nil {
			log.Error("edge shutdown", "err", err)
		}
	}
	ctx.Bus.Close()
	appl.Stop(shutdownCtx)
	log.Info("bye")
	return nil
}

// healthzHandler answers liveness: always 200, no dependency check (see the
// call site's comment for why a restart can't fix a down DB).
func healthzHandler(w http.ResponseWriter, _ *http.Request) {
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write([]byte("ok"))
}

// readyzHandler builds the /readyz handler: a 2s-budgeted DB ping (baseline,
// always run) plus every func(context.Context) error contributed to
// httpmw.ReadinessSlot. Any failure — the ping or any contributed check —
// yields 503 with a JSON body mapping a check name to its error; all green
// yields 200. Contributions are asserted to the bare func type per the plan
// (stdlib-only slot value, no httpmw import required of contributors); a
// contribution that fails the assertion is skipped rather than panicking, since
// a misregistered contribution is a bug in that module, not grounds to crash
// every request to this endpoint.
func readyzHandler(ctx *lifecycle.Context, db *sql.DB) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		checkCtx, cancel := context.WithTimeout(r.Context(), 2*time.Second)
		defer cancel()

		failures := map[string]string{}
		if err := db.PingContext(checkCtx); err != nil {
			failures["db"] = err.Error()
		}
		for i, contribution := range ctx.Contributions(httpmw.ReadinessSlot) {
			check, ok := contribution.(func(context.Context) error)
			if !ok {
				continue
			}
			if err := check(checkCtx); err != nil {
				failures[fmt.Sprintf("readiness[%d]", i)] = err.Error()
			}
		}

		if len(failures) > 0 {
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusServiceUnavailable)
			_ = json.NewEncoder(w).Encode(failures)
			return
		}
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("ok"))
	}
}

// envFloat reads key as a float64, returning def when unset or unparseable.
// Local to app per the repo convention of duplicating env helpers per package
// (no shared envutil).
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

// validateRequires asserts every module's declared Requires() is satisfied by a
// provider present in this process's module set — a real module or a remote
// stub (both report the dependency name from Name()). A gap is a wiring bug in
// the entrypoint's static list, better loud at startup than a Require panic deep
// in Build.
func validateRequires(mods []lifecycle.Module) error {
	present := map[string]struct{}{}
	for _, m := range mods {
		present[m.Name()] = struct{}{}
	}
	for _, m := range mods {
		for _, dep := range m.Requires() {
			if _, ok := present[dep]; !ok {
				return fmt.Errorf("module %q requires %q, but no provider (real module or remote stub) is present in this process", m.Name(), dep)
			}
		}
	}
	return nil
}
