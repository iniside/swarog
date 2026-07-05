// Package remote provides a stand-in lifecycle.Module for a dependency that is
// hosted in ANOTHER process. When ROLES gates a module out of this process but a
// hosted module still Requires its service, main() registers a remote.Stub for
// that name so the service registry resolves — the call crosses the process
// boundary over the QUIC edge instead of running in-process.
//
// It imports only the core foundations + edge: the edge-backed clients satisfy the consumer-
// defined interfaces (characters.OwnerOf / accounts.VerifySession) by structural
// typing, so it never imports the real characters/accounts implementation
// packages (CLAUDE.md #2 — modules never import each other's impl). The only
// coupling to the provider is the method name + JSON DTO shape, mirrored below.
package remote

import (
	"context"
	"fmt"
	"sync"

	"gamebackend/edge"
	"gamebackend/lifecycle"
	"gamebackend/modules/admin/adminapi"
	"gamebackend/registry"
)

// Stub stands in for a module hosted in a peer process. Init Provides an edge-
// backed client under the module's Name, so a co-hosted dependent's Require
// resolves to a real QUIC caller. It never migrates a schema and mounts no
// routes; as a Stopper it closes the underlying edge connection on shutdown.
type Stub struct {
	name     string
	conn     *edgeConn
	client   any    // the typed client Provided under name; satisfies the consumer iface
	adminURL string // peer's .../admin-data/<name> URL; empty ⇒ no admin surface exposed
}

// NewStub builds a stub for the given dependency name, dialing peerAddr lazily.
// adminURL, when non-empty, is the peer's /admin-data/<name> URL — the stub then
// contributes a remote admin.Item so the co-hosted admin fetches this module's
// page over HTTP (keeping the sidebar sourced uniformly from Contributions, S2).
// Only "characters" and "accounts" are edge-exposed in this topology; any other
// name is a wiring bug and fails loudly rather than Providing a dead client.
func NewStub(name, peerAddr, adminURL string) *Stub {
	conn := &edgeConn{peerAddr: peerAddr}
	s := &Stub{name: name, conn: conn, adminURL: adminURL}
	switch name {
	case "characters":
		s.client = &charactersClient{conn: conn}
	case "accounts":
		s.client = &accountsClient{conn: conn}
	default:
		panic(fmt.Sprintf("remote: no edge client for module %q", name))
	}
	return s
}

func (s *Stub) Name() string       { return s.name }
func (s *Stub) Requires() []string { return nil } // a peer's foundations live in the peer

// Register offers the edge-backed client under the module's Name in Build's
// phase 1, so a co-hosted dependent's Require resolves to a real QUIC caller.
// The client was already built in NewStub; here it only enters the registry.
func (s *Stub) Register(ctx *lifecycle.Context) error {
	registry.Provide(ctx.Registry, s.name, s.client)
	ctx.Log.Info("remote stub registered — service resolves over the QUIC edge",
		"module", s.name, "peer", s.conn.peerAddr)
	return nil
}

// Init contributes a remote admin item (when an admin peer URL is configured) so
// this module still appears in the local /admin — its Section/Label/Content are
// fetched from adminURL, not carried here.
func (s *Stub) Init(ctx *lifecycle.Context) error {
	if s.adminURL != "" {
		ctx.Contribute(adminapi.Slot, adminapi.Item{ID: s.name, RemoteURL: s.adminURL})
		ctx.Log.Info("remote stub contributed admin item — page fetched over HTTP",
			"module", s.name, "adminURL", s.adminURL)
	}
	return nil
}

// Stop closes the persistent edge connection (if one was ever dialed).
func (s *Stub) Stop(_ context.Context) error { return s.conn.close() }

// edgeConn is a lazily-dialed, self-healing wrapper over an edge.Client. It
// dials on first use, holds the connection for reuse (persistent conn, stream-
// per-call), and on a failed call drops the connection and retries exactly once
// with a fresh dial. A dial failure — the peer is down — is returned to the
// caller, which maps it to 503.
type edgeConn struct {
	peerAddr string

	mu     sync.Mutex
	client *edge.Client
}

// get returns a live client, dialing if none is cached.
func (e *edgeConn) get(ctx context.Context) (*edge.Client, error) {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.client != nil {
		return e.client, nil
	}
	c, err := edge.Dial(ctx, e.peerAddr, edge.ClientTLS())
	if err != nil {
		return nil, err
	}
	e.client = c
	return c, nil
}

// reset drops the cached connection if it is the one that just failed, so the
// next get re-dials. Guarding on identity avoids closing a connection a
// concurrent caller already replaced.
func (e *edgeConn) reset(failed *edge.Client) {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.client == failed {
		_ = e.client.Close()
		e.client = nil
	}
}

func (e *edgeConn) close() error {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.client == nil {
		return nil
	}
	err := e.client.Close()
	e.client = nil
	return err
}

// call performs one RPC with a single reconnect-and-retry on failure. The first
// error may be a stale/dead connection (peer restarted); we drop it, re-dial,
// and retry once. If the re-dial fails or the retry also errors, the error
// propagates so the consumer answers 503.
func (e *edgeConn) call(ctx context.Context, method string, req, resp any) error {
	c, err := e.get(ctx)
	if err != nil {
		return err
	}
	if err = c.Call(ctx, method, req, resp); err == nil {
		return nil
	}
	// Possible transport failure — reconnect once and retry.
	e.reset(c)
	c2, err2 := e.get(ctx)
	if err2 != nil {
		return err2
	}
	return c2.Call(ctx, method, req, resp)
}

// --- characters.OwnerOf over the edge --------------------------------------

type ownerOfReq struct {
	ID string `json:"id"`
}

type ownerOfResp struct {
	PlayerID string `json:"player_id"`
	Ok       bool   `json:"ok"`
}

// charactersClient satisfies inventory's charactersSvc structurally.
type charactersClient struct{ conn *edgeConn }

func (c *charactersClient) OwnerOf(ctx context.Context, characterID string) (playerID string, ok bool, err error) {
	var out ownerOfResp
	if err := c.conn.call(ctx, "characters.ownerOf", ownerOfReq{ID: characterID}, &out); err != nil {
		return "", false, err
	}
	return out.PlayerID, out.Ok, nil
}

// --- accounts.VerifySession over the edge -----------------------------------

type verifySessionReq struct {
	Token string `json:"token"`
}

type verifySessionResp struct {
	PlayerID string `json:"player_id"`
	Ok       bool   `json:"ok"`
}

// accountsClient satisfies the accountsSvc consumer interfaces structurally.
type accountsClient struct{ conn *edgeConn }

func (c *accountsClient) VerifySession(ctx context.Context, token string) (playerID string, ok bool, err error) {
	var out verifySessionResp
	if err := c.conn.call(ctx, "accounts.verifySession", verifySessionReq{Token: token}, &out); err != nil {
		return "", false, err
	}
	return out.PlayerID, out.Ok, nil
}
