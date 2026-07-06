package edge

import (
	"encoding/binary"
	"fmt"
	"io"
)

// maxFrameSize caps a single frame's payload to avoid a malicious or corrupt
// length prefix triggering an unbounded allocation. 16 MiB is far above any
// legitimate RPC envelope.
const maxFrameSize = 16 << 20 // 16 MiB

// writeFrame writes a length-prefixed frame: a 4-byte big-endian length followed
// by the payload bytes, in a single Write so the framing header and body are not
// split across the stream by an intermediate flush.
func writeFrame(w io.Writer, b []byte) error {
	if len(b) > maxFrameSize {
		return fmt.Errorf("edge: frame too large: %d > %d", len(b), maxFrameSize)
	}
	buf := make([]byte, 4+len(b))
	binary.BigEndian.PutUint32(buf[:4], uint32(len(b)))
	copy(buf[4:], b)
	_, err := w.Write(buf)
	return err
}

// readFrame reads a single length-prefixed frame written by writeFrame. It reads
// the 4-byte length, guards it against maxFrameSize, then reads exactly that many
// payload bytes. io.ReadFull surfaces a truncated frame as io.ErrUnexpectedEOF.
func readFrame(r io.Reader) ([]byte, error) {
	var lenBuf [4]byte
	if _, err := io.ReadFull(r, lenBuf[:]); err != nil {
		return nil, err
	}
	n := binary.BigEndian.Uint32(lenBuf[:])
	if n > maxFrameSize {
		return nil, fmt.Errorf("edge: frame too large: %d > %d", n, maxFrameSize)
	}
	b := make([]byte, n)
	if _, err := io.ReadFull(r, b); err != nil {
		return nil, err
	}
	return b, nil
}
