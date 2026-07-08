package registry_test

import (
	"fmt"
	"strings"
	"testing"

	"pgregory.net/rapid"

	"gamebackend/registry"
)

// distinctNames generates a slice of distinct, non-empty service names.
func distinctNames(t *rapid.T, minLen, maxLen int) []string {
	return rapid.SliceOfNDistinct(
		rapid.StringMatching(`[a-zA-Z][a-zA-Z0-9_]{0,12}`),
		minLen, maxLen, rapid.ID[string],
	).Draw(t, "names")
}

// TestPropProvideRequireRoundTrip_String provides N distinct names each with a
// distinct string value, then asserts Require returns exactly the value that
// was provided under each name.
func TestPropProvideRequireRoundTrip_String(t *testing.T) {
	rapid.Check(t, func(t *rapid.T) {
		names := distinctNames(t, 0, 10)
		values := rapid.SliceOfN(rapid.String(), len(names), len(names)).Draw(t, "values")

		r := registry.New()
		for i, name := range names {
			registry.Provide(r, name, values[i])
		}
		for i, name := range names {
			got := registry.Require[string](r, name)
			if got != values[i] {
				t.Fatalf("Require(%q) = %q, want %q", name, got, values[i])
			}
		}
	})
}

// TestPropProvideRequireRoundTrip_Int is the same round-trip property with T
// fixed to int, since rapid generics fix T per test (Provide/Require are
// generic functions, not the property itself).
func TestPropProvideRequireRoundTrip_Int(t *testing.T) {
	rapid.Check(t, func(t *rapid.T) {
		names := distinctNames(t, 0, 10)
		values := rapid.SliceOfN(rapid.Int(), len(names), len(names)).Draw(t, "values")

		r := registry.New()
		for i, name := range names {
			registry.Provide(r, name, values[i])
		}
		for i, name := range names {
			got := registry.Require[int](r, name)
			if got != values[i] {
				t.Fatalf("Require(%q) = %d, want %d", name, got, values[i])
			}
		}
	})
}

// TestPropRequireMissingPanics: Require on a name that was never Provided
// always panics with a message containing "required service" and the name.
func TestPropRequireMissingPanics(t *testing.T) {
	rapid.Check(t, func(t *rapid.T) {
		provided := distinctNames(t, 0, 5)
		absent := rapid.StringMatching(`[a-zA-Z][a-zA-Z0-9_]{0,12}`).
			Filter(func(s string) bool {
				for _, n := range provided {
					if n == s {
						return false
					}
				}
				return true
			}).Draw(t, "absent")

		r := registry.New()
		for _, name := range provided {
			registry.Provide(r, name, 0)
		}

		msg := requirePanicMessage(t, r, absent)
		if !strings.Contains(msg, "required service") || !strings.Contains(msg, absent) {
			t.Fatalf("panic message %q does not contain %q and %q", msg, "required service", absent)
		}
	})
}

// TestPropProvideDuplicatePanics: Providing the same name twice always panics
// with a message containing "already provided" and the name.
func TestPropProvideDuplicatePanics(t *testing.T) {
	rapid.Check(t, func(t *rapid.T) {
		name := rapid.StringMatching(`[a-zA-Z][a-zA-Z0-9_]{0,12}`).Draw(t, "name")

		r := registry.New()
		registry.Provide(r, name, 0)

		msg := provideDuplicatePanicMessage(t, r, name)
		if !strings.Contains(msg, "already provided") || !strings.Contains(msg, name) {
			t.Fatalf("panic message %q does not contain %q and %q", msg, "already provided", name)
		}
	})
}

func requirePanicMessage(t *rapid.T, r *registry.Registry, name string) (msg string) {
	t.Helper()
	defer func() {
		v := recover()
		if v == nil {
			t.Fatalf("Require(%q) did not panic", name)
		}
		msg = fmt.Sprint(v)
	}()
	registry.Require[int](r, name)
	return ""
}

func provideDuplicatePanicMessage(t *rapid.T, r *registry.Registry, name string) (msg string) {
	t.Helper()
	defer func() {
		v := recover()
		if v == nil {
			t.Fatalf("Provide(%q) did not panic on duplicate", name)
		}
		msg = fmt.Sprint(v)
	}()
	registry.Provide(r, name, 0)
	return ""
}
