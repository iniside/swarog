package outbox

import (
	"fmt"
	"sort"
	"testing"

	"pgregory.net/rapid"
)

// TestPropDeliverOrdering exercises deliver's documented contract (see the
// package doc comment on deliver): per-subscriber stop-on-first-failure,
// all-or-nothing per row, no-subscriber rows always sent, and sent is a
// strictly-ascending subsequence of the input ids — never reordered.
func TestPropDeliverOrdering(t *testing.T) {
	rapid.Check(t, func(t *rapid.T) {
		topics := []string{"a", "b", "c"}
		urlPool := []string{"http://u1", "http://u2", "http://u3"}

		// Ascending, strictly-increasing ids.
		n := rapid.IntRange(0, 12).Draw(t, "n")
		ids := make([]int64, n)
		var next int64 = 1
		for i := 0; i < n; i++ {
			next += rapid.Int64Range(1, 3).Draw(t, fmt.Sprintf("gap%d", i))
			ids[i] = next
		}

		pending := make([]outRow, n)
		for i, id := range ids {
			topic := rapid.SampledFrom(topics).Draw(t, fmt.Sprintf("topic%d", i))
			payload := rapid.SliceOfN(rapid.Byte(), 0, 8).Draw(t, fmt.Sprintf("payload%d", i))
			pending[i] = outRow{id: id, topic: topic, payload: payload}
		}

		// subscribers: topic -> subset of urlPool (possibly empty/absent).
		subscribers := map[string][]string{}
		for _, topic := range topics {
			if rapid.Bool().Draw(t, "has_"+topic) {
				urls := rapid.SliceOfNDistinct(rapid.SampledFrom(urlPool), 1, len(urlPool), rapid.ID[string]).
					Draw(t, "urls_"+topic)
				subscribers[topic] = urls
			}
		}

		// Precompute a deterministic failure pattern: for each (url) draw whether
		// it fails, and on which call-index (per url) it starts failing forever
		// after (so failures are a pure function of call count, not test state).
		failFromCall := map[string]int{} // url -> call index (0-based) at which it starts failing; -1 = never
		for _, url := range urlPool {
			if rapid.Bool().Draw(t, "fails_"+url) {
				failFromCall[url] = rapid.IntRange(0, n+1).Draw(t, "failat_"+url)
			} else {
				failFromCall[url] = -1
			}
		}
		callCount := map[string]int{}

		r := testRelay(subscribers)

		var postedByURL = map[string][]int64{}
		post := func(url, eventID string, _ []byte) error {
			idx := callCount[url]
			callCount[url]++
			// Recover the row id from eventID ("<schema>:<id>").
			var rowID int64
			_, _ = fmt.Sscanf(eventID, r.schema+":%d", &rowID)
			postedByURL[url] = append(postedByURL[url], rowID)
			if from := failFromCall[url]; from >= 0 && idx >= from {
				return fmt.Errorf("injected failure")
			}
			return nil
		}

		sent := r.deliver(pending, post)

		// (a) per-subscriber stop-on-first-failure: find, per url, the row id
		// whose call to that url failed; assert no later row id was ever posted
		// to that same url in this batch.
		for url, posts := range postedByURL {
			from := failFromCall[url]
			if from < 0 {
				continue
			}
			if from >= len(posts) {
				continue // never actually reached the failing call
			}
			failedRowID := posts[from]
			for _, rowID := range posts[from+1:] {
				if rowID > failedRowID {
					t.Fatalf("url %s: row %d posted to a blocked subscriber after row %d failed", url, rowID, failedRowID)
				}
			}
		}

		// (b) all-or-nothing: row id in sent iff every subscriber URL for its
		// topic returned nil for that row AND none of those URLs had already
		// failed on an earlier row in this batch.
		sentSet := map[int64]bool{}
		for _, id := range sent {
			sentSet[id] = true
		}
		blocked := map[string]bool{}
		for _, row := range pending {
			urls := subscribers[row.topic]
			if len(urls) == 0 {
				if !sentSet[row.id] {
					t.Fatalf("row %d (topic %q, no subscribers) must always be sent", row.id, row.topic)
				}
				continue
			}
			expectOK := true
			for _, url := range urls {
				if blocked[url] {
					expectOK = false
					continue
				}
				from := failFromCall[url]
				idx := indexOf(postedByURL[url], row.id)
				failedThisCall := idx >= 0 && from >= 0 && idx >= from
				if failedThisCall {
					blocked[url] = true
					expectOK = false
				}
			}
			if expectOK != sentSet[row.id] {
				t.Fatalf("row %d: expected sent=%v, got sent=%v", row.id, expectOK, sentSet[row.id])
			}
		}

		// (c) already covered above (no-subscriber rows always sent).

		// (d) sent is a strictly-ascending subsequence of the input ids.
		if !sort.SliceIsSorted(sent, func(i, j int) bool { return sent[i] < sent[j] }) {
			t.Fatalf("sent is not ascending: %v", sent)
		}
		inputIdx := map[int64]int{}
		for i, id := range ids {
			inputIdx[id] = i
		}
		lastIdx := -1
		for _, id := range sent {
			idx, ok := inputIdx[id]
			if !ok {
				t.Fatalf("sent id %d not present in input", id)
			}
			if idx <= lastIdx {
				t.Fatalf("sent ids out of input order: %v", sent)
			}
			lastIdx = idx
		}
	})
}

func indexOf(s []int64, v int64) int {
	for i, x := range s {
		if x == v {
			return i
		}
	}
	return -1
}

// TestPropParseSubscribersRoundTrip serializes an arbitrary topic->URLs map to
// the "topic=url1,url2;topic2=url3" wire format and asserts ParseSubscribers
// recovers it exactly (URLs are order-preserving per topic since repeats
// append, per the doc comment on ParseSubscribers).
func TestPropParseSubscribersRoundTrip(t *testing.T) {
	rapid.Check(t, func(t *rapid.T) {
		tokenGen := rapid.StringMatching(`[a-zA-Z0-9_./:-]{1,10}`)
		topics := rapid.SliceOfNDistinct(tokenGen, 0, 5, rapid.ID[string]).Draw(t, "topics")

		want := map[string][]string{}
		var sb []string
		for _, topic := range topics {
			urls := rapid.SliceOfN(tokenGen, 1, 4).Draw(t, "urls_"+topic)
			want[topic] = append(want[topic], urls...)
			sb = append(sb, topic+"="+joinComma(urls))
		}
		raw := joinSemi(sb)

		got := ParseSubscribers(raw)
		if len(got) != len(want) {
			t.Fatalf("ParseSubscribers(%q) = %#v, want %#v", raw, got, want)
		}
		for topic, urls := range want {
			if !equalSlices(got[topic], urls) {
				t.Fatalf("topic %q: got %v, want %v (raw=%q)", topic, got[topic], urls, raw)
			}
		}
	})
}

func joinComma(s []string) string {
	out := ""
	for i, v := range s {
		if i > 0 {
			out += ","
		}
		out += v
	}
	return out
}

func joinSemi(s []string) string {
	out := ""
	for i, v := range s {
		if i > 0 {
			out += ";"
		}
		out += v
	}
	return out
}

func equalSlices(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}
