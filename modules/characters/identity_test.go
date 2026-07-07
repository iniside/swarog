package characters

import (
	"context"
	"encoding/json"
	"testing"
	"time"

	"gamebackend/edge"
	"gamebackend/modules/characters/charactersapi"
	"gamebackend/modules/characters/charactersplayerrpc"
	"gamebackend/opsapi"
)

// fakePlayer echoes the caller player_id it reads from ctx as the created
// character's PlayerID, so the test can assert identity crossed the wire without
// touching a database.
type fakePlayer struct{}

func (fakePlayer) Create(ctx context.Context, name, class string) (charactersapi.Character, error) {
	pid, _ := opsapi.PlayerID(ctx)
	return charactersapi.Character{ID: "c1", PlayerID: pid, Name: name, Class: class}, nil
}
func (fakePlayer) List(context.Context) ([]charactersapi.Character, error) { return nil, nil }
func (fakePlayer) Delete(context.Context, string) error                    { return nil }

// TestPlayerIdentityOverEdge proves the cross-process half of the trust boundary:
// an identity stamped into the edge request envelope (exactly as the gateway's
// RemoteBackend does via CallRawID) reaches the operation impl as
// opsapi.PlayerID(ctx) — through the generated server adapter's WithPlayerID. This
// is "auth once at the gateway, identity in ctx" over the wire.
func TestPlayerIdentityOverEdge(t *testing.T) {
	srv := edge.NewServer()
	charactersplayerrpc.RegisterServer(srv, fakePlayer{})

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
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	cl, err := edge.Dial(ctx, srv.Addr().String(), ctls)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer func() { _ = cl.Close() }()

	body, _ := json.Marshal(map[string]string{"name": "Aria", "class": "mage"})
	respBytes, err := cl.CallRawID(ctx, charactersplayerrpc.MethodCreate, "player-42", body)
	if err != nil {
		t.Fatalf("CallRawID: %v", err)
	}

	var resp struct {
		Status    opsapi.Status           `json:"status"`
		Character charactersapi.Character `json:"character"`
	}
	if err := json.Unmarshal(respBytes, &resp); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if resp.Status != opsapi.StatusOK {
		t.Fatalf("status = %v, want OK", resp.Status)
	}
	if resp.Character.PlayerID != "player-42" {
		t.Fatalf("identity did not plumb through the edge envelope: character player_id = %q, want %q",
			resp.Character.PlayerID, "player-42")
	}
}
