package gateway

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"gamebackend/edge"
)

// dialPlayer dials a gateway edge.Server as a player would, returning a Client.
// It mirrors edge's own dial helper but lives here since edge's is unexported and
// in another package.
func dialPlayer(t *testing.T, ctx context.Context, addr string) *edge.Client {
	t.Helper()
	cli, err := edge.Dial(ctx, addr, edge.ClientTLS())
	if err != nil {
		t.Fatalf("Dial %s: %v", addr, err)
	}
	return cli
}

// TestLiveSmokeRoutesToDistinctBackends is the end-to-end proof of prefix
// dispatch: a player edge.Client dials a gateway whose "characters." and
// "inventory." prefixes each Forward to a DIFFERENT backend edge.Server. The
// player's characters.list must come back with the characters backend's payload
// and inventory.list with the inventory backend's — proving the router reaches
// distinct services, not one loopback.
func TestLiveSmokeRoutesToDistinctBackends(t *testing.T) {
	chAddr, stopCH := startBackend(t, func(s *edge.Server) {
		s.Handle("characters.list", func(_ []byte) ([]byte, error) {
			return []byte(`{"characters":["Aria"]}`), nil
		})
	})
	defer stopCH()

	invAddr, stopINV := startBackend(t, func(s *edge.Server) {
		s.Handle("inventory.list", func(_ []byte) ([]byte, error) {
			return []byte(`{"items":["starter_sword"]}`), nil
		})
	})
	defer stopINV()

	chRB := NewRoutedBackend(chAddr)
	defer func() { _ = chRB.Close() }()
	invRB := NewRoutedBackend(invAddr)
	defer func() { _ = invRB.Close() }()

	gwAddr, stopGW := startBackend(t, func(s *edge.Server) {
		s.HandlePrefix("characters.", chRB.Forward)
		s.HandlePrefix("inventory.", invRB.Forward)
	})
	defer stopGW()

	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()

	player := dialPlayer(t, ctx, gwAddr)
	defer func() { _ = player.Close() }()

	var chResp struct {
		Characters []string `json:"characters"`
	}
	if err := player.Call(ctx, "characters.list", struct{}{}, &chResp); err != nil {
		t.Fatalf("characters.list through gateway: %v", err)
	}
	if len(chResp.Characters) != 1 || chResp.Characters[0] != "Aria" {
		t.Fatalf("characters.list routed to wrong backend: %+v", chResp)
	}

	var invResp struct {
		Items []string `json:"items"`
	}
	if err := player.Call(ctx, "inventory.list", struct{}{}, &invResp); err != nil {
		t.Fatalf("inventory.list through gateway: %v", err)
	}
	if len(invResp.Items) != 1 || invResp.Items[0] != "starter_sword" {
		t.Fatalf("inventory.list routed to wrong backend: %+v", invResp)
	}
}

// TestLiveSmokeGracefulDegradation proves the gateway degrades gracefully: with
// both backends live it warms the characters route, then the characters backend
// is closed. A subsequent characters.list must surface an ERROR within a bounded
// time (the RoutedBackend retry budget is 2×1s), and — crucially — inventory.list
// must STILL succeed. A dead peer is isolated; it does not take the gateway down.
func TestLiveSmokeGracefulDegradation(t *testing.T) {
	chAddr, stopCH := startBackend(t, func(s *edge.Server) {
		s.Handle("characters.list", func(_ []byte) ([]byte, error) {
			return []byte(`{"characters":["Aria"]}`), nil
		})
	})
	chClosed := false
	defer func() {
		if !chClosed {
			stopCH()
		}
	}()

	invAddr, stopINV := startBackend(t, func(s *edge.Server) {
		s.Handle("inventory.list", func(_ []byte) ([]byte, error) {
			return []byte(`{"items":["starter_sword"]}`), nil
		})
	})
	defer stopINV()

	chRB := NewRoutedBackend(chAddr)
	defer func() { _ = chRB.Close() }()
	invRB := NewRoutedBackend(invAddr)
	defer func() { _ = invRB.Close() }()

	gwAddr, stopGW := startBackend(t, func(s *edge.Server) {
		s.HandlePrefix("characters.", chRB.Forward)
		s.HandlePrefix("inventory.", invRB.Forward)
	})
	defer stopGW()

	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()

	player := dialPlayer(t, ctx, gwAddr)
	defer func() { _ = player.Close() }()

	// Warm the characters route so the RoutedBackend caches a live client — this
	// exercises the stale-connection retry path once the backend dies.
	var chResp struct {
		Characters []string `json:"characters"`
	}
	if err := player.Call(ctx, "characters.list", struct{}{}, &chResp); err != nil {
		t.Fatalf("warm-up characters.list: %v", err)
	}
	if len(chResp.Characters) != 1 || chResp.Characters[0] != "Aria" {
		t.Fatalf("warm-up routed to wrong backend: %+v", chResp)
	}

	// Kill the characters backend.
	stopCH()
	chClosed = true

	// The player's own ctx timeout is a backstop so the test can never hang; the
	// RoutedBackend budget should make the call error well before it fires.
	callCtx, callCancel := context.WithTimeout(ctx, 10*time.Second)
	defer callCancel()

	start := time.Now()
	err := player.Call(callCtx, "characters.list", struct{}{}, nil)
	elapsed := time.Since(start)

	if err == nil {
		t.Fatal("characters.list should error after its backend was closed, got nil")
	}
	if elapsed > 3500*time.Millisecond {
		t.Fatalf("degradation took %v, want < 3.5s (2×1s retry budget)", elapsed)
	}
	t.Logf("graceful degradation: characters.list errored in %v: %v", elapsed, err)

	// Failure is isolated: the inventory route is untouched and still answers.
	var invResp struct {
		Items []string `json:"items"`
	}
	if err := player.Call(ctx, "inventory.list", struct{}{}, &invResp); err != nil {
		t.Fatalf("inventory.list must survive a dead characters backend: %v", err)
	}
	if len(invResp.Items) != 1 || invResp.Items[0] != "starter_sword" {
		t.Fatalf("inventory.list returned wrong payload after degradation: %+v", invResp)
	}
}

// TestLiveSmokeHTTPReverseProxy proves the HTTP front door: NewHTTPProxy mounts
// "/admin/" onto an origin (marked here as the inventory origin) and forwards a
// GET verbatim. The response body must come from that origin and the origin must
// see the full path "/admin/whatever" unchanged (no prefix strip / rewrite).
func TestLiveSmokeHTTPReverseProxy(t *testing.T) {
	sawPath := make(chan string, 1)
	invOrigin := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		sawPath <- r.URL.Path
		_, _ = io.WriteString(w, "inventory-origin-body")
	}))
	defer invOrigin.Close()

	// NewHTTPProxy expects host:port and prepends the http scheme itself; keys
	// are BARE prefixes (registered at both exact + subtree).
	invHost := strings.TrimPrefix(invOrigin.URL, "http://")
	proxy := NewHTTPProxy(map[string]string{"/admin": invHost})

	front := httptest.NewServer(proxy)
	defer front.Close()

	// Both the bare prefix (exact "/admin" — the backend serves "GET /admin", so
	// a trailing-slash rewrite/redirect would 404 it) AND a subtree path must
	// reach the origin verbatim. The bare case is the regression a live run caught.
	for _, path := range []string{"/admin", "/admin/whatever"} {
		resp, err := http.Get(front.URL + path)
		if err != nil {
			t.Fatalf("GET %s: %v", path, err)
		}
		body, err := io.ReadAll(resp.Body)
		_ = resp.Body.Close()
		if err != nil {
			t.Fatalf("read body: %v", err)
		}
		if resp.StatusCode != http.StatusOK {
			t.Fatalf("GET %s: status = %d, want 200 (no trailing-slash redirect)", path, resp.StatusCode)
		}
		if string(body) != "inventory-origin-body" {
			t.Fatalf("GET %s: body = %q, want it from the inventory origin", path, string(body))
		}
		select {
		case p := <-sawPath:
			if p != path {
				t.Fatalf("origin saw path %q, want verbatim %q", p, path)
			}
		default:
			t.Fatalf("GET %s: inventory origin never received the request", path)
		}
	}
}
