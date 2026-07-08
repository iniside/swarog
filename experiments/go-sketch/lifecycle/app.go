package lifecycle

import (
	"context"
	"database/sql"
	"fmt"
)

// App collects modules and drives their lifecycle. It is the renamed core
// orchestrator (the name "Registry" now belongs to the SERVICE registry, its own
// package). Modules run in REGISTRATION order — there is NO topological sort:
// full logical isolation makes init order commutative, and the two-phase Build
// (Register → Init) guarantees every Provided service exists before any Init
// Requires it.
type App struct {
	modules []Module
	names   map[string]struct{} // guards against a name registered twice
	ctx     *Context
}

func NewApp(ctx *Context) *App {
	return &App{names: map[string]struct{}{}, ctx: ctx}
}

func (a *App) Add(m Module) {
	if _, dup := a.names[m.Name()]; dup {
		panic(fmt.Sprintf("module %q registered twice", m.Name()))
	}
	a.names[m.Name()] = struct{}{}
	a.modules = append(a.modules, m)
}

// Build wires every module in two phases, both in registration order:
//
//   - phase 1 (Register): each module that PROVIDES a service (Registrar)
//     constructs and registers it. Runs FIRST so every service exists before
//     any Init runs.
//   - phase 2 (Init): each module mounts routes, subscribes to the bus,
//     contributes admin items and Requires the services it needs.
//
// No topological sort: with full logical isolation no Init consumes a Required
// service during Init, so registration order suffices. A genuinely missing
// service still fails loudly — the eager Require in phase 2 panics.
func (a *App) Build() error {
	for _, m := range a.modules {
		r, ok := m.(Registrar)
		if !ok {
			continue
		}
		if err := r.Register(a.ctx); err != nil {
			return fmt.Errorf("register %q: %w", m.Name(), err)
		}
	}
	for _, m := range a.modules {
		if err := m.Init(a.ctx); err != nil {
			return fmt.Errorf("init %q: %w", m.Name(), err)
		}
		a.ctx.Log.Info("module ready", "module", m.Name())
	}
	return nil
}

// Migrate runs Migrate on every module that implements Migrator, in registration
// order. Call it after Build and before Start.
func (a *App) Migrate(ctx context.Context, db *sql.DB) error {
	for _, m := range a.modules {
		mig, ok := m.(Migrator)
		if !ok {
			continue
		}
		if err := mig.Migrate(ctx, db); err != nil {
			return fmt.Errorf("migrate %q: %w", m.Name(), err)
		}
		a.ctx.Log.Info("module migrated", "module", m.Name())
	}
	return nil
}

// Start runs Start on every module that implements Starter, in registration
// order. Fails fast.
func (a *App) Start(ctx context.Context) error {
	for _, m := range a.modules {
		s, ok := m.(Starter)
		if !ok {
			continue
		}
		if err := s.Start(ctx); err != nil {
			return fmt.Errorf("start %q: %w", m.Name(), err)
		}
		a.ctx.Log.Info("module started", "module", m.Name())
	}
	return nil
}

// Stop runs Stop on every module that implements Stopper, in REVERSE registration
// order (S5). Best-effort: it logs and continues on error so one stuck module
// can't strand the rest.
func (a *App) Stop(ctx context.Context) {
	for i := len(a.modules) - 1; i >= 0; i-- {
		m := a.modules[i]
		s, ok := m.(Stopper)
		if !ok {
			continue
		}
		if err := s.Stop(ctx); err != nil {
			a.ctx.Log.Error("module stop failed", "module", m.Name(), "err", err)
		} else {
			a.ctx.Log.Info("module stopped", "module", m.Name())
		}
	}
}
