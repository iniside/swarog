// Package outbox is a transactional-outbox relay: a generic, domain-agnostic
// helper that drains rows a publishing module wrote (in the same DB transaction
// as its domain change) and POSTs them to remote subscribers over HTTP. It is
// NOT part of core and imports no module implementation — it works purely off a
// schema name plus a topic→URLs config, so the same relay serves any producer.
//
// Delivery contract:
//   - At-least-once. A stable event id (`<schema>:<outbox.id>`) is sent with each
//     POST so an idempotent subscriber (an inbox keyed on that id) dedups retries.
//   - Per-subscriber ordering. Rows are POSTed in ascending outbox id; on the
//     first failure to a given subscriber URL the relay stops advancing for THAT
//     url this tick (a later event can't overtake an earlier one — no
//     delete-before-create). A row is marked sent only once ALL its subscribers
//     accepted it (2xx). Zero configured subscribers for a topic = delivered to
//     nobody = success, marked sent immediately (the monolith path).
package outbox

import (
	"bytes"
	"context"
	"database/sql"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"regexp"
	"strings"
	"time"
)

// defaultInterval is how often the relay drains the outbox.
const defaultInterval = 500 * time.Millisecond

// identRe guards the schema name interpolated into SQL (there is no bind
// parameter for an identifier). Only a plain SQL identifier is accepted, so the
// interpolation below can never carry attacker-controlled input.
var identRe = regexp.MustCompile(`^[a-zA-Z_][a-zA-Z0-9_]*$`)

// Relay drains a schema's outbox table and delivers rows to HTTP subscribers.
type Relay struct {
	db          *sql.DB
	schema      string
	subscribers map[string][]string // topic -> subscriber URLs
	client      *http.Client
	interval    time.Duration
	log         *slog.Logger

	cancel context.CancelFunc
	done   chan struct{}
}

// NewRelay builds a relay for the given schema's outbox table. subscribers maps
// each event topic to the URLs to POST it to (empty/nil = no remote subscribers,
// i.e. the monolith, where rows drain to nobody and are marked sent at once). It
// panics on a non-identifier schema — a wiring bug, loud at startup.
func NewRelay(db *sql.DB, schema string, subscribers map[string][]string, log *slog.Logger) *Relay {
	if !identRe.MatchString(schema) {
		panic(fmt.Sprintf("outbox: invalid schema name %q", schema))
	}
	if subscribers == nil {
		subscribers = map[string][]string{}
	}
	return &Relay{
		db:          db,
		schema:      schema,
		subscribers: subscribers,
		client:      &http.Client{Timeout: 5 * time.Second},
		interval:    defaultInterval,
		log:         log,
	}
}

// Start launches the drain loop in the background and returns immediately. The
// passed ctx bounds only startup (there is no I/O here); the loop runs under its
// own background context until Stop cancels it — so a short Start deadline can't
// kill the relay.
//
//nolint:contextcheck // intentional: the drain loop's lifetime is bounded by Stop, not Start's ctx.
func (r *Relay) Start(_ context.Context) error {
	// The loop must outlive the Start deadline, so it deliberately roots a fresh
	// background context rather than deriving from the passed one.
	runCtx, cancel := context.WithCancel(context.Background())
	r.cancel = cancel
	r.done = make(chan struct{})
	go func() {
		defer close(r.done)
		r.run(runCtx)
	}()
	return nil
}

// Stop cancels the drain loop and waits for it to exit (bounded by ctx).
func (r *Relay) Stop(ctx context.Context) error {
	if r.cancel != nil {
		r.cancel()
	}
	if r.done == nil {
		return nil
	}
	select {
	case <-r.done:
	case <-ctx.Done():
	}
	return nil
}

func (r *Relay) run(ctx context.Context) {
	t := time.NewTicker(r.interval)
	defer t.Stop()
	// Drain once immediately so a monolith isn't left with a startup backlog for
	// up to a full interval.
	if err := r.drainOnce(ctx); err != nil && ctx.Err() == nil {
		r.log.Error("outbox drain failed", "schema", r.schema, "err", err)
	}
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			if err := r.drainOnce(ctx); err != nil && ctx.Err() == nil {
				r.log.Error("outbox drain failed", "schema", r.schema, "err", err)
			}
		}
	}
}

// outRow is one unsent outbox row.
type outRow struct {
	id      int64
	topic   string
	payload []byte
}

// drainOnce reads every unsent row in id order, delivers them (per-subscriber
// ordering enforced by deliver), and marks the fully-delivered ones sent.
func (r *Relay) drainOnce(ctx context.Context) error {
	pending, err := r.pending(ctx)
	if err != nil {
		return err
	}
	if len(pending) == 0 {
		return nil
	}
	sent := r.deliver(pending, func(url, eventID string, payload []byte) error {
		return r.post(ctx, url, eventID, payload)
	})
	for _, id := range sent {
		if err := r.markSent(ctx, id); err != nil {
			r.log.Error("outbox mark sent failed", "schema", r.schema, "id", id, "err", err)
		}
	}
	return nil
}

// deliver decides which rows are fully delivered given a batch of unsent rows in
// ascending id order and a post function. It enforces per-subscriber ordering:
// once a POST to a subscriber URL fails, no further row is POSTed to THAT url in
// this batch, so an earlier event is never overtaken by a later one for the same
// subscriber. A row is returned (to be marked sent) only when all its
// subscribers accepted it; a topic with no subscribers is delivered to nobody
// and counts as sent. Pure over post — unit-tested without Postgres or HTTP.
func (r *Relay) deliver(pending []outRow, post func(url, eventID string, payload []byte) error) []int64 {
	blocked := map[string]bool{} // subscriber URLs that failed this batch
	var sent []int64
	for _, row := range pending {
		urls := r.subscribers[row.topic]
		if len(urls) == 0 {
			sent = append(sent, row.id) // delivered to nobody = success
			continue
		}
		eventID := fmt.Sprintf("%s:%d", r.schema, row.id)
		allOK := true
		for _, url := range urls {
			if blocked[url] {
				allOK = false // can't skip ahead of an earlier undelivered row
				continue
			}
			if err := post(url, eventID, row.payload); err != nil {
				r.log.Warn("outbox delivery failed", "url", url, "event_id", eventID, "err", err)
				blocked[url] = true
				allOK = false
			}
		}
		if allOK {
			sent = append(sent, row.id)
		}
	}
	return sent
}

func (r *Relay) pending(ctx context.Context) ([]outRow, error) {
	// #nosec G201 -- schema is validated as a bare SQL identifier in NewRelay.
	q := fmt.Sprintf(`SELECT id, topic, payload FROM %s.outbox WHERE sent_at IS NULL ORDER BY id`, r.schema)
	rows, err := r.db.QueryContext(ctx, q)
	if err != nil {
		return nil, err
	}
	defer func() { _ = rows.Close() }()
	var out []outRow
	for rows.Next() {
		var row outRow
		if err := rows.Scan(&row.id, &row.topic, &row.payload); err != nil {
			return nil, err
		}
		out = append(out, row)
	}
	return out, rows.Err()
}

func (r *Relay) markSent(ctx context.Context, id int64) error {
	// #nosec G201 -- schema is validated as a bare SQL identifier in NewRelay.
	q := fmt.Sprintf(`UPDATE %s.outbox SET sent_at = now() WHERE id = $1`, r.schema)
	_, err := r.db.ExecContext(ctx, q, id)
	return err
}

// post delivers one row to one subscriber. The stable event id rides in the
// X-Event-Id header so the subscriber can dedup; the body is the raw event JSON
// exactly as the producer stored it. Any non-2xx (or transport error) is a
// failure, which stops advancing for this subscriber (retried next tick).
func (r *Relay) post(ctx context.Context, url, eventID string, payload []byte) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(payload))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("X-Event-Id", eventID)
	resp, err := r.client.Do(req)
	if err != nil {
		return err
	}
	defer func() { _ = resp.Body.Close() }()
	_, _ = io.Copy(io.Discard, resp.Body) // drain for connection reuse
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("outbox: subscriber %s returned %d", url, resp.StatusCode)
	}
	return nil
}

// ParseSubscribers parses the EVENTS_SUBSCRIBERS env value into a topic→URLs map.
//
// Shape: semicolon-separated entries, each `topic=url` (URLs may be
// comma-separated for multiple subscribers, and a topic may repeat — both
// append). Whitespace around tokens is trimmed; blank entries are skipped.
// Empty/unset input yields an empty map (the monolith: no remote subscribers).
//
// Example:
//
//	character.created=http://localhost:8081/events/character-created;character.deleted=http://localhost:8081/events/character-deleted
func ParseSubscribers(raw string) map[string][]string {
	out := map[string][]string{}
	for entry := range strings.SplitSeq(raw, ";") {
		entry = strings.TrimSpace(entry)
		if entry == "" {
			continue
		}
		topic, urls, ok := strings.Cut(entry, "=")
		topic = strings.TrimSpace(topic)
		if !ok || topic == "" {
			continue
		}
		for u := range strings.SplitSeq(urls, ",") {
			if u = strings.TrimSpace(u); u != "" {
				out[topic] = append(out[topic], u)
			}
		}
	}
	return out
}
