package lifecycle

import (
	"database/sql"
	"log/slog"
	"net/http"

	"gamebackend/bus"
	"gamebackend/contrib"
	"gamebackend/registry"
)

// Context is the slice of the core handed to each module. It exposes only
// primitives: the event bus, the service registry, the contribution slots, an
// HTTP mux, the shared DB pool and a logger.
type Context struct {
	Bus      *bus.Bus
	Registry *registry.Registry
	Slots    *contrib.Slots
	Mux      *http.ServeMux
	// DB is the shared Postgres pool. It is OFFERED, not mandated: a module may
	// use it (owning its own schema), or ignore it and bring its own store.
	DB  *sql.DB
	Log *slog.Logger
}

func NewContext(log *slog.Logger) *Context {
	return &Context{
		Bus:      bus.NewBus(log),
		Registry: registry.New(),
		Slots:    contrib.New(),
		Mux:      http.NewServeMux(),
		Log:      log,
	}
}

// Contribute forwards to the slot registry: it adds a value to a named slot.
// Unlike Provide (one service per name), a slot collects MANY contributors — for
// cross-cutting collections like admin items — so a new module lights up without
// the consumer being edited.
func (c *Context) Contribute(slot string, v any) {
	c.Slots.Contribute(slot, v)
}

// Contributions forwards to the slot registry: everything contributed to a slot,
// in registration order. Read it lazily (e.g. per request), after all modules
// have initialized.
func (c *Context) Contributions(slot string) []any {
	return c.Slots.Contributions(slot)
}
