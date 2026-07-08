// White-box unit tests for the pure helpers in admin.go: slugify, items (slug
// dedupe), and buildGroups (first-seen section ordering + active marking).
// No database or network access required — all pure in-memory.
package admin

import (
	"context"
	"log/slog"
	"testing"

	"gamebackend/api/admin/adminapi"
	"gamebackend/lifecycle"
)

// ---- slugify ----------------------------------------------------------------

func TestSlugify(t *testing.T) {
	// Expected values are derived directly from the implementation:
	//   - lowercase the input
	//   - keep [a-z0-9]
	//   - map space / '-' / '_' → '-'
	//   - drop all other runes
	//   - trim leading/trailing '-'
	tests := []struct {
		in   string
		want string
	}{
		// Basic multi-word phrase: space → '-'.
		{"Game Content", "game-content"},
		// Single word: no transformation beyond lowercase.
		{"Players", "players"},
		// Only spaces → "--" → trimmed to "".
		{"  ", ""},
		// Mixed separators and symbols: '/' and '&' are dropped; the two
		// surrounding spaces each become '-', leaving "ab--c" with no leading
		// or trailing dash.
		{"A/B & C", "ab--c"},
		// All-symbol input: every rune dropped → empty.
		{"!@#$%^&*()", ""},
		// Underscores map to '-'; leading/trailing '-' are trimmed.
		{"_leading_", "leading"},
		// Existing dashes are kept as-is.
		{"hello-world", "hello-world"},
		// Mixed case with underscore separator.
		{"Hello_World", "hello-world"},
		// Digits are preserved.
		{"Zone42", "zone42"},
	}
	for _, tc := range tests {
		t.Run(tc.in, func(t *testing.T) {
			got := slugify(tc.in)
			if got != tc.want {
				t.Errorf("slugify(%q) = %q; want %q", tc.in, got, tc.want)
			}
		})
	}
}

// ---- items() slug dedupe ----------------------------------------------------

// TestItems_SlugDedupe checks that items() produces unique, deterministic slugs
// in registration order, with collision disambiguation (-2, -3, …) and the
// empty-slug fallback to "item".
func TestItems_SlugDedupe(t *testing.T) {
	ctx := lifecycle.NewContext(slog.Default())

	// Duplicate label → first keeps base slug, second gets "-2".
	ctx.Contribute(adminapi.Slot, adminapi.Item{Section: "S", Label: "Players"})
	ctx.Contribute(adminapi.Slot, adminapi.Item{Section: "S", Label: "Players"})
	// All-symbol label → slugify("!@#") == "" → base falls back to "item".
	ctx.Contribute(adminapi.Slot, adminapi.Item{Section: "S", Label: "!@#"})
	// Normal item at the end to verify order is preserved.
	ctx.Contribute(adminapi.Slot, adminapi.Item{Section: "S", Label: "Leaderboard"})

	m := &Module{ctx: ctx}
	items := m.items(context.Background())

	if len(items) != 4 {
		t.Fatalf("items() len = %d; want 4", len(items))
	}

	wantSlugs := []string{"players", "players-2", "item", "leaderboard"}
	for i, ri := range items {
		if ri.slug != wantSlugs[i] {
			t.Errorf("items()[%d].slug = %q; want %q", i, ri.slug, wantSlugs[i])
		}
	}

	// All slugs must be globally unique.
	seen := map[string]bool{}
	for _, ri := range items {
		if seen[ri.slug] {
			t.Errorf("duplicate slug %q returned by items()", ri.slug)
		}
		seen[ri.slug] = true
	}
}

// TestItems_SkipsNonItemContributions verifies that non-Item values contributed
// to the slot are silently ignored (the type-assertion guard in items()).
func TestItems_SkipsNonItemContributions(t *testing.T) {
	ctx := lifecycle.NewContext(slog.Default())
	ctx.Contribute(adminapi.Slot, "not an adminapi.Item")
	ctx.Contribute(adminapi.Slot, 42)
	ctx.Contribute(adminapi.Slot, adminapi.Item{Section: "S", Label: "Valid"})

	m := &Module{ctx: ctx}
	items := m.items(context.Background())

	if len(items) != 1 {
		t.Fatalf("items() len = %d; want 1 (non-Item contributions skipped)", len(items))
	}
	if items[0].slug != "valid" {
		t.Errorf("items()[0].slug = %q; want %q", items[0].slug, "valid")
	}
}

// TestItems_EmptySlot verifies that items() returns nil when nothing has been
// contributed.
func TestItems_EmptySlot(t *testing.T) {
	ctx := lifecycle.NewContext(slog.Default())
	m := &Module{ctx: ctx}
	if got := m.items(context.Background()); got != nil {
		t.Errorf("items() on empty slot = %v; want nil", got)
	}
}

// ---- buildGroups ------------------------------------------------------------

// TestBuildGroups_SectionOrderAndActiveMarking is the primary test: sections
// appear in first-seen order (not alphabetical), items within a section appear
// in registration order, and Active is true only on the item whose slug matches
// activeSlug.
func TestBuildGroups_SectionOrderAndActiveMarking(t *testing.T) {
	m := &Module{} // buildGroups uses no Module state

	// "B" is introduced first → must appear before "A" in the output even
	// though "A" sorts earlier alphabetically.
	input := []resolvedItem{
		{section: "B", label: "X", slug: "x"},
		{section: "A", label: "Y", slug: "y"},
		{section: "B", label: "Z", slug: "z"},
	}

	groups := m.buildGroups(input, "y")

	if len(groups) != 2 {
		t.Fatalf("buildGroups len = %d; want 2", len(groups))
	}

	// Group 0 = "B" (first seen), contains X then Z.
	if groups[0].Section != "B" {
		t.Errorf("groups[0].Section = %q; want %q", groups[0].Section, "B")
	}
	if len(groups[0].Items) != 2 {
		t.Fatalf("groups[0].Items len = %d; want 2", len(groups[0].Items))
	}
	checkNavItem(t, "groups[0].Items[0]", groups[0].Items[0], "X", "x", false)
	checkNavItem(t, "groups[0].Items[1]", groups[0].Items[1], "Z", "z", false)

	// Group 1 = "A", contains Y which is the active item.
	if groups[1].Section != "A" {
		t.Errorf("groups[1].Section = %q; want %q", groups[1].Section, "A")
	}
	if len(groups[1].Items) != 1 {
		t.Fatalf("groups[1].Items len = %d; want 1", len(groups[1].Items))
	}
	checkNavItem(t, "groups[1].Items[0]", groups[1].Items[0], "Y", "y", true)
}

// TestBuildGroups_NoActiveSlug verifies that when activeSlug is empty no item
// is marked active.
func TestBuildGroups_NoActiveSlug(t *testing.T) {
	m := &Module{}
	input := []resolvedItem{
		{section: "S", label: "Alpha", slug: "alpha"},
		{section: "S", label: "Beta", slug: "beta"},
	}

	groups := m.buildGroups(input, "")

	for _, g := range groups {
		for _, ni := range g.Items {
			if ni.Active {
				t.Errorf("item %q.Active = true; want false when activeSlug is empty", ni.Slug)
			}
		}
	}
}

// TestBuildGroups_EmptyInput verifies the nil-input edge case.
func TestBuildGroups_EmptyInput(t *testing.T) {
	m := &Module{}
	groups := m.buildGroups(nil, "anything")
	if groups != nil {
		t.Errorf("buildGroups(nil, ...) = %v; want nil", groups)
	}
}

// TestBuildGroups_SingleSection verifies all items land in one group when they
// share a section.
func TestBuildGroups_SingleSection(t *testing.T) {
	m := &Module{}
	input := []resolvedItem{
		{section: "Only", label: "One", slug: "one"},
		{section: "Only", label: "Two", slug: "two"},
		{section: "Only", label: "Three", slug: "three"},
	}

	groups := m.buildGroups(input, "two")

	if len(groups) != 1 {
		t.Fatalf("buildGroups len = %d; want 1", len(groups))
	}
	if groups[0].Section != "Only" {
		t.Errorf("groups[0].Section = %q; want %q", groups[0].Section, "Only")
	}
	if len(groups[0].Items) != 3 {
		t.Fatalf("groups[0].Items len = %d; want 3", len(groups[0].Items))
	}
	checkNavItem(t, "Items[0]", groups[0].Items[0], "One", "one", false)
	checkNavItem(t, "Items[1]", groups[0].Items[1], "Two", "two", true)
	checkNavItem(t, "Items[2]", groups[0].Items[2], "Three", "three", false)
}

// checkNavItem is a t.Helper that asserts all three fields of a navItem.
func checkNavItem(t *testing.T, name string, ni navItem, wantLabel, wantSlug string, wantActive bool) {
	t.Helper()
	if ni.Label != wantLabel {
		t.Errorf("%s.Label = %q; want %q", name, ni.Label, wantLabel)
	}
	if ni.Slug != wantSlug {
		t.Errorf("%s.Slug = %q; want %q", name, ni.Slug, wantSlug)
	}
	if ni.Active != wantActive {
		t.Errorf("%s.Active = %v; want %v", name, ni.Active, wantActive)
	}
}
