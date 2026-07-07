package accounts

import (
	"bytes"
	"context"
	"encoding/json"
	"testing"
	"time"

	"gamebackend/edge"
	"gamebackend/modules/accounts/accountsapi"
	"gamebackend/modules/accounts/accountsauthrpc"
	"gamebackend/opsapi"
)

// Companion to characters/parity_test.go: proves the generated accounts.Auth
// bindings produce the IDENTICAL external HTTP body + Status on the LocalBackend
// and RemoteBackend paths. Shapes here: register (multi-field body → {player_id,
// token} struct return, AuthNone), loginEpic (a body key remapped item — the public
// "id_token" ≠ the param idToken — round-tripped over the wire), and me (AuthPlayer
// → a FLATTENED {player_id, display_name, identities} struct return, MeView).

type stubAuth struct{}

func (stubAuth) Register(_ context.Context, _, _, _ string) (accountsapi.Session, error) {
	return accountsapi.Session{PlayerID: "p-1", Token: "tok-register"}, nil
}
func (stubAuth) Login(_ context.Context, _, _ string) (accountsapi.Session, error) {
	return accountsapi.Session{PlayerID: "p-1", Token: "tok-login"}, nil
}
func (stubAuth) LoginEpic(_ context.Context, idToken string) (accountsapi.Session, error) {
	// Echo the received id_token into the token, so a body-key remap error (public
	// "id_token" not reaching the wire field) would surface as a mismatch.
	return accountsapi.Session{PlayerID: "p-epic", Token: "tok:" + idToken}, nil
}
func (stubAuth) Me(ctx context.Context) (accountsapi.MeView, error) {
	pid, _ := opsapi.PlayerID(ctx)
	return accountsapi.MeView{
		Player:     accountsapi.Player{ID: pid, DisplayName: "Ann"},
		Identities: []accountsapi.Identity{{Provider: "dev", Subject: "ann@example.com"}},
	}, nil
}

func driveLocalAuth(t *testing.T, op opsapi.OpSet, identity string, body []byte) ([]byte, opsapi.Status) {
	t.Helper()
	ctx := context.Background()
	if identity != "" {
		ctx = opsapi.WithPlayerID(ctx, identity)
	}
	req, err := op.Binding.Decode(body, nil)
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

func driveRemoteAuth(t *testing.T, cl *edge.Client, op opsapi.OpSet, identity string, body []byte) ([]byte, opsapi.Status) {
	t.Helper()
	req, err := op.Binding.Decode(body, nil)
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

func TestAuthLocalRemoteParity(t *testing.T) {
	impl := stubAuth{}
	ops := accountsauthrpc.Operations(impl)

	srv := edge.NewServer()
	accountsauthrpc.RegisterServer(srv, impl)
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
		name     string
		method   string
		identity string
		body     []byte
		wantBody string // exact external HTTP body (contract preservation)
	}{
		{
			name:     "register ({player_id, token} struct return)",
			method:   accountsauthrpc.MethodRegister,
			body:     []byte(`{"email":"a@x.io","password":"pw","displayName":"Ann"}`),
			wantBody: `{"player_id":"p-1","token":"tok-register"}`,
		},
		{
			name:     "loginEpic (public body key id_token remapped over the wire)",
			method:   accountsauthrpc.MethodLoginEpic,
			body:     []byte(`{"id_token":"JWT-123"}`),
			wantBody: `{"player_id":"p-epic","token":"tok:JWT-123"}`,
		},
		{
			name:     "me (flattened {player_id, display_name, identities})",
			method:   accountsauthrpc.MethodMe,
			identity: "p-42",
			wantBody: `{"player_id":"p-42","display_name":"Ann","identities":[{"provider":"dev","subject":"ann@example.com"}]}`,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			op := ops[tc.method]
			lBody, lStatus := driveLocalAuth(t, op, tc.identity, tc.body)
			rBody, rStatus := driveRemoteAuth(t, cl, op, tc.identity, tc.body)

			if lStatus != rStatus || lStatus != opsapi.StatusOK {
				t.Fatalf("status: local=%v remote=%v", lStatus, rStatus)
			}
			if !bytes.Equal(lBody, rBody) {
				t.Fatalf("body mismatch:\n local=%s\nremote=%s", lBody, rBody)
			}
			if string(lBody) != tc.wantBody {
				t.Fatalf("external body = %s, want %s (contract must be unchanged)", lBody, tc.wantBody)
			}
		})
	}
}
