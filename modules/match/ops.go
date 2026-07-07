package match

import (
	"context"
	"encoding/json"
	"net/http"

	"gamebackend/lifecycle"
	"gamebackend/modules/match/matchrpc"
	"gamebackend/opsapi"
)

// reportReq is the decoded body of POST /match/report. It decodes the
// pre-migration public body shape ({"Winner":..,"Loser":..} — Go's default
// json tags, unchanged) but is re-marshaled with its own field NAMES (not the
// generated reportRequest envelope's lowercase winner/loser) when dispatched
// over a RemoteBackend — matchrpc.Client is only reached from the generated
// glue, never directly, so this is fine: LocalBackend passes the exact pointer
// (no re-marshal), and match has no split service today (no RemoteBackend path
// exists for it in practice).
type reportReq struct {
	Winner string `json:"Winner"`
	Loser  string `json:"Loser"`
}

// registerOps contributes the match module's single public operation: an
// opsapi.Operation (the HTTP route + AuthNone + success code the gateway
// binds), an opsapi.OpBinding (HTTP body → typed request, no response body),
// and an opsapi.LocalOp (the in-process invoker the gateway's LocalBackend
// dispatches). AuthNone — POST /match/report has no bearer check today
// (match.go:27 pre-migration), so it stays authless. match is monolith-only
// (no split service hosts it, no edge), so there is no RegisterServer call
// here — only the LocalOp the monolith's LocalBackend dispatches.
func registerOps(ctx *lifecycle.Context, svc *Module) {
	// POST /match/report → match.report (202).
	ctx.Contribute(opsapi.Slot, opsapi.Operation{
		Method: matchrpc.MethodReport, Verb: "POST", Path: "/match/report",
		Auth: opsapi.AuthNone, Success: http.StatusAccepted,
	})
	ctx.Contribute(opsapi.BindingSlot, opsapi.OpBinding{
		Method: matchrpc.MethodReport,
		Decode: func(body []byte, _ map[string]string) (any, error) {
			var r reportReq
			if err := json.Unmarshal(body, &r); err != nil {
				return nil, &opsapi.Error{Status: opsapi.StatusInvalid, Msg: "invalid json"}
			}
			return &r, nil
		},
		NewResp: nil, // 202: no response body
	})
	ctx.Contribute(opsapi.LocalSlot, opsapi.LocalOp{
		Method: matchrpc.MethodReport,
		Invoke: func(ctx context.Context, req, _ any) error {
			r := req.(*reportReq)
			return svc.Report(ctx, r.Winner, r.Loser)
		},
	})
}
