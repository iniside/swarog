package edge

import "encoding/json"

// request is the on-wire envelope for a single call. One stream carries exactly
// one request/response pair, so the stream itself is the correlation — there is
// no request id.
type request struct {
	Method  string          `json:"method"`
	Payload json.RawMessage `json:"payload"`
}

// response is the on-wire envelope for a single reply. OK distinguishes a
// successful Payload from a handler/dispatch Error.
type response struct {
	OK      bool            `json:"ok"`
	Payload json.RawMessage `json:"payload,omitempty"`
	Error   string          `json:"error,omitempty"`
}
