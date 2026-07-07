// Package accountsapi is the accounts module's pure, transport-free capability
// contract: the canonical interfaces a peer verifies bearer tokens against and
// performs player auth through, over the edge. It imports only context — no edge,
// no transport — so it is the clean codegen INPUT for rpcgen, which reads it to
// generate the transport glue in the sibling accountsrpc / accountsauthrpc
// packages.
//
// Domain consumers (e.g. characters, inventory) do NOT import this package: they
// keep their own local structural interface (CLAUDE.md rule 4). It is reached
// only by the generated glue (accountsrpc/accountsauthrpc) + the remote stub — the
// provider-owned contract surface, same precedent as each module's <module>events
// package. The Player/Identity/Session value types live here (not in the impl
// package) because they are return types of the Auth capability, so the generated
// glue must be able to name them; the accounts module aliases Player/Identity as
// its own.
//
//go:generate go run gamebackend/tools/rpcgen -iface Sessions -prefix accounts -out ../accountsrpc/accountsrpc_gen.go
//go:generate go run gamebackend/tools/rpcgen -iface Auth -prefix accounts -out ../accountsauthrpc/accountsauthrpc_gen.go
package accountsapi

import "context"

// Player is the product-scoped identity (the EOS PUID analogue). Its JSON tags are
// the public HTTP response shape the gateway encodes.
type Player struct {
	ID          string `json:"player_id"`
	DisplayName string `json:"display_name"`
}

// Identity is one credential mapping (provider, subject) -> player.
type Identity struct {
	Provider string `json:"provider"`
	Subject  string `json:"subject"`
}

// Session is the result of a successful register/login: the caller's player_id
// plus the opaque bearer token minted for it. Its JSON tags are the public
// {player_id, token} response shape the gateway encodes.
type Session struct {
	PlayerID string `json:"player_id"`
	Token    string `json:"token"`
}

// Sessions resolves a bearer token to its player. VerifySession returns an error
// so a transport failure (the provider hosted in a peer process, reached over the
// QUIC edge) surfaces distinctly from a genuine invalid/expired session
// ("", false, nil). It matches the accounts service's VerifySession exactly.
type Sessions interface {
	VerifySession(ctx context.Context, token string) (playerID string, ok bool, err error)
}

// Auth is the accounts module's player-facing capability: the operations that
// establish or read a player identity. Register/Login/LoginEpic are AuthNone (they
// CREATE the session, so they take no caller identity); Me is AuthPlayer — it takes
// its caller identity from ctx (opsapi.PlayerID, injected by the gateway after
// bearer verification), NEVER as an argument. Register/Login/LoginEpic carry
// differentiated outcomes via opsapi.Status (bad credentials → 401, duplicate email
// → 409). The accounts service implements it exactly; the gateway/edge glue is
// generated from it.
type Auth interface {
	Register(ctx context.Context, email, password, displayName string) (session Session, err error)
	Login(ctx context.Context, email, password string) (session Session, err error)
	LoginEpic(ctx context.Context, idToken string) (session Session, err error)
	Me(ctx context.Context) (player Player, identities []Identity, err error)
}
