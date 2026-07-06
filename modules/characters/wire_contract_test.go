package characters

import (
	"encoding/json"
	"testing"
)

// This file pins the ON-WIRE JSON shape of the "characters.ownerOf" and
// "characters.list" edge RPCs. The ownerOf DTOs are deliberately MIRRORED (not
// shared) in modules/remote/remote.go per the consumer-defined-interface idiom
// (CLAUDE.md): there is no shared type, so nothing but a test stops the two
// copies from drifting. remote's wire_contract_test.go pins the SAME canonical
// bytes below; if either side renames a json tag, that side's test fails here.
//
// CANONICAL WIRE CONTRACT — keep byte-identical with modules/remote:
//   characters.ownerOf  req:  {"id":"<characterID>"}
//                       resp: {"player_id":"<playerID>","ok":<bool>}
const (
	wireOwnerOfReq  = `{"id":"char-1"}`
	wireOwnerOfResp = `{"player_id":"player-1","ok":true}`
)

func TestOwnerOfWireContract(t *testing.T) {
	// Request: canonical bytes -> struct -> canonical bytes (round-trip stable).
	var req ownerOfReq
	if err := json.Unmarshal([]byte(wireOwnerOfReq), &req); err != nil {
		t.Fatalf("unmarshal req: %v", err)
	}
	if req.ID != "char-1" {
		t.Fatalf("req.ID = %q, want char-1 (json tag drift on ownerOfReq.ID?)", req.ID)
	}
	if got, _ := json.Marshal(req); string(got) != wireOwnerOfReq {
		t.Fatalf("ownerOfReq wire = %s, want %s (tag drift vs modules/remote)", got, wireOwnerOfReq)
	}

	// Response: struct -> canonical bytes and back.
	got, _ := json.Marshal(ownerOfResp{PlayerID: "player-1", Ok: true})
	if string(got) != wireOwnerOfResp {
		t.Fatalf("ownerOfResp wire = %s, want %s (tag/field drift vs modules/remote)", got, wireOwnerOfResp)
	}
	var resp ownerOfResp
	if err := json.Unmarshal([]byte(wireOwnerOfResp), &resp); err != nil {
		t.Fatalf("unmarshal resp: %v", err)
	}
	if resp.PlayerID != "player-1" || !resp.Ok {
		t.Fatalf("resp = %+v, want {player-1 true}", resp)
	}
}

// TestListWireContract pins the player-facing "characters.list" shape: a request
// carries player_id, a response wraps the characters under "characters". A player
// client (out of this repo) decodes these, so the key names are a contract.
func TestListWireContract(t *testing.T) {
	var req listReq
	if err := json.Unmarshal([]byte(`{"player_id":"p1"}`), &req); err != nil {
		t.Fatalf("unmarshal listReq: %v", err)
	}
	if req.PlayerID != "p1" {
		t.Fatalf("listReq.PlayerID = %q, want p1 (tag drift?)", req.PlayerID)
	}
	if got, _ := json.Marshal(listResp{}); string(got) != `{"characters":null}` {
		t.Fatalf("listResp wire = %s, want {\"characters\":null} (key drift?)", got)
	}
}
