// Package gateway is the player-facing front door. It terminates the edge QUIC
// transport and HTTP without hosting any module: a QUIC prefix router relays
// framed calls to the backend that owns each method family, and an HTTP reverse
// proxy fans player-facing paths out to the backend that serves them. It imports
// only edge + stdlib — never a module implementation.
package gateway

import (
	"context"
	"sync"
	"time"

	"gamebackend/edge"
)

// forwardBudget bounds a single relay attempt (dial + one CallRaw). Each attempt
// gets a fresh timeout, so the retry path can take up to ~2×forwardBudget in the
// worst case (first attempt times out, reset, second attempt times out too).
const forwardBudget = 1 * time.Second

// RoutedBackend relays raw edge calls to one backend peer over a lazily-dialed,
// self-healing QUIC connection. It mirrors remote.edgeConn: a single cached
// client guarded by a mutex, dialed on first use, dropped and re-dialed on a
// failed call. One RoutedBackend is shared across every inbound gateway
// connection routed to its peer, so get/reset are mutex-disciplined and reset is
// identity-guarded — a slow caller whose client already failed must not wipe a
// healthy client a concurrent caller has since installed.
type RoutedBackend struct {
	peerAddr string

	mu     sync.Mutex
	client *edge.Client
}

// NewRoutedBackend returns a RoutedBackend that dials peerAddr on first Forward.
func NewRoutedBackend(peerAddr string) *RoutedBackend {
	return &RoutedBackend{peerAddr: peerAddr}
}

// get returns a live client, dialing if none is cached.
func (r *RoutedBackend) get(ctx context.Context) (*edge.Client, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.client != nil {
		return r.client, nil
	}
	// Mutual TLS: present this process's CA-signed client leaf and verify the
	// backend against the shared CA (no InsecureSkipVerify) — ClientMTLS resolves
	// the same process-shared anchor the backend's edge server requires.
	tlsConf, err := edge.ClientMTLS()
	if err != nil {
		return nil, err
	}
	c, err := edge.Dial(ctx, r.peerAddr, tlsConf)
	if err != nil {
		return nil, err
	}
	r.client = c
	return c, nil
}

// reset drops the cached client only if it is still the one that just failed, so
// the next get re-dials. Guarding on identity avoids closing a client a
// concurrent caller already replaced (thundering-herd + slow-caller safety).
func (r *RoutedBackend) reset(failed *edge.Client) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.client == failed {
		_ = r.client.Close()
		r.client = nil
	}
}

// Forward relays one raw edge call to the peer, retrying exactly once on any
// error with a fresh client and a fresh timeout. It implements
// edge.ForwardHandler: method + already-encoded payload in, response payload
// bytes out. A per-attempt context bounds each dial+call to forwardBudget; if the
// retry also errors that error propagates so the edge dispatch turns it into
// ok=false upstream (the player sees a failed call, not a hung one).
func (r *RoutedBackend) Forward(method string, payload []byte) ([]byte, error) {
	return r.ForwardID(method, "", payload)
}

// ForwardID is Forward carrying a caller identity: it stamps identity into each
// attempt's request envelope (via edge.Client.CallRawID) so the backend's
// generated server adapter can read the gateway-verified player_id from ctx. The
// retry/budget behaviour is identical to Forward. identity is empty for an
// unauthenticated relay (Forward).
func (r *RoutedBackend) ForwardID(method, identity string, payload []byte) ([]byte, error) {
	ctx, cancel := context.WithTimeout(context.Background(), forwardBudget)
	defer cancel()

	c, err := r.get(ctx)
	if err == nil {
		var out []byte
		if out, err = c.CallRawID(ctx, method, identity, payload); err == nil {
			return out, nil
		}
		// Possible stale/dead connection (peer restarted): drop it and retry.
		r.reset(c)
	}

	// Second and final attempt: fresh timeout, fresh dial.
	ctx2, cancel2 := context.WithTimeout(context.Background(), forwardBudget)
	defer cancel2()

	c2, err2 := r.get(ctx2)
	if err2 != nil {
		return nil, err2
	}
	out, err2 := c2.CallRawID(ctx2, method, identity, payload)
	if err2 != nil {
		r.reset(c2)
		return nil, err2
	}
	return out, nil
}

// Close best-effort closes the cached client (if one was ever dialed).
func (r *RoutedBackend) Close() error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.client == nil {
		return nil
	}
	err := r.client.Close()
	r.client = nil
	return err
}
