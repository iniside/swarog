package characters

import (
	"bytes"
	"context"
	"encoding/json"
	"testing"
	"time"

	"gamebackend/edge"
	"gamebackend/modules/characters/charactersapi"
	"gamebackend/modules/characters/charactersplayerrpc"
	"gamebackend/opsapi"
)

// This is the load-bearing proof of the unified operation transport: for every
// representative op SHAPE, dispatching a generated operation through the LOCAL path
// (Decode → in-process invoker → Encode, what gateway.LocalBackend does) and the
// REMOTE path (Decode → marshal wire request → QUIC edge → generated RegisterServer
// → unmarshal wire response → Encode, what gateway.RemoteBackend does) must produce
// the IDENTICAL external HTTP body + Status. Both paths use the SAME generated
// bindings and the SAME impl, so they are equal by construction — this test would
// FAIL under the pre-fix bug where the hand-written binding types (deleteReq{id
// unexported}, list's bare []Character) did not match the wire envelopes.
//
// Shapes covered here: create (multi-field body → struct return), list (no args →
// array return), delete (path arg → no body, incl. a not-found domain Status over
// the wire). accounts/parity_test.go covers the {player_id, token} struct return.

// parityPlayer is a deterministic in-memory charactersapi.Player: Create echoes the
// caller identity (proving it crosses the wire), List returns a fixed pair, Delete
// distinguishes a real delete from a not-found (a domain Status that must ride the
// envelope identically on both paths).
type parityPlayer struct{}

func (parityPlayer) Create(ctx context.Context, name, class string) (charactersapi.Character, error) {
	pid, _ := opsapi.PlayerID(ctx)
	return charactersapi.Character{ID: "c-new", PlayerID: pid, Name: name, Class: class, CreatedAt: time.Time{}}, nil
}

func (parityPlayer) List(ctx context.Context) ([]charactersapi.Character, error) {
	pid, _ := opsapi.PlayerID(ctx)
	return []charactersapi.Character{
		{ID: "char-1", PlayerID: pid, Name: "Aria", Class: "mage"},
		{ID: "char-2", PlayerID: pid, Name: "Bolt", Class: "rogue"},
	}, nil
}

func (parityPlayer) Delete(_ context.Context, characterID string) error {
	if characterID == "char-1" {
		return nil
	}
	return &opsapi.Error{Status: opsapi.StatusNotFound, Msg: "character not found"}
}

// driveLocal runs the operation through the in-process path (gateway.LocalBackend):
// Decode the HTTP body/path into the wire request envelope, invoke the provider
// in-process filling the wire response envelope, then Encode to the external body.
func driveLocal(t *testing.T, op opsapi.OpSet, identity string, body []byte, path map[string]string) ([]byte, opsapi.Status) {
	t.Helper()
	ctx := context.Background()
	if identity != "" {
		ctx = opsapi.WithPlayerID(ctx, identity)
	}
	req, err := op.Binding.Decode(body, path)
	if err != nil {
		t.Fatalf("local decode: %v", err)
	}
	resp := op.Binding.NewResp()
	if err := op.Local.Invoke(ctx, req, resp); err != nil {
		t.Fatalf("local invoke: %v", err)
	}
	outBody, status, _ := op.Binding.Encode(resp)
	return outBody, status
}

// driveRemote runs the operation through the cross-process path
// (gateway.RemoteBackend): Decode, marshal the wire request envelope, relay it over
// the real QUIC edge to the generated RegisterServer, unmarshal the reply into the
// wire response envelope, then Encode — the exact sequence RemoteBackend performs.
func driveRemote(t *testing.T, cl *edge.Client, op opsapi.OpSet, identity string, body []byte, path map[string]string) ([]byte, opsapi.Status) {
	t.Helper()
	req, err := op.Binding.Decode(body, path)
	if err != nil {
		t.Fatalf("remote decode: %v", err)
	}
	reqBytes, err := json.Marshal(req)
	if err != nil {
		t.Fatalf("remote marshal: %v", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	respBytes, err := cl.CallRawID(ctx, op.Operation.Method, identity, reqBytes)
	if err != nil {
		t.Fatalf("remote CallRawID: %v", err)
	}
	resp := op.Binding.NewResp()
	if err := json.Unmarshal(respBytes, resp); err != nil {
		t.Fatalf("remote unmarshal: %v", err)
	}
	outBody, status, _ := op.Binding.Encode(resp)
	return outBody, status
}

func TestLocalRemoteParity(t *testing.T) {
	impl := parityPlayer{}
	ops := charactersplayerrpc.Operations(impl)

	// Loopback edge server exposing the SAME impl via the generated RegisterServer.
	srv := edge.NewServer()
	charactersplayerrpc.RegisterServer(srv, impl)
	stls, err := edge.ServerMTLS()
	if err != nil {
		t.Fatalf("server tls: %v", err)
	}
	if err := srv.ListenAddr("127.0.0.1:0", stls); err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer func() { _ = srv.Close() }()

	ctls, err := edge.ClientMTLS()
	if err != nil {
		t.Fatalf("client tls: %v", err)
	}
	dctx, dcancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer dcancel()
	cl, err := edge.Dial(dctx, srv.Addr().String(), ctls)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer func() { _ = cl.Close() }()

	cases := []struct {
		name       string
		method     string
		identity   string
		body       []byte
		path       map[string]string
		wantStatus opsapi.Status
	}{
		{
			name:       "create (multi-field body → struct return)",
			method:     charactersplayerrpc.MethodCreate,
			identity:   "player-1",
			body:       []byte(`{"name":"Aria","class":"mage"}`),
			wantStatus: opsapi.StatusOK,
		},
		{
			name:       "list (no args → array return)",
			method:     charactersplayerrpc.MethodList,
			identity:   "player-1",
			wantStatus: opsapi.StatusOK,
		},
		{
			name:       "delete (path arg → no body, found)",
			method:     charactersplayerrpc.MethodDelete,
			identity:   "player-1",
			path:       map[string]string{"id": "char-1"},
			wantStatus: opsapi.StatusOK,
		},
		{
			name:       "delete (path arg → not found, domain Status over the wire)",
			method:     charactersplayerrpc.MethodDelete,
			identity:   "player-1",
			path:       map[string]string{"id": "ghost"},
			wantStatus: opsapi.StatusNotFound,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			op := ops[tc.method]
			lBody, lStatus := driveLocal(t, op, tc.identity, tc.body, tc.path)
			rBody, rStatus := driveRemote(t, cl, op, tc.identity, tc.body, tc.path)

			if lStatus != rStatus {
				t.Fatalf("status mismatch: local=%v remote=%v", lStatus, rStatus)
			}
			if lStatus != tc.wantStatus {
				t.Fatalf("status = %v, want %v", lStatus, tc.wantStatus)
			}
			if !bytes.Equal(lBody, rBody) {
				t.Fatalf("HTTP body mismatch:\n local=%s\nremote=%s", lBody, rBody)
			}
			// Sanity: the create body carries the identity that crossed the wire.
			if tc.method == charactersplayerrpc.MethodCreate && !bytes.Contains(rBody, []byte(`"player_id":"player-1"`)) {
				t.Fatalf("create body did not carry the wire identity: %s", rBody)
			}
		})
	}
}
