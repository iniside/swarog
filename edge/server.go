package edge

import (
	"context"
	"crypto/tls"
	"fmt"
	"net"
	"sync"
	"time"

	quic "github.com/quic-go/quic-go"
)

// Handler is a transport-agnostic RPC handler: raw request payload bytes in,
// raw response payload bytes out. Encoding of the payload is the caller's
// concern (a typed helper can wrap this); the server only frames and dispatches.
type Handler func(reqPayload []byte) (respPayload []byte, err error)

// Server is a QUIC RPC server. It accepts connections, then streams, and
// dispatches one framed request per stream to a Handler registered by method
// name. It knows nothing about the application domain — it is pure transport.
type Server struct {
	codec    Codec
	handlers map[string]Handler

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
	var req request
	if err := s.codec.Decode(reqBytes, &req); err != nil {
		return response{OK: false, Error: "edge: malformed request envelope"}
	}

	h, ok := s.handlers[req.Method]
	if !ok {
		return response{OK: false, Error: fmt.Sprintf("edge: unknown method %q", req.Method)}
	}

	// Recover a panicking handler into an error response so one bad call cannot
	// take down the stream goroutine silently.
	defer func() {
		if r := recover(); r != nil {
			resp = response{OK: false, Error: fmt.Sprintf("edge: handler panic: %v", r)}
		}
	}()

	respPayload, err := h(req.Payload)
	if err != nil {
		return response{OK: false, Error: err.Error()}
	}
	return response{OK: true, Payload: respPayload}
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
