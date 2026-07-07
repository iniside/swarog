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
	"strings"
	"sync"

	"gamebackend/edge"
	"gamebackend/lifecycle"
	"gamebackend/modules/accounts/accountsadminrpc"
	"gamebackend/modules/accounts/accountsrpc"
	"gamebackend/modules/admin/adminapi"
	"gamebackend/modules/characters/charactersadminrpc"
	"gamebackend/modules/characters/charactersrpc"
	"gamebackend/opsapi"
	"gamebackend/registry"
)

// adminFetcher is the generated adminData client shape (charactersadminrpc.Client /
// accountsadminrpc.Client both implement it): a single AdminData op returning the
// module's admin page as adminapi.ItemData over the edge.
type adminFetcher interface {
	AdminData(ctx context.Context) (adminapi.ItemData, error)
}

// Stub stands in for a module hosted in a peer process. Init Provides an edge-
// backed client under the module's Name, so a co-hosted dependent's Require
// resolves to a real QUIC caller. It never migrates a schema and mounts no
// routes; as a Stopper it closes the underlying edge connection on shutdown.
type Stub struct {
	name       string
	conn       *edgeConn
	client     any          // the typed client Provided under name; satisfies the consumer iface
	adminFetch adminFetcher // the generated adminData client; fans the peer's admin page over the edge
}

// NewStub builds a stub for the given dependency name, dialing peerAddr lazily.
// Every stub also gets an adminData edge client, so the co-hosted admin fans this
// module's page out over the SAME mTLS QUIC edge as ownerOf/verifySession (no
// bespoke HTTP endpoint) — the sidebar stays sourced uniformly from Contributions.
// Only "characters" and "accounts" are edge-exposed in this topology; any other
// name is a wiring bug and fails loudly rather than Providing a dead client.
func NewStub(name, peerAddr string) *Stub {
	conn := &edgeConn{peerAddr: peerAddr}
	s := &Stub{name: name, conn: conn}
	// The generated clients dial through an opsapi.Caller; hand them conn AS that
	// interface so the glue depends on the transport seam, never remote's concrete
	// edgeConn type (mirrors how app receives modules as lifecycle.Module).
	var caller opsapi.Caller = conn
	switch name {
	case "characters":
		// The generated Client implements charactersapi.Ownership over the Caller;
		// it structurally satisfies inventory's charactersSvc.
		s.client = charactersrpc.NewClient(caller)
		s.adminFetch = charactersadminrpc.NewClient(caller)
	case "accounts":
		// The generated Client implements accountsapi.Sessions over the Caller;
		// it structurally satisfies the consumers' accountsSvc.
		s.client = accountsrpc.NewClient(caller)
		s.adminFetch = accountsadminrpc.NewClient(caller)
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

// Init contributes a remote admin item so this module still appears in the local
// /admin. Its Section/Label/Content are fetched lazily via the adminData edge
// operation (RemoteFetch), riding the same QUIC edge as the module's other ops —
// no separate HTTP endpoint. A peer that has not registered adminData surfaces the
// edge "unknown method" error, which fetchAdmin maps to adminapi.ErrItemAbsent so
// the admin drops the item silently.
func (s *Stub) Init(ctx *lifecycle.Context) error {
	ctx.Contribute(adminapi.Slot, adminapi.Item{ID: s.name, RemoteFetch: s.fetchAdmin})
	ctx.Log.Info("remote stub contributed admin item — page fetched over the QUIC edge",
		"module", s.name, "peer", s.conn.peerAddr)
	return nil
}

// fetchAdmin calls the peer's adminData operation over the edge and returns its
// ItemData. A peer with no admin surface answers the edge "unknown method" error;
// that single case maps to adminapi.ErrItemAbsent (skip), every other error (a
// down peer, a transport failure) propagates so the admin shows an error card.
func (s *Stub) fetchAdmin(ctx context.Context) (adminapi.ItemData, error) {
	data, err := s.adminFetch.AdminData(ctx)
	if err != nil && strings.Contains(err.Error(), "unknown method") {
		return adminapi.ItemData{}, adminapi.ErrItemAbsent
	}
	return data, err
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
