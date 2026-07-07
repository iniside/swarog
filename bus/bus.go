// Package bus is the asynchronous, fire-and-forget in-process pub/sub — the
// default glue between modules. It's a leaf: it imports only stdlib and is
// importable by everyone (a foundation).
package bus

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"log/slog"
	"sync"
)

// Event is a topic plus payload. Topics are plain strings ("match.finished").
// Publishers don't know who listens; subscribers don't know who publishes.
type Event struct {
	Topic string
	Data  any
}

type Handler func(Event)

// Bus is an asynchronous, fire-and-forget in-process pub/sub — the default glue
// between modules. Publish never blocks and never returns a result: if you need
// a synchronous answer, that's a direct interface call, not an event.
//
// Each subscriber gets its own goroutine and its own FIFO mailbox, so:
//   - delivery to a single subscriber preserves publish order,
//   - a slow subscriber can't stall the publisher or other subscribers,
//   - a panicking handler is contained, never killing anyone else.
//
// State built from events is therefore eventually consistent — a read right
// after a Publish may not see its effect yet. Anything needing immediate
// consistency must go through a service interface instead.
type Bus struct {
	mu        sync.RWMutex
	subs      map[string][]*mailbox
	boxes     []*mailbox // every mailbox, for draining on Close
	wg        sync.WaitGroup
	log       *slog.Logger
	transport Transport // nil-able durable-plane hook; see SetTransport
}

func NewBus(log *slog.Logger) *Bus {
	return &Bus{subs: map[string][]*mailbox{}, log: log}
}

func (b *Bus) Subscribe(topic string, h Handler) {
	box := newMailbox()

	b.mu.Lock()
	b.subs[topic] = append(b.subs[topic], box)
	b.boxes = append(b.boxes, box)
	b.mu.Unlock()

	b.wg.Go(func() {
		for {
			e, ok := box.pop() // blocks until an event arrives or the box is closed+drained
			if !ok {
				return
			}
			b.deliver(h, e)
		}
	})
}

// EventType binds a topic to its payload type T in ONE place. Publishers and
// subscribers reference the same EventType value, so they cannot disagree on
// topic-vs-payload: a mismatch is a compile error, not a runtime panic. This is
// the most compile-time safety achievable over an untyped (any-carrying) bus.
type EventType[T any] struct{ topic string }

// Define declares an event: a topic plus the payload type it always carries.
// Call it once, at package level, in the owning <module>events package.
func Define[T any](topic string) EventType[T] { return EventType[T]{topic: topic} }

func (e EventType[T]) Topic() string { return e.topic }

// On subscribes a typed handler. The handler signature is checked at compile
// time against the EventType's T. The internal assertion can't fail, because
// every value on this topic was put there by Emit with the same T.
func On[T any](b *Bus, et EventType[T], h func(T)) {
	b.Subscribe(et.topic, func(e Event) {
		v, ok := e.Data.(T)
		if !ok {
			b.log.Error("event payload type mismatch", "topic", e.Topic)
			return
		}
		h(v)
	})
}

// Emit publishes a typed event. Non-blocking, like Publish.
func Emit[T any](b *Bus, et EventType[T], v T) {
	b.Publish(Event{Topic: et.topic, Data: v})
}

// ErrNoTransport is returned by EmitTx when no durable transport is installed,
// so a durable event is never silently dropped — a caller that meant to persist
// an event learns that this process has no durable plane.
var ErrNoTransport = errors.New("bus: no durable transport installed")

// Transport is the durable plane's hook: a nil-able seam this leaf package
// defines but never implements. modules/messaging implements it (outbox log +
// inbox dedup + relay) and installs it via SetTransport, so the dependency
// points module → leaf and bus stays free of any module import (hard constraint
// #1). It deals only in topic strings and []byte — the generic payload T is
// collapsed to bytes at the EmitTx/OnTx/OnTxRaw boundary, so the transport never
// sees a type parameter.
type Transport interface {
	// EnqueueTx writes the encoded event to the durable log inside the caller's
	// transaction, so persisting the event is atomic with the domain change.
	EnqueueTx(tx *sql.Tx, topic string, payload []byte) error
	// SubscribeTx registers a durable handler for topic. subscriber is a stable
	// name identifying this subscription for inbox dedup ((event_id, subscriber)).
	SubscribeTx(topic, subscriber string, h func(ctx context.Context, tx *sql.Tx, payload []byte) error)
}

// SetTransport installs the durable transport. It panics on a double-set, so a
// second installer is a loud programmer error rather than a silent override
// (mirroring registry.Provide's duplicate-provide panic).
func (b *Bus) SetTransport(t Transport) {
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.transport != nil {
		panic("bus: transport already set")
	}
	b.transport = t
}

// EmitTx publishes a typed event on the durable plane, inside the caller's
// transaction. Unlike Emit it returns an error: ErrNoTransport if no durable
// transport is installed (so the event is never silently lost), or the marshal /
// enqueue error otherwise. The generic payload is marshalled to JSON here — the
// exact point where T collapses to bytes for the transport.
func EmitTx[T any](b *Bus, tx *sql.Tx, et EventType[T], v T) error {
	if b.transport == nil {
		return ErrNoTransport
	}
	payload, err := json.Marshal(v)
	if err != nil {
		return err
	}
	return b.transport.EnqueueTx(tx, et.topic, payload)
}

// OnTx subscribes a typed durable handler. subscriber is the stable dedup name.
// It is a no-op when no transport is installed, so a best-effort-only process
// (one that hosts no durable plane) is legal and simply skips durable wiring.
// The closure is the boundary where the transport's bytes are unmarshalled back
// into T before the typed handler runs.
func OnTx[T any](b *Bus, et EventType[T], subscriber string, h func(context.Context, *sql.Tx, T) error) {
	if b.transport == nil {
		return
	}
	b.transport.SubscribeTx(et.topic, subscriber, func(ctx context.Context, tx *sql.Tx, payload []byte) error {
		var v T
		if err := json.Unmarshal(payload, &v); err != nil {
			return err
		}
		return h(ctx, tx, v)
	})
}

// OnTxRaw is the untyped durable subscribe: it hands the handler the raw JSON
// payload, for a subscriber that reacts to a topic string without importing the
// producer's <module>events package (audit's cross-domain ledger). Like OnTx it
// is a no-op when no transport is installed.
func OnTxRaw(b *Bus, topic, subscriber string, h func(context.Context, *sql.Tx, json.RawMessage) error) {
	if b.transport == nil {
		return
	}
	b.transport.SubscribeTx(topic, subscriber, func(ctx context.Context, tx *sql.Tx, payload []byte) error {
		return h(ctx, tx, json.RawMessage(payload))
	})
}

// Publish hands the event to each subscriber's mailbox and returns immediately.
// Prefer the typed Emit; Publish is the lower-level primitive On/Emit build on.
func (b *Bus) Publish(e Event) {
	b.mu.RLock()
	boxes := b.subs[e.Topic]
	b.mu.RUnlock()
	for _, box := range boxes {
		box.push(e)
	}
}

// Close stops every subscriber once its mailbox has drained, then waits for all
// handler goroutines to finish. Call after the HTTP server has stopped so no new
// events arrive mid-drain.
func (b *Bus) Close() {
	b.mu.RLock()
	boxes := b.boxes
	b.mu.RUnlock()
	for _, box := range boxes {
		box.close()
	}
	b.wg.Wait()
}

func (b *Bus) deliver(h Handler, e Event) {
	defer func() {
		if r := recover(); r != nil {
			b.log.Error("event handler panicked", "topic", e.Topic, "panic", r)
		}
	}()
	h(e)
}

// mailbox is an unbounded, ordered, blocking-on-empty queue feeding one
// subscriber goroutine. Unbounded is a deliberate baseline choice: lossless and
// ordered, at the cost of memory if a producer permanently outruns a consumer.
// Swap for a bounded queue with a drop/backpressure policy if that ever bites.
type mailbox struct {
	mu     sync.Mutex
	cond   *sync.Cond
	queue  []Event
	closed bool
}

func newMailbox() *mailbox {
	m := &mailbox{}
	m.cond = sync.NewCond(&m.mu)
	return m
}

func (m *mailbox) push(e Event) {
	m.mu.Lock()
	if m.closed {
		m.mu.Unlock()
		return
	}
	m.queue = append(m.queue, e)
	m.mu.Unlock()
	m.cond.Signal()
}

func (m *mailbox) close() {
	m.mu.Lock()
	m.closed = true
	m.mu.Unlock()
	m.cond.Signal()
}

// pop returns the next event, blocking while empty. It returns ok=false only
// once the mailbox is closed AND fully drained, which ends the subscriber loop.
func (m *mailbox) pop() (Event, bool) {
	m.mu.Lock()
	defer m.mu.Unlock()
	for len(m.queue) == 0 && !m.closed {
		m.cond.Wait()
	}
	if len(m.queue) == 0 { // closed and drained
		return Event{}, false
	}
	e := m.queue[0]
	m.queue = m.queue[1:]
	return e, true
}
