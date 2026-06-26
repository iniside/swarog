package core

import (
	"fmt"
	"log/slog"
	"net/http"
)

// Module is the contract every plugin implements. The core NEVER imports a
// module; modules import the core. Dependency points one way only.
type Module interface {
	Name() string
	DependsOn() []string // modules that must Init first — the registry enforces it
	Init(ctx *Context) error
}

// Context is the slice of the core handed to each module at Init. It exposes
// only primitives: the event bus, a service registry, an HTTP mux and a logger.
type Context struct {
	Bus      *Bus
	Mux      *http.ServeMux
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
	for _, name := range sorted {
		if err := r.modules[name].Init(r.ctx); err != nil {
			return fmt.Errorf("init %q: %w", name, err)
		}
		r.ctx.Log.Info("module ready", "module", name)
	}
	return nil
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
