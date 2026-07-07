// Package leaderboardapi is the leaderboard module's pure, transport-free
// capability contract: the canonical interface the module exposes as a public
// (AuthNone) operation. It imports only context — no edge, no transport — so it
// is the clean codegen INPUT for rpcgen, which reads it to generate the
// transport glue in the sibling leaderboardrpc package.
//
// Domain consumers do NOT import this package: leaderboard has no domain
// consumers (nothing depends on it — Requires() is nil). It is reached only by
// the generated glue (leaderboardrpc), the same precedent as each module's
// <module>events package.
//
//go:generate go run gamebackend/tools/rpcgen -iface Leaderboard -prefix leaderboard -out ../leaderboardrpc/leaderboardrpc_gen.go
package leaderboardapi

import (
	"context"

	"gamebackend/opsapi"
)

// HTTPBindings declares the HTTP surface of the Leaderboard operation for rpcgen.
// Keyed by Go method name. TopScores is a public (AuthNone) read with no args.
var HTTPBindings = map[string]opsapi.HTTPBind{
	"TopScores": {Verb: "GET", Path: "/leaderboard", Auth: opsapi.AuthNone, Success: 200},
}

// Score is one player's standing. Its JSON tags are the public wire shape the
// pre-migration handleList wrote (player/wins), unchanged.
type Score struct {
	Player string `json:"player"`
	Wins   int64  `json:"wins"`
}

// Leaderboard is the leaderboard module's public capability: reading the top
// scores. It takes no caller identity — the operation is AuthNone, a public
// read, exactly as GET /leaderboard was before migration. The leaderboard
// service implements it exactly; the gateway/edge glue is generated from it.
type Leaderboard interface {
	// TopScores returns the top-ranked players (wins desc, player asc), capped at
	// 100 — the same shape and limit as the pre-migration handler.
	TopScores(ctx context.Context) (scores []Score, err error)
}
