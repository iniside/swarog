package app

import (
	"strings"
	"testing"

	"gamebackend/lifecycle"
)

func TestNormalizeAddr(t *testing.T) {
	tests := map[string]string{
		"":       ":8080",
		"  ":     ":8080",
		"8080":   ":8080",
		":8080":  ":8080",
		"9000":   ":9000",
		":9000":  ":9000",
		" 8081 ": ":8081",
	}
	for in, want := range tests {
		if got := normalizeAddr(in); got != want {
			t.Errorf("normalizeAddr(%q) = %q, want %q", in, got, want)
		}
	}
}

// fakeModule is a minimal lifecycle.Module for exercising validateRequires: it
// only needs a Name and a Requires manifest (Init is never called here).
type fakeModule struct {
	name string
	deps []string
}

func (f fakeModule) Name() string                  { return f.name }
func (f fakeModule) Requires() []string            { return f.deps }
func (fakeModule) Init(_ *lifecycle.Context) error { return nil }

func TestValidateRequires(t *testing.T) {
	tests := []struct {
		name    string
		mods    []lifecycle.Module
		wantErr bool
	}{
		{
			name: "monolith — every requirement satisfied locally",
			mods: []lifecycle.Module{
				fakeModule{name: "accounts"},
				fakeModule{name: "characters", deps: []string{"accounts"}},
				fakeModule{name: "inventory", deps: []string{"accounts", "characters"}},
			},
		},
		{
			name: "split — requirement satisfied by a stub sharing the dep name",
			mods: []lifecycle.Module{
				fakeModule{name: "inventory", deps: []string{"accounts", "characters"}},
				fakeModule{name: "admin"},
				fakeModule{name: "accounts"},   // stub, reports the dep name
				fakeModule{name: "characters"}, // stub
			},
		},
		{
			name: "missing provider — inventory needs characters, none present",
			mods: []lifecycle.Module{
				fakeModule{name: "inventory", deps: []string{"accounts", "characters"}},
				fakeModule{name: "accounts"},
			},
			wantErr: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			err := validateRequires(tc.mods)
			if tc.wantErr && err == nil {
				t.Fatal("expected an error, got nil")
			}
			if !tc.wantErr && err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if tc.wantErr && err != nil && !strings.Contains(err.Error(), "characters") {
				t.Fatalf("error %q does not mention the missing dependency %q", err.Error(), "characters")
			}
		})
	}
}

// TestValidateRequires_HardRequireConfig is the fail-loud guarantee that
// justifies moving inventory's config dependency from soft (TryRequire) to
// hard (Require + Requires()): a process that hosts inventory but forgets to
// host config must refuse to boot, with an error naming "config", rather than
// silently degrading or panicking deep inside Build.
func TestValidateRequires_HardRequireConfig(t *testing.T) {
	mods := []lifecycle.Module{
		fakeModule{name: "accounts"},
		fakeModule{name: "characters", deps: []string{"accounts"}},
		fakeModule{name: "inventory", deps: []string{"accounts", "characters", "config"}},
		// config is deliberately absent from this process's module set.
	}
	err := validateRequires(mods)
	if err == nil {
		t.Fatal("expected an error when config is not hosted, got nil")
	}
	if !strings.Contains(err.Error(), "config") {
		t.Fatalf("error %q does not mention the missing dependency %q", err.Error(), "config")
	}

	mods = append(mods, fakeModule{name: "config"})
	if err := validateRequires(mods); err != nil {
		t.Fatalf("unexpected error once config is hosted: %v", err)
	}
}
