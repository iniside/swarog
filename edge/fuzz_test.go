package edge

import (
	"bytes"
	"errors"
	"testing"
)

// FuzzReadFrame throws arbitrary bytes at readFrame: it must never panic,
// regardless of a corrupt length prefix or a truncated body. The maxFrameSize
// guard must reject a huge length prefix before it reaches make().
func FuzzReadFrame(f *testing.F) {
	f.Add([]byte{0, 0, 0, 0})                // zero-length frame
	f.Add([]byte{0, 0, 0, 3, 'a', 'b', 'c'}) // a normal 3-byte frame
	f.Add([]byte{0xff, 0xff, 0xff, 0xff})    // huge length prefix, no body
	f.Add([]byte{0, 0, 0, 10, 1, 2, 3})      // truncated body (claims 10, has 3)

	f.Fuzz(func(t *testing.T, raw []byte) {
		_, _ = readFrame(bytes.NewReader(raw))
	})
}

// FuzzFrameRoundTrip asserts writeFrame/readTrip is lossless for any payload up
// to maxFrameSize, and that an oversized payload is rejected by writeFrame (not
// silently truncated).
func FuzzFrameRoundTrip(f *testing.F) {
	f.Add([]byte(""))
	f.Add([]byte("hello"))
	f.Add(bytes.Repeat([]byte("x"), 4096)) // a few KB, not 16 MiB — keeps the fuzzer fast

	f.Fuzz(func(t *testing.T, payload []byte) {
		var buf bytes.Buffer
		err := writeFrame(&buf, payload)
		if len(payload) > maxFrameSize {
			if err == nil {
				t.Fatalf("writeFrame accepted oversized payload of %d bytes", len(payload))
			}
			return
		}
		if err != nil {
			t.Fatalf("writeFrame(%d bytes): %v", len(payload), err)
		}
		got, rerr := readFrame(&buf)
		if rerr != nil {
			t.Fatalf("readFrame after writeFrame: %v", rerr)
		}
		if !bytes.Equal(got, payload) {
			t.Fatalf("round-trip mismatch: got %d bytes, want %d bytes", len(got), len(payload))
		}
	})
}

// FuzzCodecDecodeRequest asserts the default codec never panics decoding
// arbitrary bytes into a request envelope (errors are fine; panics are not).
func FuzzCodecDecodeRequest(f *testing.F) {
	f.Add([]byte(`{"method":"x","payload":null}`))
	f.Add([]byte(`{"method":"echo","payload":"hi"}`))
	f.Add([]byte("garbage"))
	f.Add([]byte{})

	f.Fuzz(func(t *testing.T, data []byte) {
		var req request
		_ = defaultCodec.Decode(data, &req)
	})
}

// FuzzDispatch drives dispatch with arbitrary request bytes against a server
// wired with exact echo/boom/panic handlers plus an "x." prefix. It regression-
// guards the recover-before-decode fix: dispatch must never panic, must never
// return OK with an error set nor a not-OK with a payload, and its response must
// always re-encode. No listener/TLS is needed — dispatch runs on a bare Server.
func FuzzDispatch(f *testing.F) {
	srv := NewServer()
	srv.Handle("echo", func(reqPayload []byte) ([]byte, error) { return reqPayload, nil })
	srv.Handle("boom", func([]byte) ([]byte, error) { return nil, errors.New("boom: on purpose") })
	srv.Handle("panic", func([]byte) ([]byte, error) { panic("kaboom") })
	srv.HandlePrefix("x.", func(_ string, payload []byte) ([]byte, error) { return payload, nil })

	f.Add([]byte(`{"method":"echo","payload":"hi"}`))
	f.Add([]byte(`{"method":"boom"}`))
	f.Add([]byte(`{"method":"panic"}`))
	f.Add([]byte(`{"method":"x.foo","payload":123}`))
	f.Add([]byte(`{"method":"unknown"}`))
	f.Add([]byte("not json"))

	f.Fuzz(func(t *testing.T, reqBytes []byte) {
		resp := srv.dispatch(reqBytes)
		if resp.OK && resp.Error != "" {
			t.Fatalf("dispatch returned OK with an error set: %q", resp.Error)
		}
		if !resp.OK && resp.Payload != nil {
			t.Fatalf("dispatch returned not-OK with a payload: %q", resp.Payload)
		}
		if _, err := srv.codec.Encode(resp); err != nil {
			t.Fatalf("response failed to re-encode: %v", err)
		}
	})
}
