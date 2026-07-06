package remote

import (
	"encoding/json"
	"testing"
)

// This file pins the ON-WIRE JSON shape of the edge RPCs this stub CALLS, from
// the CLIENT side. The DTOs here are deliberately MIRRORED (not shared) from the
// provider modules: ownerOf mirrors modules/characters, verifySession mirrors
// modules/accounts. Nothing but these tests stops the two hand-kept copies from
// drifting. The canonical bytes below MUST stay byte-identical with the
// provider-side pins:
//   - modules/characters/wire_contract_test.go  (characters.ownerOf)
//   - modules/accounts/wire_contract_test.go    (accounts.verifySession)
// If a json tag drifts on either side, that side's test fails against these bytes.
const (
	wireOwnerOfReq        = `{"id":"char-1"}`
	wireOwnerOfResp       = `{"player_id":"player-1","ok":true}`
	wireVerifySessionReq  = `{"token":"good-token"}`
	wireVerifySessionResp = `{"player_id":"player-9","ok":true}`
)

func TestOwnerOfWireContract(t *testing.T) {
	got, _ := json.Marshal(ownerOfReq{ID: "char-1"})
	if string(got) != wireOwnerOfReq {
		t.Fatalf("ownerOfReq wire = %s, want %s (tag drift vs modules/characters)", got, wireOwnerOfReq)
	}
	var resp ownerOfResp
	if err := json.Unmarshal([]byte(wireOwnerOfResp), &resp); err != nil {
		t.Fatalf("unmarshal ownerOfResp: %v", err)
	}
	if resp.PlayerID != "player-1" || !resp.Ok {
		t.Fatalf("ownerOfResp = %+v, want {player-1 true} (tag drift?)", resp)
	}
	if back, _ := json.Marshal(resp); string(back) != wireOwnerOfResp {
		t.Fatalf("ownerOfResp re-marshal = %s, want %s", back, wireOwnerOfResp)
	}
}

func TestVerifySessionWireContract(t *testing.T) {
	got, _ := json.Marshal(verifySessionReq{Token: "good-token"})
	if string(got) != wireVerifySessionReq {
		t.Fatalf("verifySessionReq wire = %s, want %s (tag drift vs modules/accounts)", got, wireVerifySessionReq)
	}
	var resp verifySessionResp
	if err := json.Unmarshal([]byte(wireVerifySessionResp), &resp); err != nil {
		t.Fatalf("unmarshal verifySessionResp: %v", err)
	}
	if resp.PlayerID != "player-9" || !resp.Ok {
		t.Fatalf("verifySessionResp = %+v, want {player-9 true} (tag drift?)", resp)
	}
	if back, _ := json.Marshal(resp); string(back) != wireVerifySessionResp {
		t.Fatalf("verifySessionResp re-marshal = %s, want %s", back, wireVerifySessionResp)
	}
}
