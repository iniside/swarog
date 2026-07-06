package inventory

import (
	"encoding/json"
	"testing"
)

// Pins the player-facing "inventory.list" wire shape. Unlike ownerOf/verifySession
// there is no second typed copy to mirror (the gateway relays raw bytes, and the
// only decoder is an out-of-repo player client), so this pins the shape a player
// client depends on: a request carries player_id, a response wraps holdings under
// "items". A key rename here silently breaks that client — this test makes it loud.
func TestListWireContract(t *testing.T) {
	var req listReq
	if err := json.Unmarshal([]byte(`{"player_id":"p1"}`), &req); err != nil {
		t.Fatalf("unmarshal listReq: %v", err)
	}
	if req.PlayerID != "p1" {
		t.Fatalf("listReq.PlayerID = %q, want p1 (tag drift?)", req.PlayerID)
	}
	if got, _ := json.Marshal(listResp{}); string(got) != `{"items":null}` {
		t.Fatalf("listResp wire = %s, want {\"items\":null} (key drift?)", got)
	}
}
