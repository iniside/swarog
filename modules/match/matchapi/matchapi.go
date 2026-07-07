// Package matchapi is the match module's pure, transport-free capability
// contract: the canonical interface the module exposes as a public (AuthNone)
// operation. It imports only context — no edge, no transport — so it is the
// clean codegen INPUT for rpcgen, which reads it to generate the transport glue
// in the sibling matchrpc package.
//
// Domain consumers do NOT import this package: match has no domain consumers.
// It is reached only by the generated glue (matchrpc), the same precedent as
// each module's <module>events package.
//
//go:generate go run gamebackend/tools/rpcgen -iface Match -prefix match -out ../matchrpc/matchrpc_gen.go
package matchapi

import "context"

// Match is the match module's public capability: reporting a match result. It
// takes no caller identity — the operation is AuthNone, exactly as
// POST /match/report was before migration (no bearer check today). The match
// service implements it exactly, doing the synchronous MMR lookup and emitting
// matchevents.Finished; the gateway/edge glue is generated from it.
type Match interface {
	// Report records that winner beat loser: it synchronously reads the winner's
	// current MMR (for the log line) and fire-and-forgets a matchevents.Finished.
	Report(ctx context.Context, winner, loser string) (err error)
}
