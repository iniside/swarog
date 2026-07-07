package leaderboard

import (
	"context"
	"net/http"

	"gamebackend/lifecycle"
	"gamebackend/modules/leaderboard/leaderboardapi"
	"gamebackend/modules/leaderboard/leaderboardrpc"
	"gamebackend/opsapi"
)

// registerOps contributes the leaderboard's single public operation: an
// opsapi.Operation (the HTTP route + AuthNone + success code the gateway
// binds), an opsapi.OpBinding (no body/path to decode, and the typed response
// allocator), and an opsapi.LocalOp (the in-process invoker the gateway's
// LocalBackend dispatches). AuthNone — GET /leaderboard is a public read, no
// bearer, exactly as the pre-migration handleList. leaderboard is
// monolith-only (no split service hosts it, no edge), so there is no
// RegisterServer call here — only the LocalOp the monolith's LocalBackend
// dispatches.
func registerOps(ctx *lifecycle.Context, svc *Module) {
	// GET /leaderboard → leaderboard.topScores (200).
	ctx.Contribute(opsapi.Slot, opsapi.Operation{
		Method: leaderboardrpc.MethodTopScores, Verb: "GET", Path: "/leaderboard",
		Auth: opsapi.AuthNone, Success: http.StatusOK,
	})
	ctx.Contribute(opsapi.BindingSlot, opsapi.OpBinding{
		Method:  leaderboardrpc.MethodTopScores,
		Decode:  func([]byte, map[string]string) (any, error) { return &struct{}{}, nil },
		NewResp: func() any { return &[]leaderboardapi.Score{} },
	})
	ctx.Contribute(opsapi.LocalSlot, opsapi.LocalOp{
		Method: leaderboardrpc.MethodTopScores,
		Invoke: func(ctx context.Context, _, resp any) error {
			list, err := svc.TopScores(ctx)
			if err != nil {
				return err
			}
			*resp.(*[]leaderboardapi.Score) = list
			return nil
		},
	})
}
