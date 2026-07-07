// Package remote provides a stand-in lifecycle.Module for a dependency that is
// hosted in ANOTHER process. When ROLES gates a module out of this process but a
// hosted module still Requires its service, main() registers a remote.Stub for
// that name so the service registry resolves — the call crosses the process
// boundary over the QUIC edge instead of running in-process.
//
// It imports only the core foundations + edge + the generated <module>rpc glue:
// each rpc package's Client implements the provider's capability interface over an
// opsapi.Caller (satisfied by edgeConn below), so remote never imports the real
// characters/accounts implementation packages (CLAUDE.md #2 — modules never import
// each other's impl). The wire shape + method name are OWNED by the generated
// glue, not hand-mirrored here — so wire drift between the two sides is impossible.
package remote

import (
	"context"
	"fmt"
	"sync"

	"gamebackend/edge"
	"gamebackend/lifecycle"
	"gamebackend/modules/accounts/accountsrpc"
	"gamebackend/modules/admin/adminapi"
	"gamebackend/modules/characters/charactersrpc"
	"gamebackend/opsapi"
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
	// The generated clients dial through an opsapi.Caller; hand them conn AS that
	// interface so the glue depends on the transport seam, never remote's concrete
	// edgeConn type (mirrors how app receives modules as lifecycle.Module).
	var caller opsapi.Caller = conn
	switch name {
	case "characters":
		// The generated Client implements charactersapi.Ownership over the Caller;
		// it structurally satisfies inventory's charactersSvc.
		s.client = charactersrpc.NewClient(caller)
	case "accounts":
		// The generated Client implements accountsapi.Sessions over the Caller;
		// it structurally satisfies the consumers' accountsSvc.
		s.client = accountsrpc.NewClient(caller)
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
	// Mutual TLS: present this process's CA-signed client leaf and verify the peer
	// against the shared CA (no InsecureSkipVerify). ClientMTLS resolves the same
	// process-shared anchor the peer's edge server trusts.
	tlsConf, err := edge.ClientMTLS()
	if err != nil {
		return nil, err
	}
	c, err := edge.Dial(ctx, e.peerAddr, tlsConf)
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

// Call performs one RPC with a single reconnect-and-retry on failure. The first
// error may be a stale/dead connection (peer restarted); we drop it, re-dial,
// and retry once. If the re-dial fails or the retry also errors, the error
// propagates so the consumer answers 503. Its signature matches opsapi.Caller,
// so *edgeConn is the transport a generated <module>rpc.Client dials through.
func (e *edgeConn) Call(ctx context.Context, method string, req, resp any) error {
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
