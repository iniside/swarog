package main

import (
	"testing"

	"golang.org/x/tools/go/packages"
)

// TestAnalyze exercises the real object-identity match end to end: it loads the
// testdata fixture module (events + sub) with the same mode the binary uses and
// asserts the diff reports exactly the defined-but-unsubscribed, non-allowlisted
// topic — proving the cross-package shared-object match works and that the
// allowlist directive suppresses a finding.
func TestAnalyze(t *testing.T) {
	// Load by explicit import path: the `...` wildcard skips testdata/ dirs, but
	// naming the fixture packages directly loads them (and their shared bus objects).
	pkgs, err := packages.Load(&packages.Config{Mode: loadMode},
		"gamebackend/tools/topiccheck/testdata/events",
		"gamebackend/tools/topiccheck/testdata/sub",
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

	got := map[string]bool{}
	for _, f := range analyze(pkgs) {
		got[f.Topic] = true
	}

	if !got["testdata.unsubscribed"] {
		t.Error("testdata.unsubscribed should be reported (defined, no subscriber, no allowlist)")
	}
	if got["testdata.subscribed"] {
		t.Error("testdata.subscribed must NOT be reported (it is subscribed via bus.On)")
	}
	if got["testdata.ontx"] {
		t.Error("testdata.ontx must NOT be reported (subscribed via bus.OnTx, EventType var object identity)")
	}
	if got["testdata.ontxraw"] {
		t.Error("testdata.ontxraw must NOT be reported (subscribed via bus.OnTxRaw, string-literal topic match)")
	}
	if got["testdata.allowlisted"] {
		t.Error("testdata.allowlisted must NOT be reported (allowlist directive)")
	}
	if len(got) != 1 {
		t.Errorf("expected exactly one finding, got %d: %v", len(got), got)
	}
}
