package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

const (
	goodPattern = "./testdata/ownershipapi"
	goldenPath  = "testdata/ownershiprpc/ownershiprpc_gen.go"
)

// TestGoldenUpToDate is the drift gate: regenerating from the committed api
// package must reproduce the committed golden byte-for-byte (after the identical
// gofmt normalization -check applies). If this fails, the golden is stale — run
// `go generate ./tools/rpcgen/testdata/ownershipapi`.
func TestGoldenUpToDate(t *testing.T) {
	src, err := run(goodPattern, "Ownership", "ownership", "ownershiprpc")
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	if err := checkAgainst(goldenPath, src); err != nil {
		t.Fatalf("generated output does not match committed golden: %v", err)
	}
}

// TestCheckDetectsDrift proves -check FAILS when the committed file diverges from
// the generator: a one-line mutation of the golden written to a temp file must be
// reported as stale against the true generated source.
func TestCheckDetectsDrift(t *testing.T) {
	src, err := run(goodPattern, "Ownership", "ownership", "ownershiprpc")
	if err != nil {
		t.Fatalf("run: %v", err)
	}

	golden, err := os.ReadFile(goldenPath)
	if err != nil {
		t.Fatalf("read golden: %v", err)
	}
	mutated := strings.Replace(string(golden),
		`MethodOwnerOf  = "ownership.ownerOf"`,
		`MethodOwnerOf  = "ownership.MUTATED"`, 1)
	if mutated == string(golden) {
		t.Fatal("mutation did not change the golden — test setup is stale")
	}

	tmp := filepath.Join(t.TempDir(), "mutated_gen.go")
	if err := os.WriteFile(tmp, []byte(mutated), 0o644); err != nil {
		t.Fatalf("write mutated: %v", err)
	}
	if err := checkAgainst(tmp, src); err == nil {
		t.Fatal("checkAgainst accepted a mutated file; drift went undetected")
	}
}

// TestUnsupportedShapesError proves each out-of-scope signature shape is a
// generate-time error, not silently-broken code.
func TestUnsupportedShapesError(t *testing.T) {
	cases := []struct {
		iface string
		want  string // substring the error must contain
	}{
		{"IfaceParam", "interface-typed"},
		{"IfaceReturn", "interface-typed"},
		{"ChanParam", "channel-typed"},
		{"FuncParam", "func-typed"},
		{"NoCtx", "context.Context"},
		{"NoErr", "last result must be error"},
		{"Generic", "generic"},
	}
	for _, tc := range cases {
		t.Run(tc.iface, func(t *testing.T) {
			_, err := run("./testdata/badapi", tc.iface, "bad", "badrpc")
			if err == nil {
				t.Fatalf("expected an error for %s, got none", tc.iface)
			}
			if !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("error for %s = %q; want it to contain %q", tc.iface, err.Error(), tc.want)
			}
		})
	}
}

// TestMissingInterface reports a clear error when the named interface is absent.
func TestMissingInterface(t *testing.T) {
	_, err := run(goodPattern, "Nonexistent", "x", "xrpc")
	if err == nil || !strings.Contains(err.Error(), "not found") {
		t.Fatalf("want a not-found error, got %v", err)
	}
}
