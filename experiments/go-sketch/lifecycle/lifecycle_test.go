package lifecycle

import (
	"context"
	"database/sql"
	"io"
	"log/slog"
	"slices"
	"testing"
)

// recMod records every lifecycle callback into a shared slice so a test can
// assert phase ordering. It implements Registrar/Migrator/Starter/Stopper so all
// phases fire.
type recMod struct {
	name string
	rec  *[]string
}

func (m *recMod) Name() string                     { return m.name }
func (m *recMod) Requires() []string               { return nil }
func (m *recMod) Register(*Context) error          { *m.rec = append(*m.rec, "register:"+m.name); return nil }
func (m *recMod) Init(*Context) error              { *m.rec = append(*m.rec, "init:"+m.name); return nil }
func (m *recMod) Migrate(context.Context, *sql.DB) error {
	*m.rec = append(*m.rec, "migrate:"+m.name)
	return nil
}
func (m *recMod) Start(context.Context) error { *m.rec = append(*m.rec, "start:"+m.name); return nil }
func (m *recMod) Stop(context.Context) error  { *m.rec = append(*m.rec, "stop:"+m.name); return nil }

func testCtx() *Context {
	return NewContext(slog.New(slog.NewTextHandler(io.Discard, nil)))
}

// TestTwoPhaseBuild is the core guarantee of the split: ALL Registers run before
// ANY Init (phase 1 → phase 2), each phase in registration order. That is what
// lets a module Require any service in Init without a topological sort.
func TestTwoPhaseBuild(t *testing.T) {
	var rec []string
	app := NewApp(testCtx())
	app.Add(&recMod{"a", &rec})
	app.Add(&recMod{"b", &rec})

	if err := app.Build(); err != nil {
		t.Fatalf("build: %v", err)
	}

	want := []string{"register:a", "register:b", "init:a", "init:b"}
	if !slices.Equal(rec, want) {
		t.Fatalf("build order = %v; want %v", rec, want)
	}
}

// TestLifecyclePhases proves Migrate and Start run in registration order after
// Build, and Stop runs in REVERSE registration order (S5).
func TestLifecyclePhases(t *testing.T) {
	var rec []string
	app := NewApp(testCtx())
	app.Add(&recMod{"a", &rec})
	app.Add(&recMod{"b", &rec})

	if err := app.Build(); err != nil {
		t.Fatalf("build: %v", err)
	}
	if err := app.Migrate(context.Background(), nil); err != nil {
		t.Fatalf("migrate: %v", err)
	}
	if err := app.Start(context.Background()); err != nil {
		t.Fatalf("start: %v", err)
	}
	app.Stop(context.Background())

	want := []string{
		"register:a", "register:b",
		"init:a", "init:b",
		"migrate:a", "migrate:b",
		"start:a", "start:b",
		"stop:b", "stop:a", // reverse registration order
	}
	if !slices.Equal(rec, want) {
		t.Fatalf("lifecycle order = %v; want %v", rec, want)
	}
}

// TestDuplicateModulePanics keeps the loud-at-startup guard on a double Add.
func TestDuplicateModulePanics(t *testing.T) {
	defer func() {
		if r := recover(); r == nil {
			t.Fatal("expected panic on duplicate module, got none")
		}
	}()
	var rec []string
	app := NewApp(testCtx())
	app.Add(&recMod{"dup", &rec})
	app.Add(&recMod{"dup", &rec})
}
