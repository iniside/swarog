package core

import (
	"context"
	"database/sql"
	"fmt"
	"log/slog"
	"net/http"
)

// Module is the contract every plugin implements. The core NEVER imports a
// module; modules import the core. Dependency points one way only.
//
// Init only WIRES the module up: register services, subscribe to events, mount
// routes. No background work and no I/O yet — that belongs in Start.
type Module interface {
	Name() string
	DependsOn() []string // modules that must Init first — the registry enforces it
	Init(ctx *Context) error
}

// Starter is an OPTIONAL capability. A module that runs background work (a
// ticker, a worker pool, an outbound connection) implements it. Started in
// dependency order, after every module's Init.
type Starter interface {
	Start(ctx context.Context) error
}

// Stopper is an OPTIONAL capability. A module that holds resources implements it
// to clean up. Stopped in REVERSE dependency order, so a module's dependencies
// are still alive while it tears itself down. Don't emit events from Stop — by
// then the bus has already drained.
type Stopper interface {
	Stop(ctx context.Context) error
}

// Migrator is an OPTIONAL capability. A module that persists data implements it
// to create/upgrade its OWN schema — and only its own (full logical isolation:
// no cross-module tables, no cross-module foreign keys). Run after Init, before
// Start, in dependency order. Must be idempotent (CREATE ... IF NOT EXISTS).
type Migrator interface {
	Migrate(ctx context.Context, db *sql.DB) error
}

// Context is the slice of the core handed to each module at Init. It exposes
// only primitives: the event bus, a service registry, an HTTP mux and a logger.
type Context struct {
	Bus *Bus
	Mux *http.ServeMux
	// DB is the shared Postgres pool. It is OFFERED, not mandated: a module may
	// use it (owning its own schema), or ignore it and bring its own store.
	DB       *sql.DB
	Log      *slog.Logger
	services map[string]any
}

func NewContext(log *slog.Logger) *Context {
	return &Context{
		Bus:      NewBus(log),
		Mux:      http.NewServeMux(),
		Log:      log,
		services: map[string]any{},
	}
}

// Provide registers a named service so other modules can Require it. Panics on
// duplicate — that's a wiring bug, better loud at startup than silent later.
func (c *Context) Provide(name string, svc any) {
	if _, exists := c.services[name]; exists {
		panic(fmt.Sprintf("service %q already provided", name))
	}
	c.services[name] = svc
}

// Require returns the raw service; the caller asserts it to its OWN local
// interface (Go structural typing — see modules/match/match.go).
func (c *Context) Require(name string) any {
	svc, ok := c.services[name]
	if !ok {
		panic(fmt.Sprintf("required service %q not found", name))
	}
	return svc
}

// Registry collects modules and initializes them in dependency order.
type Registry struct {
	modules map[string]Module
	order   []string // registration order, for stable tie-breaking
	sorted  []string // dependency order, set by Build; drives Start/Stop
	ctx     *Context
}

func NewRegistry(ctx *Context) *Registry {
	return &Registry{modules: map[string]Module{}, ctx: ctx}
}

func (r *Registry) Add(m Module) {
	if _, dup := r.modules[m.Name()]; dup {
		panic(fmt.Sprintf("module %q registered twice", m.Name()))
	}
	r.modules[m.Name()] = m
	r.order = append(r.order, m.Name())
}

// Build topologically sorts modules by DependsOn and calls Init on each.
// Detects cycles and missing dependencies — loudly, at startup.
func (r *Registry) Build() error {
	sorted, err := r.topoSort()
	if err != nil {
		return err
	}
	r.sorted = sorted
	for _, name := range sorted {
		if err := r.modules[name].Init(r.ctx); err != nil {
			return fmt.Errorf("init %q: %w", name, err)
		}
		r.ctx.Log.Info("module ready", "module", name)
	}
	return nil
}

// Migrate runs Migrate on every module that implements Migrator, in dependency
// order, so a module always migrates after the modules it depends on. Call it
// after Build and before Start.
func (r *Registry) Migrate(ctx context.Context, db *sql.DB) error {
	for _, name := range r.sorted {
		m, ok := r.modules[name].(Migrator)
		if !ok {
			continue
		}
		if err := m.Migrate(ctx, db); err != nil {
			return fmt.Errorf("migrate %q: %w", name, err)
		}
		r.ctx.Log.Info("module migrated", "module", name)
	}
	return nil
}

// Start runs Start on every module that implements Starter, in dependency order
// (a module's dependencies start before it). Fails fast.
func (r *Registry) Start(ctx context.Context) error {
	for _, name := range r.sorted {
		s, ok := r.modules[name].(Starter)
		if !ok {
			continue
		}
		if err := s.Start(ctx); err != nil {
			return fmt.Errorf("start %q: %w", name, err)
		}
		r.ctx.Log.Info("module started", "module", name)
	}
	return nil
}

// Stop runs Stop on every module that implements Stopper, in REVERSE dependency
// order. Best-effort: it logs and continues on error so one stuck module can't
// strand the rest.
func (r *Registry) Stop(ctx context.Context) {
	for i := len(r.sorted) - 1; i >= 0; i-- {
		name := r.sorted[i]
		s, ok := r.modules[name].(Stopper)
		if !ok {
			continue
		}
		if err := s.Stop(ctx); err != nil {
			r.ctx.Log.Error("module stop failed", "module", name, "err", err)
		} else {
			r.ctx.Log.Info("module stopped", "module", name)
		}
	}
}

func (r *Registry) topoSort() ([]string, error) {
	const (
		white = 0 // unvisited
		gray  = 1 // on the current DFS stack
		black = 2 // done
	)
	state := map[string]int{}
	var out []string
	var visit func(string) error
	visit = func(name string) error {
		switch state[name] {
		case gray:
			return fmt.Errorf("dependency cycle involving %q", name)
		case black:
			return nil
		}
		state[name] = gray
		for _, dep := range r.modules[name].DependsOn() {
			if _, ok := r.modules[dep]; !ok {
				return fmt.Errorf("module %q depends on missing module %q", name, dep)
			}
			if err := visit(dep); err != nil {
				return err
			}
		}
		state[name] = black
		out = append(out, name)
		return nil
	}
	for _, name := range r.order {
		if err := visit(name); err != nil {
			return nil, err
		}
	}
	return out, nil
}
