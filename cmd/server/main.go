package main

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"sort"
	"strings"
	"time"

	_ "github.com/jackc/pgx/v5/stdlib" // registers the "pgx" database/sql driver

	"gamebackend/core"
	"gamebackend/edge"
	"gamebackend/modules/accounts"
	"gamebackend/modules/admin"
	"gamebackend/modules/characters"
	"gamebackend/modules/inventory"
	"gamebackend/modules/leaderboard"
	"gamebackend/modules/match"
	"gamebackend/modules/rating"
	"gamebackend/modules/remote"
	"gamebackend/modules/webui"
)

const defaultDSN = "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable"

// realModules is the ONLY place that knows the full module list. Keyed by
// Name(), it preserves the pointer-vs-value shape each module needs (stateful
// modules use a pointer receiver). ROLES selects which of these this process
// hosts; anything a hosted module DependsOn but that isn't hosted is filled by
// a remote.Stub instead.
func realModules() map[string]core.Module {
	return map[string]core.Module{
		"accounts":    &accounts.Module{},    // pointer: holds db + verifiers
		"characters":  &characters.Module{},  // depends on accounts
		"inventory":   &inventory.Module{},   // depends on accounts + characters
		"rating":      rating.Module{},       // value receiver
		"leaderboard": &leaderboard.Module{}, // pointer: holds db + logger
		"match":       match.Module{},        // value receiver; depends on rating
		"webui":       webui.Module{},        // value receiver
		"admin":       &admin.Module{},       // pointer: holds theme/shell
	}
}

// roleSet is the set of roles this process hosts. The empty/monolith sentinel
// (all=true) means "host everything" — identical to the pre-ROLES behaviour.
// It lives in cmd, NOT core (CLAUDE.md: role logic never touches core/).
type roleSet struct {
	all   bool
	names map[string]struct{}
}

// Has reports whether this process hosts the named role — always true in the
// monolith sentinel, otherwise membership.
func (rs roleSet) Has(name string) bool {
	if rs.all {
		return true
	}
	_, ok := rs.names[name]
	return ok
}

// parseRoles reads ROLES (comma-separated). Empty/unset → monolith sentinel
// (host all modules). Blank entries are skipped; order and duplicates don't
// matter.
func parseRoles(raw string) roleSet {
	if strings.TrimSpace(raw) == "" {
		return roleSet{all: true}
	}
	names := map[string]struct{}{}
	for part := range strings.SplitSeq(raw, ",") {
		if p := strings.TrimSpace(part); p != "" {
			names[p] = struct{}{}
		}
	}
	if len(names) == 0 {
		return roleSet{all: true}
	}
	return roleSet{names: names}
}

// planModules decides which modules this process hosts (real) and which of
// their dependencies must be filled by remote stubs. hosted = role names that
// are real module names; needed = union of hosted modules' DependsOn, minus
// hosted. It fails loudly on an unknown role or a real∩stub overlap rather than
// letting core's Add panic be the error surface.
func planModules(rs roleSet, all map[string]core.Module) (hosted, needed []string, err error) {
	// hosted: sorted for deterministic ordering.
	hostedSet := map[string]struct{}{}
	for name := range all {
		if rs.Has(name) {
			hostedSet[name] = struct{}{}
		}
	}
	// Guard: every explicit role must name a real module.
	if !rs.all {
		for name := range rs.names {
			if _, ok := all[name]; !ok {
				return nil, nil, fmt.Errorf("unknown role %q — valid roles: %s", name, strings.Join(sortedKeys(all), ", "))
			}
		}
	}

	neededSet := map[string]struct{}{}
	for name := range hostedSet {
		for _, dep := range all[name].DependsOn() {
			if _, isHosted := hostedSet[dep]; !isHosted {
				neededSet[dep] = struct{}{}
			}
		}
	}

	// Assert hosted ∩ needed = ∅ — a name can't be both real and stub.
	for name := range neededSet {
		if _, clash := hostedSet[name]; clash {
			return nil, nil, fmt.Errorf("module %q computed as both hosted and remote-stub — gating bug", name)
		}
	}

	return sortedKeys(hostedSet), sortedKeys(neededSet), nil
}

func sortedKeys[V any](m map[string]V) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}

// peerAddrFor returns the QUIC edge address a remote stub for module `name`
// should dial: env <NAME>_EDGE_ADDR (e.g. CHARACTERS_EDGE_ADDR), else the shared
// default. In the split, both providers live behind process A's single edge
// server, so both default to the same host:port.
func peerAddrFor(name string) string {
	if v := os.Getenv(strings.ToUpper(name) + "_EDGE_ADDR"); strings.TrimSpace(v) != "" {
		return v
	}
	return "localhost:9000"
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

func main() {
	log := slog.New(slog.NewTextHandler(os.Stdout, nil))
	ctx := core.NewContext(log)

	dsn := os.Getenv("DATABASE_URL")
	if dsn == "" {
		dsn = defaultDSN
	}
	db, err := sql.Open("pgx", dsn)
	if err != nil {
		log.Error("open db", "err", err)
		os.Exit(1)
	}
	defer db.Close()
	pingCtx, cancelPing := context.WithTimeout(context.Background(), 5*time.Second)
	if err := db.PingContext(pingCtx); err != nil {
		cancelPing()
		log.Error("db unreachable", "err", err)
		os.Exit(1)
	}
	cancelPing()
	ctx.DB = db

	// /healthz is infra — available in EVERY role. 200 once the DB pings, 503
	// if it's down. Registered before anything else so it's always mounted.
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

	// ROLES selects the topology: unset → monolith (all 8 modules); a subset →
	// host those modules and fill their un-hosted dependencies with remote stubs.
	all := realModules()
	roles := parseRoles(os.Getenv("ROLES"))
	hosted, needed, err := planModules(roles, all)
	if err != nil {
		log.Error("role gating failed", "err", err)
		os.Exit(1)
	}
	log.Info("topology", "monolith", roles.all, "hosted", hosted, "remote", needed)

	// Split topology: if this process hosts an edge-exposed provider (accounts or
	// characters) and is NOT the monolith, stand up ONE shared QUIC edge server.
	// Both providers register their handlers on it (in their Init), so a single
	// UDP port (EDGE_ADDR, default :9000) serves every edge method — no per-module
	// port juggling. The monolith runs everything in-process and needs no edge.
	var edgeServer *edge.Server
	if !roles.all && (roles.Has("accounts") || roles.Has("characters")) {
		edgeServer = edge.NewServer()
		if am, ok := all["accounts"].(*accounts.Module); ok && roles.Has("accounts") {
			am.Edge = edgeServer
		}
		if cm, ok := all["characters"].(*characters.Module); ok && roles.Has("characters") {
			cm.Edge = edgeServer
		}
	}

	reg := core.NewRegistry(ctx)
	for _, name := range hosted {
		reg.Add(all[name])
	}
	for _, name := range needed {
		reg.Add(remote.NewStub(name, peerAddrFor(name)))
	}

	if err := reg.Build(); err != nil {
		log.Error("startup failed", "err", err)
		os.Exit(1)
	}

	migCtx, cancelMig := context.WithTimeout(context.Background(), 30*time.Second)
	if err := reg.Migrate(migCtx, db); err != nil {
		cancelMig()
		log.Error("migrate failed", "err", err)
		os.Exit(1)
	}
	cancelMig()

	startCtx, cancelStart := context.WithTimeout(context.Background(), 10*time.Second)
	if err := reg.Start(startCtx); err != nil {
		cancelStart()
		log.Error("start failed", "err", err)
		os.Exit(1)
	}
	cancelStart()

	// Bring up the shared edge server AFTER every module Init has registered its
	// handlers (Init ran in reg.Build). One listener, all edge methods.
	if edgeServer != nil {
		tlsConf, err := edge.SelfSignedTLS()
		if err != nil {
			log.Error("edge tls", "err", err)
			os.Exit(1)
		}
		edgeAddr := os.Getenv("EDGE_ADDR")
		if strings.TrimSpace(edgeAddr) == "" {
			edgeAddr = ":9000"
		}
		if err := edgeServer.ListenAddr(edgeAddr, tlsConf); err != nil {
			log.Error("edge listen", "err", err)
			os.Exit(1)
		}
		log.Info("edge listening", "addr", edgeServer.Addr())
	}

	srv := &http.Server{Addr: normalizeAddr(os.Getenv("PORT")), Handler: ctx.Mux, ReadHeaderTimeout: 10 * time.Second}
	go func() {
		log.Info("listening", "addr", srv.Addr)
		if err := srv.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			log.Error("server stopped", "err", err)
			os.Exit(1)
		}
	}()

	// Graceful shutdown, in order:
	//   1. stop accepting HTTP (no new events get published),
	//   2. drain the bus (in-flight events finish while module resources are up),
	//   3. stop modules in reverse dependency order (close goroutines/resources).
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
	reg.Stop(shutdownCtx)
	log.Info("bye")
}
