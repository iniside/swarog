package edge

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"
	"testing"
	"time"
)

// echoReq/echoResp are a trivial typed payload to prove typed round-tripping
// over real QUIC (encode -> frame -> UDP -> frame -> decode and back).
type echoReq struct {
	Msg string `json:"msg"`
}

type echoResp struct {
	Msg string `json:"msg"`
}

// startTestServer spins up a Server on an ephemeral localhost UDP port with an
// "echo" handler (returns its input) and a "boom" handler (always errors). It
// returns the bound address and a cleanup func.
func startTestServer(t *testing.T) (addr string, cleanup func()) {
	t.Helper()

	srv := NewServer()

	srv.Handle("echo", func(reqPayload []byte) ([]byte, error) {
		var in echoReq
		if err := srv.codec.Decode(reqPayload, &in); err != nil {
			return nil, err
		}
		return srv.codec.Encode(echoResp{Msg: in.Msg})
	})

	srv.Handle("boom", func([]byte) ([]byte, error) {
		return nil, errors.New("boom: handler failed on purpose")
	})

	srv.Handle("panic", func([]byte) ([]byte, error) {
		panic("kaboom")
	})

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

func TestEchoRoundTrip(t *testing.T) {
	addr, cleanup := startTestServer(t)
	defer cleanup()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	cli, err := Dial(ctx, addr, ClientTLS())
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer func() { _ = cli.Close() }()

	var resp echoResp
	if err := cli.Call(ctx, "echo", echoReq{Msg: "hello quic"}, &resp); err != nil {
		t.Fatalf("Call echo: %v", err)
	}
	if resp.Msg != "hello quic" {
		t.Fatalf("echo mismatch: got %q want %q", resp.Msg, "hello quic")
	}
}

func TestUnknownMethod(t *testing.T) {
	addr, cleanup := startTestServer(t)
	defer cleanup()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	cli, err := Dial(ctx, addr, ClientTLS())
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer func() { _ = cli.Close() }()

	var resp echoResp
	err = cli.Call(ctx, "does-not-exist", echoReq{Msg: "x"}, &resp)
	if err == nil {
		t.Fatal("expected error for unknown method, got nil")
	}
	if !strings.Contains(err.Error(), "unknown method") {
		t.Fatalf("unexpected error for unknown method: %v", err)
	}
}

func TestHandlerError(t *testing.T) {
	addr, cleanup := startTestServer(t)
	defer cleanup()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	cli, err := Dial(ctx, addr, ClientTLS())
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer func() { _ = cli.Close() }()

	var resp echoResp
	err = cli.Call(ctx, "boom", echoReq{Msg: "x"}, &resp)
	if err == nil {
		t.Fatal("expected error from boom handler, got nil")
	}
	if !strings.Contains(err.Error(), "handler failed on purpose") {
		t.Fatalf("unexpected error from boom: %v", err)
	}
}

func TestHandlerPanicSurfacesAsError(t *testing.T) {
	addr, cleanup := startTestServer(t)
	defer cleanup()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	cli, err := Dial(ctx, addr, ClientTLS())
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer func() { _ = cli.Close() }()

	var resp echoResp
	err = cli.Call(ctx, "panic", echoReq{Msg: "x"}, &resp)
	if err == nil {
		t.Fatal("expected error from panicking handler, got nil")
	}
	if !strings.Contains(err.Error(), "handler panic") {
		t.Fatalf("unexpected error from panic handler: %v", err)
	}
}

// TestConcurrentCallsOneConn proves stream multiplexing: many concurrent Calls
// over a SINGLE persistent client connection, each getting its own correct
// response.
func TestConcurrentCallsOneConn(t *testing.T) {
	addr, cleanup := startTestServer(t)
	defer cleanup()

	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()

	cli, err := Dial(ctx, addr, ClientTLS())
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	defer func() { _ = cli.Close() }()

	const n = 32
	var wg sync.WaitGroup
	errs := make([]error, n)
	for i := range n {
		wg.Go(func() {
			want := fmt.Sprintf("msg-%d", i)
			var resp echoResp
			if err := cli.Call(ctx, "echo", echoReq{Msg: want}, &resp); err != nil {
				errs[i] = err
				return
			}
			if resp.Msg != want {
				errs[i] = fmt.Errorf("got %q want %q", resp.Msg, want)
			}
		})
	}
	wg.Wait()

	for i, e := range errs {
		if e != nil {
			t.Fatalf("concurrent call %d failed: %v", i, e)
		}
	}
}
