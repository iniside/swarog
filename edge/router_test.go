package edge

import (
	"bytes"
	"context"
	"testing"
	"time"
)

// startServer stands up a Server on an ephemeral loopback port, applies the
// caller's registrations, starts listening, and returns the bound address plus a
// cleanup. It mirrors startTestServer but lets each test wire its own handlers.
func startServer(t *testing.T, register func(*Server)) (addr string, cleanup func()) {
	t.Helper()

	srv := NewServer()
	register(srv)

	tlsConf, err := SelfSignedTLS()
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

func dial(t *testing.T, ctx context.Context, addr string) *Client {
	t.Helper()
	cli, err := Dial(ctx, addr, ClientTLS())
	if err != nil {
		t.Fatalf("Dial %s: %v", addr, err)
	}
	return cli
}

// TestPrefixForwardRoundTrip proves method-aware forwarding under original
// names: a front server A relays the whole "x." family to a backend server B via
// CallRaw, and B's exact "x.echo" handler answers. The method name survives the
// hop (fwd receives it), so B sees "x.echo", not a rewritten name.
func TestPrefixForwardRoundTrip(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()

	// Backend B: exact handler that echoes and asserts it saw the original name.
	backendAddr, stopB := startServer(t, func(s *Server) {
		s.Handle("x.echo", func(reqPayload []byte) ([]byte, error) {
			return s.codec.Encode(echoResp{Msg: "B:" + string(reqPayload)})
		})
	})
	defer stopB()

	// Front A: dials B once, forwards "x." to it, relaying method + payload.
	toB := dial(t, ctx, backendAddr)
	defer func() { _ = toB.Close() }()

	frontAddr, stopA := startServer(t, func(s *Server) {
		s.HandlePrefix("x.", func(method string, payload []byte) ([]byte, error) {
			return toB.CallRaw(ctx, method, payload)
		})
	})
	defer stopA()

	// Player dials A and calls x.echo; the response originates in B.
	player := dial(t, ctx, frontAddr)
	defer func() { _ = player.Close() }()

	var resp echoResp
	if err := player.Call(ctx, "x.echo", []byte(`payload`), &resp); err != nil {
		t.Fatalf("Call x.echo through front: %v", err)
	}
	// The raw request payload is the JSON-encoded []byte("payload"), i.e. a
	// base64 string; what matters is that B produced the answer.
	if !bytes.HasPrefix([]byte(resp.Msg), []byte("B:")) {
		t.Fatalf("expected response from backend B, got %q", resp.Msg)
	}
}

// TestLongestPrefixWins registers both "x." and "x.admin." and asserts that
// x.admin.foo routes to the longer, more specific prefix.
func TestLongestPrefixWins(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()

	addr, stop := startServer(t, func(s *Server) {
		s.HandlePrefix("x.", func(method string, payload []byte) ([]byte, error) {
			return s.codec.Encode(echoResp{Msg: "short"})
		})
		s.HandlePrefix("x.admin.", func(method string, payload []byte) ([]byte, error) {
			return s.codec.Encode(echoResp{Msg: "long"})
		})
	})
	defer stop()

	cli := dial(t, ctx, addr)
	defer func() { _ = cli.Close() }()

	var resp echoResp
	if err := cli.Call(ctx, "x.admin.foo", struct{}{}, &resp); err != nil {
		t.Fatalf("Call x.admin.foo: %v", err)
	}
	if resp.Msg != "long" {
		t.Fatalf("longest prefix should win: got %q want %q", resp.Msg, "long")
	}
}

// TestExactBeatsPrefix asserts an exact Handle wins over a covering HandlePrefix.
func TestExactBeatsPrefix(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()

	addr, stop := startServer(t, func(s *Server) {
		s.Handle("x.echo", func(reqPayload []byte) ([]byte, error) {
			return s.codec.Encode(echoResp{Msg: "exact"})
		})
		s.HandlePrefix("x.", func(method string, payload []byte) ([]byte, error) {
			return s.codec.Encode(echoResp{Msg: "prefix"})
		})
	})
	defer stop()

	cli := dial(t, ctx, addr)
	defer func() { _ = cli.Close() }()

	var resp echoResp
	if err := cli.Call(ctx, "x.echo", struct{}{}, &resp); err != nil {
		t.Fatalf("Call x.echo: %v", err)
	}
	if resp.Msg != "exact" {
		t.Fatalf("exact handler should win over prefix: got %q want %q", resp.Msg, "exact")
	}
}

// TestCallRawNoDoubleEncode proves the relay does not re-wrap the payload: the
// bytes handed to the backend handler are byte-identical to what the caller
// passed to CallRaw (raw JSON in, raw JSON out — no extra encode/decode).
func TestCallRawNoDoubleEncode(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()

	// A distinctive raw JSON payload; the handler captures exactly what it got.
	raw := []byte(`{"hello":"world","n":42}`)
	got := make(chan []byte, 1)

	addr, stop := startServer(t, func(s *Server) {
		s.Handle("cap", func(reqPayload []byte) ([]byte, error) {
			got <- append([]byte(nil), reqPayload...)
			return reqPayload, nil
		})
	})
	defer stop()

	cli := dial(t, ctx, addr)
	defer func() { _ = cli.Close() }()

	respPayload, err := cli.CallRaw(ctx, "cap", raw)
	if err != nil {
		t.Fatalf("CallRaw: %v", err)
	}

	select {
	case seen := <-got:
		if !bytes.Equal(seen, raw) {
			t.Fatalf("backend saw re-wrapped payload: got %q want %q", seen, raw)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("backend handler never ran")
	}

	// And the response bytes come back verbatim too.
	if !bytes.Equal(respPayload, raw) {
		t.Fatalf("response payload not verbatim: got %q want %q", respPayload, raw)
	}
}
