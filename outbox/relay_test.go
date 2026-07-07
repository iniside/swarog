package outbox

import (
	"context"
	"io"
	"log/slog"
	"reflect"
	"testing"
)

func testRelay(subs map[string][]string) *Relay {
	return testRelayLocal(subs, nil)
}

func testRelayLocal(subs map[string][]string, locals []LocalTarget) *Relay {
	return &Relay{
		schema:       "characters",
		subscribers:  subs,
		localTargets: locals,
		log:          slog.New(slog.NewTextHandler(io.Discard, nil)),
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

// TestDeliverNoTargets: a topic with neither local targets nor remote
// subscribers drains to nobody and is marked sent immediately (the monolith path
// with an empty subscriber set).
func TestDeliverNoTargets(t *testing.T) {
	r := testRelay(map[string][]string{})
	rows := []outRow{{id: 1, topic: "character.created"}, {id: 2, topic: "character.deleted"}}
	sent := r.deliver(context.Background(), rows, func(context.Context, string, string, string, []byte) error {
		t.Fatal("post should not be called with no subscribers")
		return nil
	})
	if !reflect.DeepEqual(sent, []int64{1, 2}) {
		t.Fatalf("sent = %v, want [1 2]", sent)
	}
}

// TestDeliverLocalOnlyMonolith: with NO remote subscribers, local targets are
// still delivered (unconditional local delivery — the monolith with empty
// EVENTS_SUBSCRIBERS must still reach in-process subscribers).
func TestDeliverLocalOnlyMonolith(t *testing.T) {
	var got []int64
	r := testRelayLocal(map[string][]string{}, []LocalTarget{{
		Subscriber: "inventory",
		Deliver: func(_ context.Context, _ string, _ []byte, eventID string) error {
			if eventID != "characters:1" {
				t.Fatalf("event id = %q, want characters:1", eventID)
			}
			got = append(got, 1)
			return nil
		},
	}})
	sent := r.deliver(context.Background(), []outRow{{id: 1, topic: "character.created"}},
		func(context.Context, string, string, string, []byte) error {
			t.Fatal("post should not be called with no remote subscribers")
			return nil
		})
	if !reflect.DeepEqual(sent, []int64{1}) {
		t.Fatalf("sent = %v, want [1]", sent)
	}
	if !reflect.DeepEqual(got, []int64{1}) {
		t.Fatalf("local target not delivered: got %v", got)
	}
}

// TestDeliverMarkSentNeedsLocalAndRemote: a row is marked sent only when BOTH its
// local target and its remote URL ack; if the local target fails the row is not
// sent even though the remote succeeded.
func TestDeliverMarkSentNeedsLocalAndRemote(t *testing.T) {
	r := testRelayLocal(map[string][]string{"t": {"http://ok"}}, []LocalTarget{{
		Subscriber: "inventory",
		Deliver: func(context.Context, string, []byte, string) error {
			return io.ErrUnexpectedEOF // local fails
		},
	}})
	sent := r.deliver(context.Background(), []outRow{{id: 5, topic: "t"}},
		func(context.Context, string, string, string, []byte) error { return nil }) // remote ok
	if len(sent) != 0 {
		t.Fatalf("sent = %v, want none (local target failed though remote acked)", sent)
	}
}

// TestDeliverEventID: the stable event id is <schema>:<id>.
func TestDeliverEventID(t *testing.T) {
	r := testRelay(map[string][]string{"character.created": {"http://b/c"}})
	var gotID string
	r.deliver(context.Background(), []outRow{{id: 7, topic: "character.created"}},
		func(_ context.Context, _, _, eventID string, _ []byte) error {
			gotID = eventID
			return nil
		})
	if gotID != "characters:7" {
		t.Fatalf("event id = %q, want characters:7", gotID)
	}
}

// TestDeliverStopsOnFirstFailure: once a POST for a (topic, url) fails, no later
// row of THAT topic is delivered to that subscriber this batch, and neither row
// is marked sent — so a later event can't overtake an earlier one for the same
// (topic, subscriber) (per-subscriber ordering / S6).
func TestDeliverStopsOnFirstFailure(t *testing.T) {
	url := "http://b/events"
	r := testRelay(map[string][]string{"character.created": {url}})
	rows := []outRow{
		{id: 10, topic: "character.created"},
		{id: 11, topic: "character.created"},
	}
	var posted []int64
	sent := r.deliver(context.Background(), rows, func(_ context.Context, _, _, eventID string, _ []byte) error {
		// Fail the first POST; the second (same topic+url) must never be attempted.
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

// TestDeliverPerTopicURLIsolation: a failed (topicA, url) must NOT block a
// (topicB, url) to the same peer — a poison event of one topic can't stall a
// different topic to the same subscriber.
func TestDeliverPerTopicURLIsolation(t *testing.T) {
	url := "http://b/events"
	r := testRelay(map[string][]string{
		"topic.a": {url}, // will fail
		"topic.b": {url}, // same url, different topic — must still deliver
	})
	rows := []outRow{
		{id: 1, topic: "topic.a"},
		{id: 2, topic: "topic.b"},
	}
	sent := r.deliver(context.Background(), rows, func(_ context.Context, _, topic, _ string, _ []byte) error {
		if topic == "topic.a" {
			return io.ErrUnexpectedEOF
		}
		return nil
	})
	if !reflect.DeepEqual(sent, []int64{2}) {
		t.Fatalf("sent = %v, want [2] (topic.b to same url unaffected by topic.a failure)", sent)
	}
}

// TestDeliverLocalFailureIsolation: a failing local subscriber blocks only its
// own topic's later rows, not a different topic.
func TestDeliverLocalFailureIsolation(t *testing.T) {
	var delivered []int64
	r := testRelayLocal(map[string][]string{}, []LocalTarget{{
		Subscriber: "inventory",
		Deliver: func(_ context.Context, topic string, _ []byte, eventID string) error {
			if topic == "topic.a" {
				return io.ErrUnexpectedEOF // topic.a always fails
			}
			var id int64
			switch eventID {
			case "characters:2":
				id = 2
			case "characters:4":
				id = 4
			}
			delivered = append(delivered, id)
			return nil
		},
	}})
	rows := []outRow{
		{id: 1, topic: "topic.a"}, // fails, blocks (inventory, topic.a)
		{id: 2, topic: "topic.b"}, // different topic — delivered
		{id: 3, topic: "topic.a"}, // blocked (same topic+subscriber)
		{id: 4, topic: "topic.b"}, // still delivered
	}
	sent := r.deliver(context.Background(), rows, func(context.Context, string, string, string, []byte) error {
		return nil
	})
	if !reflect.DeepEqual(sent, []int64{2, 4}) {
		t.Fatalf("sent = %v, want [2 4] (topic.b unaffected by topic.a local failure)", sent)
	}
	if !reflect.DeepEqual(delivered, []int64{2, 4}) {
		t.Fatalf("delivered = %v, want [2 4]", delivered)
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
	sent := r.deliver(context.Background(), rows, func(_ context.Context, url, _, _ string, _ []byte) error {
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
	sent := r.deliver(context.Background(), []outRow{{id: 5, topic: "t"}},
		func(_ context.Context, url, _, _ string, _ []byte) error {
			if url == "http://bad" {
				return io.ErrUnexpectedEOF
			}
			return nil
		})
	if len(sent) != 0 {
		t.Fatalf("sent = %v, want none (one of two subscribers failed)", sent)
	}
}
