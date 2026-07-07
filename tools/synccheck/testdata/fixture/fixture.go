// Package fixture is a synccheck test fixture: a minimal package the harness
// test can load via packages.Load, with nothing for the (not-yet-wired)
// detector to flag. Step 2 replaces/extends this with the real poll/clean/
// publisher/allowed fixtures.
package fixture

// Noop exists only so the package has a syntax tree to load.
func Noop() {}
