package registry

import "testing"

// greeter is a narrow consumer-defined interface, asserted against the concrete
// service the provider registers.
type greeter interface{ Greet() string }

type svc struct{ name string }

func (s *svc) Greet() string { return "hi " + s.name }

// TestProvideRequire covers the happy path: a concrete *svc Provided under a name
// resolves and asserts to the consumer's own greeter interface.
func TestProvideRequire(t *testing.T) {
	r := New()
	Provide(r, "svc", &svc{name: "x"})

	g := Require[greeter](r, "svc")
	if g.Greet() != "hi x" {
		t.Fatalf("Greet() = %q; want %q", g.Greet(), "hi x")
	}
}

func TestRequireNotFoundPanics(t *testing.T) {
	defer mustPanic(t, "required service")
	Require[greeter](New(), "absent")
}

func TestRequireWrongTypePanics(t *testing.T) {
	r := New()
	Provide(r, "svc", 42) // an int can't satisfy greeter
	defer mustPanic(t, "does not implement")
	Require[greeter](r, "svc")
}

func TestProvideDuplicatePanics(t *testing.T) {
	r := New()
	Provide(r, "svc", &svc{})
	defer mustPanic(t, "already provided")
	Provide(r, "svc", &svc{})
}

func mustPanic(t *testing.T, substr string) {
	t.Helper()
	r := recover()
	if r == nil {
		t.Fatalf("expected panic containing %q, got none", substr)
	}
	msg, _ := r.(string)
	if msg == "" {
		if e, ok := r.(error); ok {
			msg = e.Error()
		}
	}
	if !contains(msg, substr) {
		t.Fatalf("panic %q does not contain %q", msg, substr)
	}
}

func contains(s, sub string) bool {
	for i := 0; i+len(sub) <= len(s); i++ {
		if s[i:i+len(sub)] == sub {
			return true
		}
	}
	return false
}
