package core

import (
	"context"
	"io"
	"log/slog"
	"slices"
	"strings"
	"testing"
)

// recMod records init/start/stop into a shared slice so tests can assert order.
type recMod struct {
	name string
	deps []string
	rec  *[]string
}

func (m recMod) Name() string                    { return m.name }
func (m recMod) DependsOn() []string             { return m.deps }
func (m recMod) Init(*Context) error             { *m.rec = append(*m.rec, "init:"+m.name); return nil }
func (m recMod) Start(context.Context) error     { *m.rec = append(*m.rec, "start:"+m.name); return nil }
func (m recMod) Stop(context.Context) error      { *m.rec = append(*m.rec, "stop:"+m.name); return nil }

func testCtx() *Context {
	return NewContext(slog.New(slog.NewTextHandler(io.Discard, nil)))
}

func before(t *testing.T, rec []string, a, b string) {
	t.Helper()
	ia, ib := slices.Index(rec, a), slices.Index(rec, b)
	if ia < 0 || ib < 0 {
		t.Fatalf("missing event: %q@%d %q@%d in %v", a, ia, b, ib, rec)
	}
	if ia >= ib {
		t.Fatalf("expected %q before %q, got %v", a, b, rec)
	}
}

func TestLifecycleOrder(t *testing.T) {
	var rec []string
	reg := NewRegistry(testCtx())
	// match depends on rating; leaderboard is independent. Registration order is
	// deliberately "wrong" to prove the topo-sort fixes it.
	reg.Add(recMod{"match", []string{"rating"}, &rec})
	reg.Add(recMod{"leaderboard", nil, &rec})
	reg.Add(recMod{"rating", nil, &rec})

	if err := reg.Build(); err != nil {
		t.Fatalf("build: %v", err)
	}
	reg.Start(context.Background())
	reg.Stop(context.Background())

	// Dependency comes up before its dependent...
	before(t, rec, "init:rating", "init:match")
	before(t, rec, "start:rating", "start:match")
	// ...and tears down after it (reverse order).
	before(t, rec, "stop:match", "stop:rating")
}

func TestMissingDependencyFails(t *testing.T) {
	var rec []string
	reg := NewRegistry(testCtx())
	reg.Add(recMod{"match", []string{"rating"}, &rec}) // rating never added

	err := reg.Build()
	if err == nil || !strings.Contains(err.Error(), "missing module") {
		t.Fatalf("expected missing-module error, got %v", err)
	}
}

func TestContributions(t *testing.T) {
	ctx := testCtx()
	ctx.Contribute("s", "a")
	ctx.Contribute("s", "b")
	ctx.Contribute("other", "x")

	got := ctx.Contributions("s")
	if len(got) != 2 || got[0] != "a" || got[1] != "b" {
		t.Fatalf("contributions out of order or wrong: %v", got)
	}
	if len(ctx.Contributions("missing")) != 0 {
		t.Fatalf("empty slot should return nothing, got %v", ctx.Contributions("missing"))
	}
}

func TestCycleFails(t *testing.T) {
	var rec []string
	reg := NewRegistry(testCtx())
	reg.Add(recMod{"a", []string{"b"}, &rec})
	reg.Add(recMod{"b", []string{"a"}, &rec})

	err := reg.Build()
	if err == nil || !strings.Contains(err.Error(), "cycle") {
		t.Fatalf("expected cycle error, got %v", err)
	}
}
