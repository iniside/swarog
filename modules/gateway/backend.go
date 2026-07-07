package gateway

import (
	"context"
	"encoding/json"
	"fmt"

	transport "gamebackend/gateway"
	"gamebackend/opsapi"
)

// OperationBackend is the topology-swappable invoker at the heart of the gateway:
// given an operation and an already-decoded typed request, it produces the typed
// response. It abstracts ONLY the invoke — the gateway decodes the HTTP body into
// req and encodes resp back to HTTP around this call, so the same decode/encode
// path drives both topologies and only the hop in the middle differs.
//
// Contract: resp MUST be a non-nil pointer to the operation's concrete response
// type; the backend fills it. The return is nil on success or an *opsapi.Error
// carrying the domain Status (which the gateway maps to an HTTP status). A plain
// (non-*Error) error means an unclassified failure (→ StatusInternal via
// opsapi.StatusOf).
//
// Two impls, selected per operation by whether the provider is in THIS process:
//
//   - LocalBackend (monolith / same-process) — looks up a typed in-process
//     invoker by method name and calls it with the decoded req; resp is filled by
//     a direct typed assignment. ZERO wire marshal: the HTTP body is decoded ONCE
//     at the gateway boundary and the struct is handed straight to the provider.
//   - RemoteBackend (split / cross-process) — marshals req to bytes, relays them
//     over the QUIC edge to the owning peer, and unmarshals the reply into resp.
//     One extra marshal + one extra unmarshal over the Local path (the honest
//     cost of the wire hop — NOT "zero marshal").
//
// Marshal-count honesty (decision D3 / review M4), per request, on TOP of the
// gateway's unavoidable 1 HTTP-decode + 1 HTTP-encode at the boundary:
//   - LocalBackend:  +0 marshals  (typed call, struct passed by reference)
//   - RemoteBackend: +1 marshal (req→bytes) +1 unmarshal (bytes→resp), the wire hop
type OperationBackend interface {
	Invoke(ctx context.Context, op opsapi.Operation, req, resp any) error
}

// LocalBackend dispatches an operation to its provider in-process. It holds the
// map of method-name → opsapi.LocalInvoker the gateway builds from
// ctx.Contributions(opsapi.LocalSlot) (Phase D populates it; empty until then).
// It performs NO serialization — the decoded req struct is handed to the invoker
// as-is and the invoker fills resp directly.
type LocalBackend struct {
	invokers map[string]opsapi.LocalInvoker
}

// NewLocalBackend returns a LocalBackend dispatching over invokers (method name →
// in-process invoker). The gateway builds the map from the LocalSlot; a test may
// pass one directly.
func NewLocalBackend(invokers map[string]opsapi.LocalInvoker) *LocalBackend {
	if invokers == nil {
		invokers = map[string]opsapi.LocalInvoker{}
	}
	return &LocalBackend{invokers: invokers}
}

var _ OperationBackend = (*LocalBackend)(nil)

// Invoke looks up the in-process invoker for op.Method and calls it with the
// decoded req and the caller's resp pointer — a direct typed call, no marshal. A
// missing invoker is a wiring bug (a route was bound with no provider registered
// in this process) surfaced as an error rather than a silent nil response.
func (b *LocalBackend) Invoke(ctx context.Context, op opsapi.Operation, req, resp any) error {
	inv, ok := b.invokers[op.Method]
	if !ok {
		return fmt.Errorf("gateway: no local invoker for operation %q", op.Method)
	}
	return inv(ctx, req, resp)
}

// RemoteBackend dispatches an operation to a provider hosted in a PEER process
// over the QUIC edge. It marshals req into the wire request payload, relays it by
// method name, and unmarshals the reply — following the rpcgen response-envelope
// convention (the payload carries a flat "status"/"err" pair alongside the domain
// fields, mirroring the generated <module>rpc client).
type RemoteBackend struct {
	// relay sends an already-encoded request payload for method to the owning peer
	// and returns the response payload bytes. Production wires
	// gateway.RoutedBackend.Forward (lazy dial + self-healing retry + 1s budget);
	// the fake-op test wires the same against a loopback edge server. The ctx is
	// NOT threaded into the QUIC call: RoutedBackend owns its own per-attempt
	// timeout, exactly as the existing gateway relay does — no false ctx
	// propagation is claimed.
	relay func(ctx context.Context, method string, payload []byte) ([]byte, error)
}

// NewRemoteBackend returns a RemoteBackend relaying to peerAddr over a
// self-healing RoutedBackend (reusing gateway/routed_backend.go's dial/retry).
func NewRemoteBackend(peerAddr string) *RemoteBackend {
	rb := transport.NewRoutedBackend(peerAddr)
	return &RemoteBackend{
		relay: func(_ context.Context, method string, payload []byte) ([]byte, error) {
			// RoutedBackend.Forward owns its own per-attempt timeout (forwardBudget),
			// exactly as the existing transport-gateway relay does, so the caller's
			// ctx is deliberately not threaded through — no false ctx propagation.
			return rb.Forward(method, payload) //nolint:contextcheck // RoutedBackend uses its own per-attempt budget (documented)
		},
	}
}

var _ OperationBackend = (*RemoteBackend)(nil)

// opStatusEnvelope probes the rpcgen response-envelope status/err pair without
// binding to any particular operation's domain fields. RemoteBackend reads it to
// decide success-vs-error generically, then unmarshals the SAME bytes into the
// caller's resp for the domain fields (ignoring status/err) — one relay, two
// cheap decodes of the reply, matching how the generated client splits outcome
// from payload.
type opStatusEnvelope struct {
	Status opsapi.Status `json:"status"`
	Err    string        `json:"err"`
}

// Invoke marshals req, relays it to the peer, and reconstitutes the outcome. A
// transport failure (peer down / unknown method) maps to StatusUnavailable so the
// gateway answers 503; a domain failure carried in the envelope maps to its
// Status; success unmarshals the domain fields into resp.
func (b *RemoteBackend) Invoke(ctx context.Context, op opsapi.Operation, req, resp any) error {
	reqBytes, err := json.Marshal(req) // +1 wire marshal (the split cost)
	if err != nil {
		return err
	}
	respBytes, err := b.relay(ctx, op.Method, reqBytes)
	if err != nil {
		// Transport-level failure: the peer is unreachable or rejected the call.
		return &opsapi.Error{Status: opsapi.StatusUnavailable, Msg: err.Error()}
	}
	var env opStatusEnvelope
	if err := json.Unmarshal(respBytes, &env); err != nil {
		return err
	}
	if env.Status != opsapi.StatusOK {
		return &opsapi.Error{Status: env.Status, Msg: env.Err}
	}
	if resp != nil {
		return json.Unmarshal(respBytes, resp) // +1 wire unmarshal (domain fields)
	}
	return nil
}
