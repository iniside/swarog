package accounts

import (
	"encoding/json"
	"testing"
)

// Pins the ON-WIRE JSON shape of the "accounts.verifySession" edge RPC from the
// PROVIDER side. The DTOs are deliberately MIRRORED (not shared) in
// modules/remote/remote.go; these canonical bytes MUST stay byte-identical with
// modules/remote/wire_contract_test.go. A json tag drift here fails this test.
//
// CANONICAL WIRE CONTRACT — keep byte-identical with modules/remote:
//   accounts.verifySession  req:  {"token":"<opaque>"}
//                          resp: {"player_id":"<playerID>","ok":<bool>}
const (
	wireVerifySessionReq  = `{"token":"good-token"}`
	wireVerifySessionResp = `{"player_id":"player-9","ok":true}`
)

func TestVerifySessionWireContract(t *testing.T) {
	var req verifySessionReq
	if err := json.Unmarshal([]byte(wireVerifySessionReq), &req); err != nil {
		t.Fatalf("unmarshal verifySessionReq: %v", err)
	}
	if req.Token != "good-token" {
		t.Fatalf("verifySessionReq.Token = %q, want good-token (tag drift?)", req.Token)
	}
	if got, _ := json.Marshal(req); string(got) != wireVerifySessionReq {
		t.Fatalf("verifySessionReq wire = %s, want %s (tag drift vs modules/remote)", got, wireVerifySessionReq)
	}

	got, _ := json.Marshal(verifySessionResp{PlayerID: "player-9", Ok: true})
	if string(got) != wireVerifySessionResp {
		t.Fatalf("verifySessionResp wire = %s, want %s (field/tag drift vs modules/remote)", got, wireVerifySessionResp)
	}
}
