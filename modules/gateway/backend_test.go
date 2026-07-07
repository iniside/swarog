package gateway

import (
	"context"
	"encoding/json"
	"testing"
	"time"

	"gamebackend/edge"
	"gamebackend/opsapi"
)

// This test validates the OperationBackend abstraction end-to-end with a FAKE
// operation ("test.echo") — no real module wired — mirroring the de-risk pattern
// rpcgen used with its golden. It proves BOTH impls satisfy the contract:
//
//   - LocalBackend hands the DECODED request struct to the invoker by reference
//     (pointer identity preserved ⇒ zero wire marshal, decision D3) and the invoker
//     fills the WIRE RESPONSE ENVELOPE directly.
//   - RemoteBackend round-trips the same operation over a real loopback QUIC edge
//     and unmarshals the peer's reply straight into the SAME wire response envelope.
//
// Both leave an identical filled envelope (Status + domain fields), which the
// gateway's Encode reduces to the external HTTP body — the whole point of the
// unified transport: Local == Remote by construction. The real generated bindings
// are exercised by the module-level parity tests (arch-lint forbids the gateway
// package from importing a module's rpc glue), so here the envelope is inline.

// echoReq is the fake op's wire request envelope.
type echoReq struct {
	Text string `json:"text"`
}

// echoResp is the fake op's wire RESPONSE ENVELOPE: the {status, err, <domain>}
// shape rpcgen generates. Both backends fill/consume it identically.
type echoResp struct {
	Status opsapi.Status `json:"status"`
	Err    string        `json:"err,omitempty"`
	Echo   string        `json:"echo"`
}

const echoMethod = "test.echo"

// TestLocalBackend_TypedDispatchNoMarshal proves LocalBackend calls the typed
// invoker with the EXACT decoded request struct (same pointer ⇒ no serialization
// round-trip) and the invoker fills the wire response envelope the caller passed.
func TestLocalBackend_TypedDispatchNoMarshal(t *testing.T) {
	var gotReq *echoReq // the pointer the invoker actually received
	inv := func(_ context.Context, req, resp any) error {
		gotReq = req.(*echoReq) // exact typed struct, no decode
		out := resp.(*echoResp)
		out.Status = opsapi.StatusOK
		out.Echo = gotReq.Text // fill the caller's envelope directly
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
	if out.Echo != "hello" || out.Status != opsapi.StatusOK {
		t.Fatalf("resp not filled: %+v", out)
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

const forbiddenMethod = "test.forbidden"

// startFakeProvider stands up a loopback QUIC edge server serving the fake
// operation, mirroring remote_test.go's loopback pattern. "test.echo" echoes the
// request in an OK envelope; "test.forbidden" returns a Forbidden-status envelope
// (no transport error) to exercise the envelope Status crossing the wire.
func startFakeProvider(t *testing.T) (addr string, stop func()) {
	t.Helper()

	srv := edge.NewServer()
	srv.Handle(echoMethod, func(reqPayload []byte) ([]byte, error) {
		var req echoReq
		if err := json.Unmarshal(reqPayload, &req); err != nil {
			return nil, err
		}
		return json.Marshal(echoResp{Status: opsapi.StatusOK, Echo: req.Text})
	})
	srv.Handle(forbiddenMethod, func(_ []byte) ([]byte, error) {
		return json.Marshal(echoResp{Status: opsapi.StatusForbidden, Err: "not allowed"})
	})

	tlsConf, err := edge.ServerMTLS()
	if err != nil {
		t.Fatalf("tls: %v", err)
	}
	if err := srv.ListenAddr("127.0.0.1:0", tlsConf); err != nil {
		t.Fatalf("listen: %v", err)
	}
	return srv.Addr().String(), func() { _ = srv.Close() }
}

// TestRemoteBackend_RoundTrip proves RemoteBackend marshals the wire request
// envelope, relays it over the real QUIC edge, and unmarshals the whole response
// envelope (Status + domain field) into resp.
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
	if out.Echo != "over-the-wire" || out.Status != opsapi.StatusOK {
		t.Fatalf("round-trip resp: %+v", out)
	}
}

// TestRemoteBackend_DomainStatusRidesEnvelope proves a domain Status set by the
// peer rides the response envelope into resp (Invoke itself succeeds — it is not a
// transport failure), so the gateway's Encode later maps it to the right HTTP
// status. This is the fix: the outcome is generic (in the envelope), not per-op.
func TestRemoteBackend_DomainStatusRidesEnvelope(t *testing.T) {
	addr, stop := startFakeProvider(t)
	defer stop()

	rb := NewRemoteBackend(addr)

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	var out echoResp
	op := opsapi.Operation{Method: forbiddenMethod}
	if err := rb.Invoke(ctx, op, &echoReq{}, &out); err != nil {
		t.Fatalf("Invoke(forbidden op): want nil transport err, got %v", err)
	}
	if out.Status != opsapi.StatusForbidden {
		t.Fatalf("out.Status = %v, want StatusForbidden (carried in the envelope)", out.Status)
	}
	if out.Err != "not allowed" {
		t.Fatalf("out.Err = %q, want %q", out.Err, "not allowed")
	}
}

// TestRemoteBackend_PeerDown proves a transport failure (peer unreachable) maps
// to StatusUnavailable (→ 503), distinct from a domain error carried in an envelope.
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
