package core

import "sync"

// Event is a topic plus payload. Topics are plain strings ("match.finished").
// Publishers don't know who listens; subscribers don't know who publishes.
type Event struct {
	Topic string
	Data  any
}

type Handler func(Event)

// Bus is a tiny in-process pub/sub — the default glue between modules:
// "I want to react" => Subscribe, never a direct call.
type Bus struct {
	mu   sync.RWMutex
	subs map[string][]Handler
}

func NewBus() *Bus { return &Bus{subs: map[string][]Handler{}} }

func (b *Bus) Subscribe(topic string, h Handler) {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.subs[topic] = append(b.subs[topic], h)
}

// Publish delivers synchronously to every subscriber. Simple and ordered; swap
// for goroutines/a queue later if a handler must not block the publisher.
func (b *Bus) Publish(e Event) {
	b.mu.RLock()
	handlers := b.subs[e.Topic]
	b.mu.RUnlock()
	for _, h := range handlers {
		h(e)
	}
}
