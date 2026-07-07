package outbox

import (
	"context"
	"database/sql"
	"io"
	"log/slog"
	"os"
	"reflect"
	"strconv"
	"testing"
	"time"

	_ "github.com/jackc/pgx/v5/stdlib"
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

// outboxDDL is the minimal messaging.outbox shape drainOnce touches (id/origin/
// topic/payload/sent_at). Idempotent — coexists with messaging's fuller schema.
const outboxDDL = `
CREATE SCHEMA IF NOT EXISTS messaging;
CREATE TABLE IF NOT EXISTS messaging.outbox (
	id         bigserial   PRIMARY KEY,
	origin     text        NOT NULL,
	topic      text        NOT NULL,
	payload    jsonb       NOT NULL,
	created_at timestamptz NOT NULL DEFAULT now(),
	sent_at    timestamptz
);`

// testOutboxDB opens the local Postgres, ensures the messaging.outbox table exists,
// and skips gracefully when Postgres is unreachable (repo test-DB convention).
func testOutboxDB(t *testing.T) *sql.DB {
	t.Helper()
	dsn := os.Getenv("DATABASE_URL")
	if dsn == "" {
		dsn = "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable"
	}
	db, err := sql.Open("pgx", dsn)
	if err != nil {
		t.Skipf("no postgres: %v", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	if err := db.PingContext(ctx); err != nil {
		_ = db.Close()
		t.Skipf("postgres unreachable: %v", err)
	}
	if _, err := db.Exec(outboxDDL); err != nil {
		t.Fatalf("migrate: %v", err)
	}
	return db
}

// TestRelayDrainsOnlyOwnOrigin is the BLOCKER-1 regression: two processes share one
// messaging.outbox, one row per origin. A relay running as origin "B" must drain
// ONLY B's row — never mark-sent (and thus swallow) A's row, which A's own relay
// owns. This is the exact scenario the review caught (scheduler-svc's relay eating
// characters-svc's character.created). Without the `WHERE origin = $self ... FOR
// UPDATE SKIP LOCKED` filter, B's drain would deliver-nowhere and mark A's row sent,
// silently dropping A's event.
func TestRelayDrainsOnlyOwnOrigin(t *testing.T) {
	db := testOutboxDB(t)
	defer func() { _ = db.Close() }()
	ctx := context.Background()

	// Unique origins per run so this relay's pending() never sees a prior run's rows.
	var suffix string
	if err := db.QueryRow(`SELECT gen_random_uuid()::text`).Scan(&suffix); err != nil {
		t.Fatal(err)
	}
	originA, originB := "A-"+suffix, "B-"+suffix

	var idA, idB int64
	if err := db.QueryRow(
		`INSERT INTO messaging.outbox (origin, topic, payload) VALUES ($1, 'a.topic', '{}'::jsonb) RETURNING id`,
		originA).Scan(&idA); err != nil {
		t.Fatalf("insert A: %v", err)
	}
	if err := db.QueryRow(
		`INSERT INTO messaging.outbox (origin, topic, payload) VALUES ($1, 'b.topic', '{}'::jsonb) RETURNING id`,
		originB).Scan(&idB); err != nil {
		t.Fatalf("insert B: %v", err)
	}

	// A relay owned by origin B, with a local target that records what it delivers.
	var delivered []string
	r := &Relay{
		db:          db,
		schema:      "messaging",
		origin:      originB,
		subscribers: map[string][]string{},
		localTargets: []LocalTarget{{
			Subscriber: "recorder",
			Deliver: func(_ context.Context, _ string, _ []byte, eventID string) error {
				delivered = append(delivered, eventID)
				return nil
			},
		}},
		log: slog.New(slog.NewTextHandler(io.Discard, nil)),
	}
	if err := r.drainOnce(ctx); err != nil {
		t.Fatalf("drainOnce: %v", err)
	}

	// B's relay delivered exactly B's row (event id messaging:<idB>) and nothing else.
	wantEventID := "messaging:" + strconv.FormatInt(idB, 10)
	if !reflect.DeepEqual(delivered, []string{wantEventID}) {
		t.Fatalf("delivered = %v, want [%s] (B must drain only its own origin's row)", delivered, wantEventID)
	}

	// B's row is now marked sent...
	var bSent sql.NullTime
	if err := db.QueryRow(`SELECT sent_at FROM messaging.outbox WHERE id=$1`, idB).Scan(&bSent); err != nil {
		t.Fatal(err)
	}
	if !bSent.Valid {
		t.Fatal("origin-B row not marked sent by B's own relay")
	}
	// ...and A's row is UNTOUCHED (sent_at IS NULL) — B's relay did not swallow it.
	var aSent sql.NullTime
	if err := db.QueryRow(`SELECT sent_at FROM messaging.outbox WHERE id=$1`, idA).Scan(&aSent); err != nil {
		t.Fatal(err)
	}
	if aSent.Valid {
		t.Fatalf("origin-A row was marked sent by B's relay (sent_at=%v) — a foreign relay swallowed another origin's event", aSent.Time)
	}
}
