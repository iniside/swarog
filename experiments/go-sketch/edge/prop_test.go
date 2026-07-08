package edge

import (
	"bytes"
	"strings"
	"testing"

	"pgregory.net/rapid"
)

// codecSample is a small comparable struct used to exercise codec round-tripping
// with generated values. Strings are constrained to a clean, JSON-safe alphabet
// so the comparison is exact (no UTF-8 replacement to reason about).
type codecSample struct {
	Name  string `json:"name"`
	Count int    `json:"count"`
	Blob  string `json:"blob"`
}

// TestPropFrameRoundTrip: for any payload up to ~8 KiB, writeFrame followed by
// readFrame yields the original bytes with no error.
func TestPropFrameRoundTrip(t *testing.T) {
	rapid.Check(t, func(t *rapid.T) {
		payload := rapid.SliceOfN(rapid.Byte(), 0, 8<<10).Draw(t, "payload")

		var buf bytes.Buffer
		if err := writeFrame(&buf, payload); err != nil {
			t.Fatalf("writeFrame(%d bytes): %v", len(payload), err)
		}
		got, err := readFrame(&buf)
		if err != nil {
			t.Fatalf("readFrame: %v", err)
		}
		if !bytes.Equal(got, payload) {
			t.Fatalf("round-trip mismatch: got %d bytes, want %d bytes", len(got), len(payload))
		}
	})
}

// TestPropCodecRoundTrip: Decode(Encode(v)) == v for a generated struct.
func TestPropCodecRoundTrip(t *testing.T) {
	rapid.Check(t, func(t *rapid.T) {
		want := codecSample{
			Name:  rapid.StringMatching(`[a-zA-Z0-9 ]{0,16}`).Draw(t, "name"),
			Count: rapid.Int().Draw(t, "count"),
			Blob:  rapid.StringMatching(`[a-zA-Z0-9 ]{0,16}`).Draw(t, "blob"),
		}

		data, err := defaultCodec.Encode(want)
		if err != nil {
			t.Fatalf("Encode: %v", err)
		}
		var got codecSample
		if err := defaultCodec.Decode(data, &got); err != nil {
			t.Fatalf("Decode: %v", err)
		}
		if got != want {
			t.Fatalf("codec round-trip mismatch: got %+v want %+v", got, want)
		}
	})
}

// TestPropPrefixLongestMatch: for any set of distinct registered prefixes and any
// method string, longestPrefix matches iff some prefix is a strings.HasPrefix of
// the method, and when it matches it selects the LONGEST such prefix. Each handler
// is tagged with its own prefix so the winner is identifiable; an oracle loop
// computes the expected result independently.
func TestPropPrefixLongestMatch(t *testing.T) {
	rapid.Check(t, func(t *rapid.T) {
		prefixes := rapid.SliceOfNDistinct(
			rapid.StringMatching(`[a-z]{1,5}\.`), 0, 8, rapid.ID[string],
		).Draw(t, "prefixes")

		srv := NewServer()
		for _, p := range prefixes {
			tag := p
			srv.HandlePrefix(p, func(_ string, _ []byte) ([]byte, error) {
				return []byte(tag), nil
			})
		}

		var method string
		if len(prefixes) > 0 && rapid.Bool().Draw(t, "useRegistered") {
			chosen := rapid.SampledFrom(prefixes).Draw(t, "chosen")
			method = chosen + rapid.StringMatching(`[a-z.]{0,6}`).Draw(t, "suffix")
		} else {
			method = rapid.StringMatching(`[a-z.]{0,12}`).Draw(t, "method")
		}

		// Oracle: the longest registered prefix that method starts with.
		bestLen := -1
		bestTag := ""
		for _, p := range prefixes {
			if strings.HasPrefix(method, p) && len(p) > bestLen {
				bestLen = len(p)
				bestTag = p
			}
		}
		wantOK := bestLen >= 0

		fwd, ok := srv.longestPrefix(method)
		if ok != wantOK {
			t.Fatalf("longestPrefix(%q) ok=%v, want %v (prefixes=%v)", method, ok, wantOK, prefixes)
		}
		if ok {
			gotTag, _ := fwd("", nil)
			if string(gotTag) != bestTag {
				t.Fatalf("longestPrefix(%q) chose %q, want %q (prefixes=%v)", method, gotTag, bestTag, prefixes)
			}
		}
	})
}
