package outbox

import (
	"io"
	"log/slog"
	"reflect"
	"testing"
)

func testRelay(subs map[string][]string) *Relay {
	return &Relay{
		schema:      "characters",
		subscribers: subs,
		log:         slog.New(slog.NewTextHandler(io.Discard, nil)),
	}
}

func TestParseSubscribers(t *testing.T) {
	cases := []struct {
		name string
		in   string
		want map[string][]string
	}{
		{"empty", "", map[string][]string{}},
		{"whitespace", "   ", map[string][]string{}},
		{
			"two topics",
			"character.created=http://b/events/created;character.deleted=http://b/events/deleted",
			map[string][]string{
				"character.created": {"http://b/events/created"},
				"character.deleted": {"http://b/events/deleted"},
			},
		},
		{
			"multi url + repeat topic, trimmed",
			" character.created = http://b/c , http://c/c ; character.created=http://d/c ",
			map[string][]string{
				"character.created": {"http://b/c", "http://c/c", "http://d/c"},
			},
		},
		{"skips blank + malformed", "=nope;;character.x=http://b/x", map[string][]string{
			"character.x": {"http://b/x"},
		}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := ParseSubscribers(tc.in); !reflect.DeepEqual(got, tc.want) {
				t.Fatalf("ParseSubscribers(%q) = %#v, want %#v", tc.in, got, tc.want)
			}
		})
	}
}

// TestDeliverNoSubscribers: a topic with no subscribers drains to nobody and is
// marked sent immediately (the monolith path).
func TestDeliverNoSubscribers(t *testing.T) {
	r := testRelay(map[string][]string{})
	rows := []outRow{{id: 1, topic: "character.created"}, {id: 2, topic: "character.deleted"}}
	sent := r.deliver(rows, func(string, string, []byte) error {
		t.Fatal("post should not be called with no subscribers")
		return nil
	})
	if !reflect.DeepEqual(sent, []int64{1, 2}) {
		t.Fatalf("sent = %v, want [1 2]", sent)
	}
}

// TestDeliverEventID: the stable event id is <schema>:<id>.
func TestDeliverEventID(t *testing.T) {
	r := testRelay(map[string][]string{"character.created": {"http://b/c"}})
	var gotID string
	r.deliver([]outRow{{id: 7, topic: "character.created"}}, func(_, eventID string, _ []byte) error {
		gotID = eventID
		return nil
	})
	if gotID != "characters:7" {
		t.Fatalf("event id = %q, want characters:7", gotID)
	}
}

// TestDeliverStopsOnFirstFailure: once a POST to a subscriber fails, no later row
// is delivered to that subscriber this batch, and neither row is marked sent —
// so a delete can't overtake a create (per-subscriber ordering / S6).
func TestDeliverStopsOnFirstFailure(t *testing.T) {
	url := "http://b/events"
	r := testRelay(map[string][]string{
		"character.created": {url},
		"character.deleted": {url},
	})
	rows := []outRow{
		{id: 10, topic: "character.created"},
		{id: 11, topic: "character.deleted"},
	}
	var posted []int64
	sent := r.deliver(rows, func(_, eventID string, _ []byte) error {
		// Fail the first (create) POST; the second must never be attempted.
		if eventID == "characters:10" {
			return io.ErrUnexpectedEOF
		}
		posted = append(posted, 11)
		return nil
	})
	if len(sent) != 0 {
		t.Fatalf("sent = %v, want none (subscriber blocked after first failure)", sent)
	}
	if len(posted) != 0 {
		t.Fatalf("later row was delivered to a blocked subscriber: %v", posted)
	}
}

// TestDeliverIndependentSubscribers: a failure to one subscriber must not stall a
// row whose OWN subscribers all succeed (ordering is per-subscriber, not global).
func TestDeliverIndependentSubscribers(t *testing.T) {
	r := testRelay(map[string][]string{
		"topic.a": {"http://a"}, // will fail
		"topic.b": {"http://b"}, // will succeed
	})
	rows := []outRow{
		{id: 1, topic: "topic.a"},
		{id: 2, topic: "topic.b"},
	}
	sent := r.deliver(rows, func(url, _ string, _ []byte) error {
		if url == "http://a" {
			return io.ErrUnexpectedEOF
		}
		return nil
	})
	if !reflect.DeepEqual(sent, []int64{2}) {
		t.Fatalf("sent = %v, want [2] (b succeeds though a fails)", sent)
	}
}

// TestDeliverPartialFanoutNotSent: a row with two subscribers where only one
// accepts is NOT marked sent (all-or-nothing per row); the accepting subscriber
// will be re-POSTed next tick and deduped by the inbox.
func TestDeliverPartialFanoutNotSent(t *testing.T) {
	r := testRelay(map[string][]string{
		"t": {"http://ok", "http://bad"},
	})
	sent := r.deliver([]outRow{{id: 5, topic: "t"}}, func(url, _ string, _ []byte) error {
		if url == "http://bad" {
			return io.ErrUnexpectedEOF
		}
		return nil
	})
	if len(sent) != 0 {
		t.Fatalf("sent = %v, want none (one of two subscribers failed)", sent)
	}
}
