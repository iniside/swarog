package gateway

import (
	"context"
	"encoding/json"
	"errors"
	"testing"
	"time"

	"gamebackend/edge"
	"gamebackend/opsapi"
)

// This test validates the OperationBackend abstraction end-to-end with a FAKE
// operation ("test.echo") — no real module wired — mirroring the de-risk pattern
// rpcgen used with its golden. It proves BOTH impls satisfy the contract before
// Phase D migrates any real operation onto it:
//
//   - LocalBackend hands the DECODED request struct to the invoker by reference
//     (pointer identity preserved ⇒ zero wire marshal, decision D3) and returns
//     the typed response the invoker filled.
//   - RemoteBackend round-trips the same operation over a real loopback QUIC edge
//     and maps the response-envelope opsapi.Status onto an *opsapi.Error.

// echoReq / echoResp are the fake operation's typed request/response — pure
// domain shapes, exactly what the gateway would decode from / encode to HTTP.
type echoReq struct {
	Text string `json:"text"`
}

type echoResp struct {
	Echo string `json:"echo"`
}

const echoMethod = "test.echo"

// TestLocalBackend_TypedDispatchNoMarshal proves LocalBackend calls the typed
// invoker with the EXACT decoded request struct (same pointer ⇒ no serialization
// round-trip) and returns the typed response the invoker filled.
func TestLocalBackend_TypedDispatchNoMarshal(t *testing.T) {
	var gotReq *echoReq // the pointer the invoker actually received
	inv := func(_ context.Context, req, resp any) error {
		gotReq = req.(*echoReq)             // exact typed struct, no decode
		resp.(*echoResp).Echo = gotReq.Text // fill the caller's resp directly
		return nil
	}

	lb := NewLocalBackend(map[string]opsapi.LocalInvoker{echoMethod: inv})

	in := &echoReq{Text: "hello"}
	var out echoResp
	op := opsapi.Operation{Method: echoMethod}

	if err := lb.Invoke(context.Background(), op, in, &out); err != nil {
		t.Fatalf("LocalBackend.Invoke: %v", err)
	}
	if gotReq != in {
		t.Fatalf("invoker got a different request pointer (%p != %p) — a marshal/copy happened; local path must be zero-marshal", gotReq, in)
	}
	if out.Echo != "hello" {
		t.Fatalf("resp not filled: Echo = %q, want %q", out.Echo, "hello")
	}
}

// TestLocalBackend_UnknownMethod proves an unbound method is a loud error, not a
// silent nil response.
func TestLocalBackend_UnknownMethod(t *testing.T) {
	lb := NewLocalBackend(nil)
	var out echoResp
	err := lb.Invoke(context.Background(), opsapi.Operation{Method: "nope.nope"}, &echoReq{}, &out)
	if err == nil {
		t.Fatal("Invoke(unknown method): want error, got nil")
	}
}

// echoWire is the rpcgen response-envelope convention the fake provider emits: a
// flat status/err pair alongside the domain fields. RemoteBackend probes
// status/err and unmarshals the domain field (echo) from the same bytes.
type echoWire struct {
	Status opsapi.Status `json:"status"`
	Err    string        `json:"err,omitempty"`
	Echo   string        `json:"echo"`
}

const forbiddenMethod = "test.forbidden"

// startFakeProvider stands up a loopback QUIC edge server serving the fake
// operation, mirroring remote_test.go's loopback pattern. "test.echo" echoes the
// request; "test.forbidden" returns a Forbidden-status envelope (no transport
// error) to exercise the Status→error mapping.
func startFakeProvider(t *testing.T) (addr string, stop func()) {
	t.Helper()

	srv := edge.NewServer()
	srv.Handle(echoMethod, func(reqPayload []byte) ([]byte, error) {
		var req echoReq
		if err := json.Unmarshal(reqPayload, &req); err != nil {
			return nil, err
		}
		return json.Marshal(echoWire{Status: opsapi.StatusOK, Echo: req.Text})
	})
	srv.Handle(forbiddenMethod, func(_ []byte) ([]byte, error) {
		return json.Marshal(echoWire{Status: opsapi.StatusForbidden, Err: "not allowed"})
	})

	tlsConf, err := edge.SelfSignedTLS()
	if err != nil {
		t.Fatalf("tls: %v", err)
	}
	if err := srv.ListenAddr("127.0.0.1:0", tlsConf); err != nil {
		t.Fatalf("listen: %v", err)
	}
	return srv.Addr().String(), func() { _ = srv.Close() }
}

// TestRemoteBackend_RoundTrip proves RemoteBackend marshals the request, relays
// it over the real QUIC edge, and unmarshals the domain response.
func TestRemoteBackend_RoundTrip(t *testing.T) {
	addr, stop := startFakeProvider(t)
	defer stop()

	rb := NewRemoteBackend(addr)

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	var out echoResp
	op := opsapi.Operation{Method: echoMethod}
	if err := rb.Invoke(ctx, op, &echoReq{Text: "over-the-wire"}, &out); err != nil {
		t.Fatalf("RemoteBackend.Invoke: %v", err)
	}
	if out.Echo != "over-the-wire" {
		t.Fatalf("round-trip resp: Echo = %q, want %q", out.Echo, "over-the-wire")
	}
}

// TestRemoteBackend_StatusMapping proves a domain Status carried in the response
// envelope is reconstituted as an *opsapi.Error with that Status (so the gateway
// can later map it onto the right HTTP status instead of collapsing to 500).
func TestRemoteBackend_StatusMapping(t *testing.T) {
	addr, stop := startFakeProvider(t)
	defer stop()

	rb := NewRemoteBackend(addr)

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	var out echoResp
	op := opsapi.Operation{Method: forbiddenMethod}
	err := rb.Invoke(ctx, op, &echoReq{}, &out)
	if err == nil {
		t.Fatal("Invoke(forbidden op): want error, got nil")
	}
	if opsapi.StatusOf(err) != opsapi.StatusForbidden {
		t.Fatalf("StatusOf(err) = %v, want StatusForbidden", opsapi.StatusOf(err))
	}
	var opErr *opsapi.Error
	if !errors.As(err, &opErr) {
		t.Fatalf("err is not *opsapi.Error: %T", err)
	}
}

// TestRemoteBackend_PeerDown proves a transport failure (peer unreachable) maps
// to StatusUnavailable (→ 503), distinct from a domain error.
func TestRemoteBackend_PeerDown(t *testing.T) {
	addr, stop := startFakeProvider(t)
	stop() // kill the provider before any call

	rb := NewRemoteBackend(addr)

	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()

	var out echoResp
	err := rb.Invoke(ctx, opsapi.Operation{Method: echoMethod}, &echoReq{}, &out)
	if err == nil {
		t.Fatal("Invoke(peer down): want error, got nil")
	}
	if opsapi.StatusOf(err) != opsapi.StatusUnavailable {
		t.Fatalf("StatusOf(err) = %v, want StatusUnavailable", opsapi.StatusOf(err))
	}
}
