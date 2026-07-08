// Package lifecycle wires modules together: it holds the Context handed to each
// module at startup and the App that builds/migrates/starts/stops them. It
// imports the three leaf foundations (bus, registry, contrib) plus stdlib;
// nothing in those leaves imports lifecycle, so the import graph stays acyclic.
package lifecycle

import (
	"context"
	"database/sql"
)

// Module is the contract every plugin implements. The core foundations NEVER
// import a module; modules import them. Dependency points one way only.
//
// Init only WIRES the module up: subscribe to events, mount routes, Require the
// services it needs. No background work and no I/O yet — that belongs in Start.
// A module that PROVIDES a service does so in Register (see Registrar), which
// runs before any Init, so every service exists by the time Inits run.
type Module interface {
	Name() string
	// Requires lists the service names this module Requires. It is a MANIFEST of
	// declared dependencies — it drives composition-time stub-planning (cmd) and
	// is available for future validation. It does NOT order startup: with full
	// logical isolation no Init consumes a Required service during Init, so init
	// order is commutative and Build runs modules in registration order.
	Requires() []string
	Init(ctx *Context) error
}

// Registrar is an OPTIONAL capability. A module that PROVIDES a service to the
// registry implements it: Register constructs the service and registers it in
// Build's phase 1, BEFORE any module's Init. That ordering is what lets every
// Init freely Require any service without a topological sort.
type Registrar interface {
	Register(ctx *Context) error
}

// Starter is an OPTIONAL capability. A module that runs background work (a
// ticker, a worker pool, an outbound connection) implements it. Started in
// registration order, after every module's Init.
type Starter interface {
	Start(ctx context.Context) error
}

// Stopper is an OPTIONAL capability. A module that holds resources implements it
// to clean up. Stopped in REVERSE registration order (S5: reverse-registration,
// not reverse-dependency — benign here, since no module uses a dependency in
// Stop). Don't emit events from Stop — by then the bus has already drained.
type Stopper interface {
	Stop(ctx context.Context) error
}

// Migrator is an OPTIONAL capability. A module that persists data implements it
// to create/upgrade its OWN schema — and only its own (full logical isolation:
// no cross-module tables, no cross-module foreign keys). Run after Build, before
// Start, in registration order. Must be idempotent (CREATE ... IF NOT EXISTS).
type Migrator interface {
	Migrate(ctx context.Context, db *sql.DB) error
}
