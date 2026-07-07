package edge

import "encoding/json"

// request is the on-wire envelope for a single call. One stream carries exactly
// one request/response pair, so the stream itself is the correlation — there is
// no request id.
type request struct {
	Method string `json:"method"`
	// Identity is a reserved, additive field for the later auth phase (Phase D of
	// the unified-operation-transport plan): the gateway will inject the caller's
	// verified player_id here so backends read identity from the (trusted) envelope
	// instead of re-verifying a bearer per hop. It is UNUSED today — the client
	// leaves it empty and the server ignores it — and only wired now so the wire
	// shape is stable before auth logic lands. Do NOT read it for trust yet: the
	// edge hop is not mutually authenticated until Phase C.
	Identity string          `json:"identity,omitempty"`
	Payload  json.RawMessage `json:"payload"`
}

// response is the on-wire envelope for a single reply. OK distinguishes a
// successful Payload from a handler/dispatch Error.
type response struct {
	OK      bool            `json:"ok"`
	Payload json.RawMessage `json:"payload,omitempty"`
	Error   string          `json:"error,omitempty"`
}
