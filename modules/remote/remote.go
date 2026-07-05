// Package remote provides a stand-in core.Module for a dependency that is hosted
// in ANOTHER process. When ROLES gates a module out of this process but a hosted
// module still Requires its service, main() registers a remote.Stub for that
// name so the service registry resolves — the call will cross the process
// boundary (QUIC edge) instead of running in-process.
//
// It imports only core: the placeholder client satisfies the consumer-defined
// interfaces (OwnerOf / VerifySession / ListByPlayer) by structural typing, so
// it never needs to import the real characters/accounts implementation packages
// (CLAUDE.md #2 — modules never import each other's impl).
package remote

import (
	"context"

	"gamebackend/core"
)

// Stub stands in for a module hosted in a peer process. In Krok 1 it is a pure
// skeleton: Init Provides a placeholder client under the module's Name, so a
// co-hosted dependent's Require resolves. It never migrates a schema and mounts
// no routes.
type Stub struct {
	name     string
	peerAddr string
	// client is what other modules get from Require(name). In Krok 1 it is a
	// placeholder that returns "not wired yet"; Krok 3 replaces it with the edge
	// client that actually calls the peer over QUIC.
	client any
}

// NewStub builds a stub for the given dependency name, pointing at peerAddr.
func NewStub(name, peerAddr string) *Stub {
	return &Stub{
		name:     name,
		peerAddr: peerAddr,
		// TODO(Krok 3): replace placeholder with edge client dialing peerAddr.
		client: placeholder{},
	}
}

func (s *Stub) Name() string        { return s.name }
func (s *Stub) DependsOn() []string { return nil } // a peer's foundations live in the peer

// Init registers the placeholder client so a co-hosted dependent can Require it.
func (s *Stub) Init(ctx *core.Context) error {
	ctx.Provide(s.name, s.client)
	ctx.Log.Warn("remote stub registered — service resolves to a not-yet-wired placeholder",
		"module", s.name, "peer", s.peerAddr)
	return nil
}

// Stop is a no-op for now; Krok 3's edge client closes its connection here.
func (s *Stub) Stop(_ context.Context) error { return nil }

// placeholder satisfies every consumer-defined interface the stubbed modules
// expose (accounts.VerifySession, characters.OwnerOf, characters.ListByPlayer)
// by structural typing. Krok 1's verification only exercises pure-local role
// subsets, so these methods are never actually invoked; they exist so the stub
// Provides SOMETHING assertable and the binary compiles.
//
// Signatures intentionally match the CURRENT (no-error) consumer interfaces;
// Krok 3 widens OwnerOf / VerifySession with an error return and swaps this
// placeholder for the real edge client.
type placeholder struct{}

func (placeholder) VerifySession(_ context.Context, _ string) (playerID string, ok bool) {
	return "", false
}

func (placeholder) OwnerOf(_ context.Context, _ string) (playerID string, ok bool) {
	return "", false
}

func (placeholder) ListByPlayer(_ context.Context, _ string) (any, error) {
	return nil, nil
}
