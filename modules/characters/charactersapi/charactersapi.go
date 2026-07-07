// Package charactersapi is the characters module's pure, transport-free
// capability contract: the canonical Ownership interface a peer resolves over
// the edge. It imports only context — no edge, no transport — so it is the clean
// codegen INPUT for rpcgen, which reads it to generate the transport glue in the
// sibling charactersrpc package.
//
// Domain consumers (e.g. inventory) do NOT import this package: they keep their
// own local structural interface (CLAUDE.md rule 4). It is reached only by the
// generated glue (charactersrpc) + the remote stub — the provider-owned contract
// surface, same precedent as each module's <module>events package.
//
//go:generate go run gamebackend/tools/rpcgen -iface Ownership -prefix characters -out ../charactersrpc/charactersrpc_gen.go
package charactersapi

import "context"

// Ownership resolves a character's owning player. OwnerOf returns an error so a
// transport failure (the provider hosted in a peer process, reached over the
// QUIC edge) surfaces distinctly from a genuine "no such character"
// ("", false, nil). It matches the characters service's OwnerOf exactly.
type Ownership interface {
	OwnerOf(ctx context.Context, characterID string) (playerID string, ok bool, err error)
}
