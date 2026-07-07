package main

import (
	"strings"
	"testing"

	"golang.org/x/tools/go/packages"
)

// TestAnalyze exercises the emit-then-poll detector end to end against the four
// testdata fixtures: poll/ (emit then a sleep+read loop) must fire; clean/
// (emit then response, no loop), publisher/ (emit INSIDE the loop — the
// emit-in-loop guard), and allowed/ (poll shape + //synccheck:allow) must all
// stay silent.
func TestAnalyze(t *testing.T) {
	// Load the fixtures by explicit import path: the `...` wildcard skips
	// testdata/ dirs, but naming the packages directly loads them (and the shared
	// gamebackend/bus + database/sql objects the predicates resolve against).
	const base = "gamebackend/tools/synccheck/testdata/"
	pkgs, err := packages.Load(&packages.Config{Mode: loadMode},
		base+"poll", base+"clean", base+"publisher", base+"allowed",
	)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if n := packages.PrintErrors(pkgs); n > 0 {
		t.Fatalf("testdata failed to load: %d package errors", n)
	}
	if len(pkgs) == 0 {
		t.Fatal("no testdata packages loaded")
	}

	findings := analyze(pkgs, config{})

	// Exactly one finding, and it must be in the poll fixture.
	if len(findings) != 1 {
		t.Fatalf("expected exactly one finding, got %d: %+v", len(findings), findings)
	}
	f := findings[0]
	if f.Kind != "emit-then-poll" {
		t.Errorf("finding Kind = %q, want emit-then-poll", f.Kind)
	}
	if !strings.Contains(f.Pos.Filename, "poll") {
		t.Errorf("finding should be in the poll fixture, got %s", f.Pos)
	}

	// None of the silent fixtures may contribute a finding.
	for _, f := range findings {
		for _, silent := range []string{"clean", "publisher", "allowed"} {
			if strings.Contains(f.Pos.Filename, silent) {
				t.Errorf("%s fixture must stay silent, but produced: %s: %s", silent, f.Pos, f.Msg)
			}
		}
	}
}
