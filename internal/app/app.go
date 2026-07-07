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
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"strings"
	"time"

	_ "github.com/jackc/pgx/v5/stdlib" // registers the "pgx" database/sql driver

	"gamebackend/edge"
	"gamebackend/lifecycle"
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

	// /healthz is infra — always mounted, in every service. 200 once the DB
	// pings, 503 if it's down. Registered before anything else so it's always up.
	ctx.Mux.HandleFunc("GET /healthz", func(w http.ResponseWriter, r *http.Request) {
		pingCtx, cancel := context.WithTimeout(r.Context(), 2*time.Second)
		defer cancel()
		if err := db.PingContext(pingCtx); err != nil {
			http.Error(w, "db unreachable", http.StatusServiceUnavailable)
			return
		}
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("ok"))
	})

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
		tlsConf, err := edge.SelfSignedTLS()
		if err != nil {
			return fmt.Errorf("edge tls: %w", err)
		}
		if err := edgeServer.ListenAddr(cfg.EdgeAddr, tlsConf); err != nil {
			return fmt.Errorf("edge listen: %w", err)
		}
		log.Info("edge listening", "addr", edgeServer.Addr())
	}

	srv := &http.Server{Addr: cfg.ListenAddr, Handler: ctx.Mux, ReadHeaderTimeout: 10 * time.Second}
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
