package edge

import (
	"context"
	"crypto/tls"
	"testing"
	"time"
)

// mtlsServer stands up a loopback edge.Server whose TLS is built from ca (mutual
// TLS: requires+verifies a client cert). It serves a trivial "ping" and returns
// the bound address plus a stop func.
func mtlsServer(t *testing.T, ca *DevCA) (addr string, stop func()) {
	t.Helper()
	srv := NewServer()
	srv.Handle("ping", func([]byte) ([]byte, error) { return []byte(`"pong"`), nil })

	conf, err := ca.ServerTLS()
	if err != nil {
		t.Fatalf("ServerTLS: %v", err)
	}
	if err := srv.ListenAddr("127.0.0.1:0", conf); err != nil {
		t.Fatalf("ListenAddr: %v", err)
	}
	return srv.Addr().String(), func() { _ = srv.Close() }
}

// tryPing dials addr with clientConf and calls "ping", returning the first error
// from either the handshake (Dial) or the call. A rejected mutual-TLS handshake
// surfaces here as a non-nil error — the whole point of the negative tests.
func tryPing(addr string, clientConf *tls.Config) error {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	cli, err := Dial(ctx, addr, clientConf)
	if err != nil {
		return err
	}
	defer func() { _ = cli.Close() }()
	return cli.Call(ctx, "ping", struct{}{}, nil)
}

// TestMTLS_ProperClientSucceeds is the POSITIVE case: a client presenting a leaf
// signed by the SAME CA as the server both authenticates and verifies — existing
// behavior (a real cross-process edge call) is preserved under mTLS.
func TestMTLS_ProperClientSucceeds(t *testing.T) {
	ca, err := GenerateDevCA()
	if err != nil {
		t.Fatalf("GenerateDevCA: %v", err)
	}
	addr, stop := mtlsServer(t, ca)
	defer stop()

	clientConf, err := ca.ClientTLS()
	if err != nil {
		t.Fatalf("ClientTLS: %v", err)
	}
	if err := tryPing(addr, clientConf); err != nil {
		t.Fatalf("proper mTLS client should succeed, got: %v", err)
	}
}

// TestMTLS_NoClientCertRejected is the CORE negative case that closes the
// impersonation hole: a client that reaches the port but presents NO client
// certificate (it still trusts the server via RootCAs) is rejected at the TLS
// handshake. Pre-mTLS this dial would have succeeded and could call any method.
func TestMTLS_NoClientCertRejected(t *testing.T) {
	ca, err := GenerateDevCA()
	if err != nil {
		t.Fatalf("GenerateDevCA: %v", err)
	}
	addr, stop := mtlsServer(t, ca)
	defer stop()

	// Trusts the server (RootCAs) but offers NO client certificate.
	noCert := &tls.Config{
		RootCAs:    ca.pool,
		NextProtos: []string{alpnProto},
		MinVersion: tls.VersionTLS13,
	}
	if err := tryPing(addr, noCert); err == nil {
		t.Fatal("a client with NO certificate MUST be rejected by the mTLS server, but the call succeeded — impersonation hole open")
	}
}

// TestMTLS_UntrustedClientCertRejected: a client presenting a leaf signed by a
// DIFFERENT CA (not the server's trust anchor) is rejected — a self-minted cert
// does not grant access.
func TestMTLS_UntrustedClientCertRejected(t *testing.T) {
	serverCA, err := GenerateDevCA()
	if err != nil {
		t.Fatalf("GenerateDevCA server: %v", err)
	}
	otherCA, err := GenerateDevCA()
	if err != nil {
		t.Fatalf("GenerateDevCA other: %v", err)
	}
	addr, stop := mtlsServer(t, serverCA)
	defer stop()

	// A client leaf signed by an UNTRUSTED CA. To isolate the client-auth
	// rejection (not the client's own server check) we still trust the real
	// server via RootCAs, but present otherCA's client leaf.
	otherClient, err := otherCA.ClientTLS()
	if err != nil {
		t.Fatalf("otherCA.ClientTLS: %v", err)
	}
	otherClient.RootCAs = serverCA.pool
	if err := tryPing(addr, otherClient); err == nil {
		t.Fatal("a client cert signed by an untrusted CA MUST be rejected, but the call succeeded")
	}
}

// TestMTLS_ClientRejectsUntrustedServer proves the InsecureSkipVerify removal:
// a proper client (CA-A leaf, RootCAs=CA-A) dialing a server whose cert chains to
// a DIFFERENT CA rejects the server, rather than blindly trusting it.
func TestMTLS_ClientRejectsUntrustedServer(t *testing.T) {
	clientCA, err := GenerateDevCA()
	if err != nil {
		t.Fatalf("GenerateDevCA client: %v", err)
	}
	serverCA, err := GenerateDevCA()
	if err != nil {
		t.Fatalf("GenerateDevCA server: %v", err)
	}
	addr, stop := mtlsServer(t, serverCA)
	defer stop()

	clientConf, err := clientCA.ClientTLS() // RootCAs = clientCA, which did NOT sign the server
	if err != nil {
		t.Fatalf("clientCA.ClientTLS: %v", err)
	}
	if err := tryPing(addr, clientConf); err == nil {
		t.Fatal("client MUST reject a server whose cert does not chain to its CA (no InsecureSkipVerify), but the call succeeded")
	}
}
