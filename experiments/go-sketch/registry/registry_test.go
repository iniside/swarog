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

// TestTryRequire covers the comma-ok variant: present+correct type, absent
// name, and present-but-wrong-type, none of which should panic.
func TestTryRequire(t *testing.T) {
	t.Run("present and correct type", func(t *testing.T) {
		r := New()
		Provide(r, "svc", &svc{name: "x"})

		g, ok := TryRequire[greeter](r, "svc")
		if !ok {
			t.Fatalf("ok = false, want true")
		}
		if g.Greet() != "hi x" {
			t.Fatalf("Greet() = %q; want %q", g.Greet(), "hi x")
		}
	})

	t.Run("absent name", func(t *testing.T) {
		g, ok := TryRequire[greeter](New(), "absent")
		if ok {
			t.Fatalf("ok = true, want false")
		}
		if g != nil {
			t.Fatalf("g = %v, want zero value (nil)", g)
		}
	})

	t.Run("present but wrong type", func(t *testing.T) {
		r := New()
		Provide(r, "svc", 42) // an int can't satisfy greeter

		g, ok := TryRequire[greeter](r, "svc")
		if ok {
			t.Fatalf("ok = true, want false")
		}
		if g != nil {
			t.Fatalf("g = %v, want zero value (nil)", g)
		}
	})
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
