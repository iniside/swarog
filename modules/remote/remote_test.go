package remote

import (
	"context"
	"errors"
	"testing"
	"time"

	"gamebackend/edge"
	"gamebackend/modules/accounts/accountsrpc"
	"gamebackend/modules/characters/charactersrpc"
)

// fakeOwnership is an in-memory charactersapi.Ownership: a real owner, a genuine
// not-found, and a store failure — the three OwnerOf outcomes the client maps.
type fakeOwnership struct{}

func (fakeOwnership) OwnerOf(_ context.Context, characterID string) (string, bool, error) {
	switch characterID {
	case "char-1":
		return "player-1", true, nil
	case "boom":
		return "", false, errors.New("store exploded") // provider-side failure → client err
	default:
		return "", false, nil // genuine not-found
	}
}

// fakeSessions is an in-memory accountsapi.Sessions: one valid token, everything
// else an (unknown/expired) invalid session.
type fakeSessions struct{}

func (fakeSessions) VerifySession(_ context.Context, token string) (string, bool, error) {
	if token == "good-token" {
		return "player-9", true, nil
	}
	return "", false, nil
}

// startFakeProvider stands up a loopback QUIC edge server that mimics process A:
// it serves "characters.ownerOf" and "accounts.verifySession" via the SAME
// rpcgen-generated RegisterServer the real providers use (no Postgres). It
// returns the dial address and a stop func — proving client and server glue,
// both generated from the one interface, interoperate over a real edge hop.
func startFakeProvider(t *testing.T) (addr string, stop func()) {
	t.Helper()

	srv := edge.NewServer()
	charactersrpc.RegisterServer(srv, fakeOwnership{})
	accountsrpc.RegisterServer(srv, fakeSessions{})

	tlsConf, err := edge.ServerMTLS()
	if err != nil {
		t.Fatalf("tls: %v", err)
	}
	if err := srv.ListenAddr("127.0.0.1:0", tlsConf); err != nil {
		t.Fatalf("listen: %v", err)
	}
	return srv.Addr().String(), func() { _ = srv.Close() }
}

func TestCharactersClient_OwnerOf(t *testing.T) {
	addr, stop := startFakeProvider(t)
	defer stop()

	conn := &edgeConn{peerAddr: addr}
	cc := charactersrpc.NewClient(conn)
	defer func() { _ = conn.close() }()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	// Real owner → (playerID, true, nil).
	pid, ok, err := cc.OwnerOf(ctx, "char-1")
	if err != nil || !ok || pid != "player-1" {
		t.Fatalf("OwnerOf(char-1) = (%q,%v,%v), want (player-1,true,nil)", pid, ok, err)
	}

	// Genuine not-found → ("", false, nil): NOT an error (stays a 404, not 503).
	pid, ok, err = cc.OwnerOf(ctx, "ghost")
	if err != nil || ok || pid != "" {
		t.Fatalf("OwnerOf(ghost) = (%q,%v,%v), want (\"\",false,nil)", pid, ok, err)
	}

	// Provider-side store failure → non-nil error (surfaces as 503 at consumer).
	if _, _, err = cc.OwnerOf(ctx, "boom"); err == nil {
		t.Fatal("OwnerOf(boom): want error, got nil")
	}
}

func TestAccountsClient_VerifySession(t *testing.T) {
	addr, stop := startFakeProvider(t)
	defer stop()

	conn := &edgeConn{peerAddr: addr}
	ac := accountsrpc.NewClient(conn)
	defer func() { _ = conn.close() }()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	pid, ok, err := ac.VerifySession(ctx, "good-token")
	if err != nil || !ok || pid != "player-9" {
		t.Fatalf("VerifySession(good) = (%q,%v,%v), want (player-9,true,nil)", pid, ok, err)
	}

	pid, ok, err = ac.VerifySession(ctx, "bad-token")
	if err != nil || ok || pid != "" {
		t.Fatalf("VerifySession(bad) = (%q,%v,%v), want (\"\",false,nil)", pid, ok, err)
	}
}

// TestPeerDown_Errors proves the 503 path: once the provider is gone, a call
// through a previously-live connection re-dials, fails, and returns an error
// (rather than a false not-found).
func TestPeerDown_Errors(t *testing.T) {
	addr, stop := startFakeProvider(t)

	conn := &edgeConn{peerAddr: addr}
	cc := charactersrpc.NewClient(conn)
	defer func() { _ = conn.close() }()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	// Warm the persistent connection with a successful call.
	if _, _, err := cc.OwnerOf(ctx, "char-1"); err != nil {
		t.Fatalf("warm-up call failed: %v", err)
	}

	// Kill the provider, then call again: reconnect-and-retry must fail → error.
	stop()

	failCtx, cancel2 := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel2()
	if _, _, err := cc.OwnerOf(failCtx, "char-1"); err == nil {
		t.Fatal("call after peer down: want error (→503), got nil")
	}
}
