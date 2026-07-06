package gateway

import (
	"bytes"
	"testing"
	"time"

	"gamebackend/edge"
)

// startBackend stands up an edge.Server on an ephemeral loopback port with the
// caller's registrations and returns its bound address plus a stop func.
func startBackend(t *testing.T, register func(*edge.Server)) (addr string, stop func()) {
	t.Helper()

	srv := edge.NewServer()
	register(srv)

	tlsConf, err := edge.SelfSignedTLS()
	if err != nil {
		t.Fatalf("SelfSignedTLS: %v", err)
	}
	if err := srv.ListenAddr("127.0.0.1:0", tlsConf); err != nil {
		t.Fatalf("ListenAddr: %v", err)
	}
	bound := srv.Addr()
	if bound == nil {
		t.Fatal("server Addr is nil after ListenAddr")
	}
	return bound.String(), func() { _ = srv.Close() }
}

// TestForwardHappyPath: a RoutedBackend pointed at a live backend relays the call
// and returns the backend's response payload verbatim.
func TestForwardHappyPath(t *testing.T) {
	raw := []byte(`{"id":"abc"}`)
	want := []byte(`{"player_id":"p1","ok":true}`)

	addr, stop := startBackend(t, func(s *edge.Server) {
		s.Handle("characters.ownerOf", func(reqPayload []byte) ([]byte, error) {
			if !bytes.Equal(reqPayload, raw) {
				t.Errorf("backend saw %q, want %q", reqPayload, raw)
			}
			return want, nil
		})
	})
	defer stop()

	rb := NewRoutedBackend(addr)
	defer func() { _ = rb.Close() }()

	got, err := rb.Forward("characters.ownerOf", raw)
	if err != nil {
		t.Fatalf("Forward: %v", err)
	}
	if !bytes.Equal(got, want) {
		t.Fatalf("Forward returned %q, want %q", got, want)
	}
}

// TestForwardDeadBackendDegrades: a RoutedBackend pointed at a backend that is
// then closed returns an error within a bounded time (never hangs/panics), while
// a second, healthy RoutedBackend keeps working — a dead peer must not take the
// gateway down.
func TestForwardDeadBackendDegrades(t *testing.T) {
	// A live backend we close before forwarding, so the dial fails fast.
	deadAddr, stopDead := startBackend(t, func(s *edge.Server) {
		s.Handle("inventory.list", func(_ []byte) ([]byte, error) { return []byte(`[]`), nil })
	})
	stopDead() // peer is now gone

	dead := NewRoutedBackend(deadAddr)
	defer func() { _ = dead.Close() }()

	done := make(chan error, 1)
	go func() { _, err := dead.Forward("inventory.list", []byte(`{}`)); done <- err }()

	select {
	case err := <-done:
		if err == nil {
			t.Fatal("Forward to a dead backend should error, got nil")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Forward to a dead backend hung past 5s (retry budget should bound it)")
	}

	// A healthy backend routed independently is unaffected.
	okAddr, stopOK := startBackend(t, func(s *edge.Server) {
		s.Handle("characters.list", func(_ []byte) ([]byte, error) { return []byte(`["c1"]`), nil })
	})
	defer stopOK()

	ok := NewRoutedBackend(okAddr)
	defer func() { _ = ok.Close() }()

	got, err := ok.Forward("characters.list", []byte(`{}`))
	if err != nil {
		t.Fatalf("healthy backend Forward failed: %v", err)
	}
	if !bytes.Equal(got, []byte(`["c1"]`)) {
		t.Fatalf("healthy backend returned %q", got)
	}
}
