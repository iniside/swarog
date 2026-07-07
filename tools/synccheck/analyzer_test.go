package main

import (
	"testing"

	"golang.org/x/tools/go/packages"
)

// TestAnalyze is the Step 1 harness smoke test: it loads the minimal testdata
// fixture with the same mode the binary uses and asserts analyze returns zero
// findings — proving the whole-module load + config plumbing works before any
// detector logic exists. Step 2 replaces this with fixture-driven assertions
// (poll/ fires, clean/publisher/allowed stay silent).
func TestAnalyze(t *testing.T) {
	pkgs, err := packages.Load(&packages.Config{Mode: loadMode},
		"gamebackend/tools/synccheck/testdata/fixture")
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
	if len(findings) != 0 {
		t.Errorf("expected zero findings from the Step 1 stub, got %d: %+v", len(findings), findings)
	}
}
