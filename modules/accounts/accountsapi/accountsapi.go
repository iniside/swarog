// Package accountsapi is the accounts module's pure, transport-free capability
// contract: the canonical Sessions interface a peer verifies bearer tokens
// against over the edge. It imports only context — no edge, no transport — so it
// is the clean codegen INPUT for rpcgen, which reads it to generate the transport
// glue in the sibling accountsrpc package.
//
// Domain consumers (e.g. characters, inventory) do NOT import this package: they
// keep their own local structural interface (CLAUDE.md rule 4). It is reached
// only by the generated glue (accountsrpc) + the remote stub — the provider-owned
// contract surface, same precedent as each module's <module>events package.
//
//go:generate go run gamebackend/tools/rpcgen -iface Sessions -prefix accounts -out ../accountsrpc/accountsrpc_gen.go
package accountsapi

import "context"

// Sessions resolves a bearer token to its player. VerifySession returns an error
// so a transport failure (the provider hosted in a peer process, reached over the
// QUIC edge) surfaces distinctly from a genuine invalid/expired session
// ("", false, nil). It matches the accounts service's VerifySession exactly.
type Sessions interface {
	VerifySession(ctx context.Context, token string) (playerID string, ok bool, err error)
}
