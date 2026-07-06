package edge

import (
	"context"
	"crypto/tls"
	"fmt"
	"net"
	"sync"
	"time"

	"strings"

	quic "github.com/quic-go/quic-go"
)

// Handler is a transport-agnostic RPC handler: raw request payload bytes in,
// raw response payload bytes out. Encoding of the payload is the caller's
// concern (a typed helper can wrap this); the server only frames and dispatches.
type Handler func(reqPayload []byte) (respPayload []byte, err error)

// ForwardHandler is like Handler but also receives the method name, so a single
// prefix registration can serve a whole family of methods under their original
// names — the natural shape for a gateway that relays to a backend.
type ForwardHandler func(method string, payload []byte) (respPayload []byte, err error)

// prefixEntry pairs a method-name prefix with the ForwardHandler that serves any
// method matching it.
type prefixEntry struct {
	prefix string
	fwd    ForwardHandler
}

// Server is a QUIC RPC server. It accepts connections, then streams, and
// dispatches one framed request per stream to a Handler registered by method
// name. It knows nothing about the application domain — it is pure transport.
type Server struct {
	codec    Codec
	handlers map[string]Handler
	prefixes []prefixEntry

	mu    sync.Mutex
	ln    *quic.Listener
	conns map[*quic.Conn]struct{}
	wg    sync.WaitGroup
	ctx   context.Context
	stop  context.CancelFunc
}

// NewServer returns a Server using the default (JSON) codec.
func NewServer() *Server {
	ctx, cancel := context.WithCancel(context.Background())
	return &Server{
		codec:    defaultCodec,
		handlers: make(map[string]Handler),
		conns:    make(map[*quic.Conn]struct{}),
		ctx:      ctx,
		stop:     cancel,
	}
}

// Handle registers a Handler under a method name. Not safe to call concurrently
// with Serve; register all handlers before listening.
func (s *Server) Handle(method string, h Handler) {
	s.handlers[method] = h
}

// HandlePrefix registers a ForwardHandler for every method whose name starts
// with prefix. An exact Handle registration always wins over any prefix; among
// competing prefixes the longest match wins. Not safe to call concurrently with
// Serve; register all handlers before listening.
func (s *Server) HandlePrefix(prefix string, fwd ForwardHandler) {
	s.prefixes = append(s.prefixes, prefixEntry{prefix: prefix, fwd: fwd})
}

// defaultConfig is the shared QUIC config: a keep-alive holds the connection
// open between calls so the client's persistent conn survives idle gaps.
func defaultConfig() *quic.Config {
	return &quic.Config{
		MaxIdleTimeout:  30 * time.Second,
		KeepAlivePeriod: 15 * time.Second,
	}
}

// ListenAddr binds a QUIC listener on addr (e.g. "127.0.0.1:0" for an ephemeral
// port) and starts the accept loop in the background, returning once the socket
// is bound. It does not block, so a Starter can call it directly; Addr is valid
// as soon as it returns. Use Close to stop.
func (s *Server) ListenAddr(addr string, tlsConf *tls.Config) error {
	ln, err := quic.ListenAddr(addr, tlsConf, defaultConfig())
	if err != nil {
		return err
	}
	s.mu.Lock()
	s.ln = ln
	s.mu.Unlock()

	s.wg.Go(func() { s.acceptLoop(ln) })
	return nil
}

// Serve runs the accept loop on an existing listener and blocks until the
// listener is closed (via Close). Use this when you own the listener lifecycle;
// prefer ListenAddr for the common case.
func (s *Server) Serve(ln *quic.Listener) error {
	s.mu.Lock()
	s.ln = ln
	s.mu.Unlock()
	s.acceptLoop(ln)
	return nil
}

// Addr returns the listener's bound network address, or nil before ListenAddr
// has taken the listener.
func (s *Server) Addr() net.Addr {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.ln == nil {
		return nil
	}
	return s.ln.Addr()
}

// acceptLoop accepts connections until the listener is closed or the server's
// context is cancelled.
func (s *Server) acceptLoop(ln *quic.Listener) {
	for {
		conn, err := ln.Accept(s.ctx)
		if err != nil {
			// Listener closed / server shutting down.
			return
		}
		s.trackConn(conn)
		s.wg.Go(func() { s.serveConn(conn) })
	}
}

func (s *Server) trackConn(conn *quic.Conn) {
	s.mu.Lock()
	s.conns[conn] = struct{}{}
	s.mu.Unlock()
}

func (s *Server) untrackConn(conn *quic.Conn) {
	s.mu.Lock()
	delete(s.conns, conn)
	s.mu.Unlock()
}

// serveConn accepts streams on a single connection, one goroutine per stream.
func (s *Server) serveConn(conn *quic.Conn) {
	defer s.untrackConn(conn)

	for {
		stream, err := conn.AcceptStream(s.ctx)
		if err != nil {
			// Peer closed the conn, idle timeout, or server shutting down.
			return
		}
		s.wg.Go(func() { s.serveStream(stream) })
	}
}

// serveStream reads one framed request, dispatches it, and writes one framed
// response. Handler panics are recovered into an error response.
func (s *Server) serveStream(stream *quic.Stream) {
	defer func() { _ = stream.Close() }()

	reqBytes, err := readFrame(stream)
	if err != nil {
		// Malformed / truncated request: nothing to reply to reliably.
		stream.CancelRead(0)
		return
	}

	resp := s.dispatch(reqBytes)

	respBytes, err := s.codec.Encode(resp)
	if err != nil {
		// Fall back to a hand-rolled minimal error if the response won't encode.
		respBytes = []byte(`{"ok":false,"error":"edge: response encode failed"}`)
	}
	_ = writeFrame(stream, respBytes)
	// stream.Close() (deferred) closes the write side, signalling EOF to client.
}

// dispatch decodes the request envelope, invokes the handler, and builds the
// response envelope. Unknown methods and handler errors/panics become OK:false.
func (s *Server) dispatch(reqBytes []byte) (resp response) {
	// Recover a panicking handler — or a panicking codec Decode (a future custom
	// Codec could panic on adversarial bytes) — into an error response so one bad
	// call cannot take down the stream goroutine silently. Installed before Decode
	// so it covers the decode too; also covers the forward path.
	defer func() {
		if r := recover(); r != nil {
			resp = response{OK: false, Error: fmt.Sprintf("edge: handler panic: %v", r)}
		}
	}()

	var req request
	if err := s.codec.Decode(reqBytes, &req); err != nil {
		return response{OK: false, Error: "edge: malformed request envelope"}
	}

	// Exact registration always wins.
	if h, ok := s.handlers[req.Method]; ok {
		respPayload, err := h(req.Payload)
		if err != nil {
			return response{OK: false, Error: err.Error()}
		}
		return response{OK: true, Payload: respPayload}
	}

	// Otherwise, forward via the longest matching prefix registration.
	if fwd, ok := s.longestPrefix(req.Method); ok {
		respPayload, err := fwd(req.Method, req.Payload)
		if err != nil {
			return response{OK: false, Error: err.Error()}
		}
		return response{OK: true, Payload: respPayload}
	}

	return response{OK: false, Error: fmt.Sprintf("edge: unknown method %q", req.Method)}
}

// longestPrefix returns the ForwardHandler whose prefix is the longest one that
// method starts with, or ok=false if none match.
func (s *Server) longestPrefix(method string) (ForwardHandler, bool) {
	var (
		best    ForwardHandler
		bestLen = -1
	)
	for _, e := range s.prefixes {
		if strings.HasPrefix(method, e.prefix) && len(e.prefix) > bestLen {
			best = e.fwd
			bestLen = len(e.prefix)
		}
	}
	return best, bestLen >= 0
}

// Close stops the accept loop, closes the listener and all live connections,
// and waits for in-flight goroutines to drain.
func (s *Server) Close() error {
	s.stop()

	s.mu.Lock()
	ln := s.ln
	conns := make([]*quic.Conn, 0, len(s.conns))
	for c := range s.conns {
		conns = append(conns, c)
	}
	s.mu.Unlock()

	var err error
	if ln != nil {
		err = ln.Close()
	}
	for _, c := range conns {
		_ = c.CloseWithError(0, "server shutting down")
	}
	s.wg.Wait()
	return err
}
