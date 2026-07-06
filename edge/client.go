package edge

import (
	"context"
	"crypto/tls"
	"encoding/json"
	"errors"

	quic "github.com/quic-go/quic-go"
)

// Client is a QUIC RPC client holding a single persistent connection. Each Call
// opens a fresh, cheap stream over that reused connection (the plan's S5:
// persistent conn, stream-per-call — never a re-dial per call).
type Client struct {
	conn  *quic.Conn
	codec Codec
}

// Dial establishes the persistent QUIC connection to addr. The connection is
// held for the Client's lifetime; individual calls open streams over it.
func Dial(ctx context.Context, addr string, tlsConf *tls.Config) (*Client, error) {
	conn, err := quic.DialAddr(ctx, addr, tlsConf, defaultConfig())
	if err != nil {
		return nil, err
	}
	return &Client{conn: conn, codec: defaultCodec}, nil
}

// Call performs one RPC: it opens a fresh stream on the persistent connection,
// writes the request envelope, closes the stream's write side (so the server
// reads a complete frame then EOF), reads the response envelope, and decodes.
// A transport-level failure (dead conn) is returned as-is — reconnection is a
// concern for a higher seam layer (Krok 3), not the raw transport.
func (c *Client) Call(ctx context.Context, method string, req any, resp any) error {
	reqPayload, err := c.codec.Encode(req)
	if err != nil {
		return err
	}

	envBytes, err := c.codec.Encode(request{Method: method, Payload: reqPayload})
	if err != nil {
		return err
	}

	stream, err := c.conn.OpenStreamSync(ctx)
	if err != nil {
		return err
	}
	// Ensure the stream is not leaked if we return before a clean close.
	defer stream.CancelRead(0)

	if err := writeFrame(stream, envBytes); err != nil {
		return err
	}
	// Close the write side: signals EOF so the server can read the full frame.
	if err := stream.Close(); err != nil {
		return err
	}

	respBytes, err := readFrame(stream)
	if err != nil {
		return err
	}

	var env response
	if err := c.codec.Decode(respBytes, &env); err != nil {
		return err
	}
	if !env.OK {
		return errors.New(env.Error)
	}
	if resp != nil && len(env.Payload) > 0 {
		return c.codec.Decode(env.Payload, resp)
	}
	return nil
}

// CallRaw performs one RPC relaying raw payload bytes verbatim: payload is
// already-encoded request bytes (assigned straight into the envelope, never run
// through the codec again) and the returned bytes are the response payload
// exactly as the server sent them (no decode). This is the gateway relay path —
// it neither knows nor cares about the payload's shape. Stream lifecycle matches
// Call: fresh stream, write, close write side, read, decode only the envelope.
func (c *Client) CallRaw(ctx context.Context, method string, payload []byte) ([]byte, error) {
	envBytes, err := c.codec.Encode(request{Method: method, Payload: json.RawMessage(payload)})
	if err != nil {
		return nil, err
	}

	stream, err := c.conn.OpenStreamSync(ctx)
	if err != nil {
		return nil, err
	}
	// Ensure the stream is not leaked if we return before a clean close.
	defer stream.CancelRead(0)

	if err := writeFrame(stream, envBytes); err != nil {
		return nil, err
	}
	// Close the write side: signals EOF so the server can read the full frame.
	if err := stream.Close(); err != nil {
		return nil, err
	}

	respBytes, err := readFrame(stream)
	if err != nil {
		return nil, err
	}

	var env response
	if err := c.codec.Decode(respBytes, &env); err != nil {
		return nil, err
	}
	if !env.OK {
		return nil, errors.New(env.Error)
	}
	return env.Payload, nil
}

// Close tears down the persistent connection.
func (c *Client) Close() error {
	return c.conn.CloseWithError(0, "bye")
}
