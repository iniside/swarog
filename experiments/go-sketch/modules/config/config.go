// Package config is a central, DB-backed configuration store with live reload.
// Namespaced key=value settings live in schema "config"; any module reads them
// via the provided "config" service (GetString/GetBool/GetInt with a code-
// default fallback), and edits made in /admin (or raw psql) propagate to every
// reader through Postgres LISTEN/NOTIFY -> in-memory cache refresh ->
// config.changed. Secrets stay in env; only non-secret operational knobs go here.
package config

import (
	"context"
	"database/sql"
	"log/slog"
	"os"

	"gamebackend/api/admin/adminapi"
	"gamebackend/bus"
	"gamebackend/lifecycle"
	"gamebackend/registry"
)

// defaultDSN is the fallback the listener connects with — same default as the
// app's shared pool (internal/app/app.go). config can't store the DSN it needs
// to reach its own store, so this bootstrap tier stays in env (decision #6).
const defaultDSN = "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable"

// Module is a pointer receiver: it holds the constructed service (shared between
// Register and the listener), the listener's cancel/done handles, and its DSN.
type Module struct {
	db  *sql.DB
	log *slog.Logger
	bus *bus.Bus
	svc *service
	dsn string

	cancel context.CancelFunc
	done   chan struct{}
}

func (*Module) Name() string       { return "config" }
func (*Module) Requires() []string { return nil } // foundation — depends on nobody

// Register builds the service and offers it in Build's phase 1, before any Init,
// so a dependent's (Try)Require resolves regardless of registration order.
func (m *Module) Register(ctx *lifecycle.Context) error {
	m.svc = &service{cache: map[cacheKey]string{}}
	registry.Provide(ctx.Registry, "config", m.svc)
	return nil
}

// Migrate creates this module's own schema. Idempotent.
func (*Module) Migrate(_ context.Context, db *sql.DB) error {
	_, err := db.Exec(schemaDDL)
	return err
}

// Init only wires up — no DB I/O (constraint #8). It stores handles, resolves
// the listener DSN, and contributes the admin editor page. The cache is filled
// by the listener's first connect (Start), not here.
func (m *Module) Init(ctx *lifecycle.Context) error {
	m.db = ctx.DB
	m.log = ctx.Log
	m.bus = ctx.Bus
	m.svc.db = ctx.DB
	m.svc.log = ctx.Log
	m.dsn = envOr("DATABASE_URL", defaultDSN)

	ctx.Contribute(adminapi.Slot, adminapi.Item{
		ID:      "config",
		Section: "Platform",
		Label:   "Game Config & Flags",
		Render:  m.adminRender,
	})
	return nil
}

// Start launches the LISTEN/NOTIFY loop. Like outbox.Relay it roots a fresh
// background context so a short Start deadline can't kill the loop; Stop cancels
// it. The initial full cache load happens inside the loop's first connect, so
// boot and reconnect share one cache-population path.
//
//nolint:contextcheck // intentional: the listen loop's lifetime is bounded by Stop, not Start's ctx.
func (m *Module) Start(_ context.Context) error {
	runCtx, cancel := context.WithCancel(context.Background())
	m.cancel = cancel
	m.done = make(chan struct{})
	go func() {
		defer close(m.done)
		m.listen(runCtx)
	}()
	return nil
}

// Stop cancels the loop and waits for it to exit (bounded by ctx). It does NOT
// close the pgx conn — the loop owns and re-creates it across reconnects, so
// Stop has no stable handle (a permanent outage degrades to stale-cache+retry).
func (m *Module) Stop(ctx context.Context) error {
	if m.cancel != nil {
		m.cancel()
	}
	if m.done == nil {
		return nil
	}
	select {
	case <-m.done:
	case <-ctx.Done():
	}
	return nil
}

func envOr(key, def string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return def
}
