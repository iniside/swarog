// Package contrib is the multi-value slot registry. Unlike registry (one service
// per name), a slot collects MANY contributors — for cross-cutting collections
// like admin items, health checks or nav entries. A consumer reads them all via
// Contributions, so a new module lights up without the consumer being edited.
// It's a leaf: stdlib only, importable by everyone.
package contrib

// Slots holds every slot's contributions, each in registration order.
type Slots struct {
	m map[string][]any
}

func New() *Slots {
	return &Slots{m: map[string][]any{}}
}

// Contribute adds a value to a named slot.
func (s *Slots) Contribute(slot string, v any) {
	s.m[slot] = append(s.m[slot], v)
}

// Contributions returns everything contributed to a slot, in registration order.
// Read it lazily (e.g. per request), after all modules have initialized.
func (s *Slots) Contributions(slot string) []any {
	return s.m[slot]
}
