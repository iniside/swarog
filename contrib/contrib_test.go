package contrib

import "testing"

// TestContributions checks a slot collects values in registration order and an
// unknown slot returns nothing.
func TestContributions(t *testing.T) {
	s := New()
	s.Contribute("s", "a")
	s.Contribute("s", "b")
	s.Contribute("other", "x")

	got := s.Contributions("s")
	if len(got) != 2 || got[0] != "a" || got[1] != "b" {
		t.Fatalf("contributions out of order or wrong: %v", got)
	}
	if len(s.Contributions("missing")) != 0 {
		t.Fatalf("empty slot should return nothing, got %v", s.Contributions("missing"))
	}
}
