// Package registry is the synchronous service lookup: a module Provides a named
// service, another Requires it and asserts it to its OWN local interface (Go
// structural typing — see modules/match/match.go). It's a leaf: it imports only
// stdlib and is importable by everyone.
package registry

import "fmt"

// Registry maps a service name to its implementation — one service per name.
// (Contrast contrib.Slots, which collects MANY values under one name.)
type Registry struct {
	services map[string]any
}

func New() *Registry {
	return &Registry{services: map[string]any{}}
}

// Provide registers a named service so other modules can Require it. Panics on
// duplicate — that's a wiring bug, better loud at startup than silent later. T
// is inferred from svc; the value is stored boxed as any for the lookup.
func Provide[T any](r *Registry, name string, svc T) {
	if _, exists := r.services[name]; exists {
		panic(fmt.Sprintf("service %q already provided", name))
	}
	r.services[name] = any(svc)
}

// Require looks up a named service and asserts it to T — the consumer's OWN
// local interface. The comma-ok lookup comes FIRST so a missing service keeps
// its distinct "required service %q not found" message; a present-but-wrong-type
// service then fails its assertion with a separate message. Both are wiring
// bugs, loud at startup rather than a surprise later.
func Require[T any](r *Registry, name string) T {
	svc, ok := r.services[name]
	if !ok {
		panic(fmt.Sprintf("required service %q not found", name))
	}
	t, ok := svc.(T)
	if !ok {
		panic(fmt.Sprintf("service %q does not implement %T", name, *new(T)))
	}
	return t
}

// TryRequire is the comma-ok variant of Require: it returns (svc, true) when a
// service is registered under name AND is assignable to T, and (zero, false)
// otherwise (name absent, or present but not a T). Unlike Require it never
// panics — use it for an OPTIONAL dependency that a consumer can run without.
func TryRequire[T any](r *Registry, name string) (T, bool) {
	var zero T
	svc, ok := r.services[name]
	if !ok {
		return zero, false
	}
	t, ok := svc.(T)
	if !ok {
		return zero, false
	}
	return t, true
}
