package app

import (
	"context"
	"database/sql"
	"errors"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"testing"
	"time"

	_ "github.com/jackc/pgx/v5/stdlib"

	"gamebackend/httpmw"
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

// testDB opens a real connection to the local Postgres (per the repo's "local
// Postgres is the test DB" convention) and skips the test if it's unreachable,
// rather than faking a *sql.DB (an untestable concrete type).
func testDB(t *testing.T) *sql.DB {
	t.Helper()
	dsn := os.Getenv("DATABASE_URL")
	if dsn == "" {
		dsn = defaultDSN
	}
	db, err := sql.Open("pgx", dsn)
	if err != nil {
		t.Skipf("no postgres: %v", err)
	}
	pingCtx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	if err := db.PingContext(pingCtx); err != nil {
		_ = db.Close()
		t.Skipf("postgres unreachable: %v", err)
	}
	t.Cleanup(func() { _ = db.Close() })
	return db
}

func TestHealthzAlwaysOK(t *testing.T) {
	req := httptest.NewRequest(http.MethodGet, "/healthz", nil)
	rec := httptest.NewRecorder()
	healthzHandler(rec, req)
	if rec.Code != http.StatusOK {
		t.Fatalf("healthz status = %d, want 200", rec.Code)
	}
	if rec.Body.String() != "ok" {
		t.Fatalf("healthz body = %q, want %q", rec.Body.String(), "ok")
	}
}

func TestReadyzHealthyNoContributors(t *testing.T) {
	db := testDB(t)
	ctx := lifecycle.NewContext(slog.New(slog.NewTextHandler(io.Discard, nil)))

	req := httptest.NewRequest(http.MethodGet, "/readyz", nil)
	rec := httptest.NewRecorder()
	readyzHandler(ctx, db)(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("readyz status = %d, want 200; body=%s", rec.Code, rec.Body.String())
	}
}

func TestReadyzFailingContributorReturns503(t *testing.T) {
	db := testDB(t)
	ctx := lifecycle.NewContext(slog.New(slog.NewTextHandler(io.Discard, nil)))
	ctx.Contribute(httpmw.ReadinessSlot, func(context.Context) error {
		return errors.New("dependency down")
	})

	req := httptest.NewRequest(http.MethodGet, "/readyz", nil)
	rec := httptest.NewRecorder()
	readyzHandler(ctx, db)(rec, req)

	if rec.Code != http.StatusServiceUnavailable {
		t.Fatalf("readyz status = %d, want 503; body=%s", rec.Code, rec.Body.String())
	}
	if !strings.Contains(rec.Body.String(), "dependency down") {
		t.Fatalf("readyz body %q does not mention the contributor's error", rec.Body.String())
	}
}

func TestReadyzHealthyContributorReturns200(t *testing.T) {
	db := testDB(t)
	ctx := lifecycle.NewContext(slog.New(slog.NewTextHandler(io.Discard, nil)))
	ctx.Contribute(httpmw.ReadinessSlot, func(context.Context) error { return nil })

	req := httptest.NewRequest(http.MethodGet, "/readyz", nil)
	rec := httptest.NewRecorder()
	readyzHandler(ctx, db)(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("readyz status = %d, want 200; body=%s", rec.Code, rec.Body.String())
	}
}
