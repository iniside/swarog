// Package adminapi is the contract between the admin portal and the modules that
// appear in it. A module contributes an Item (into the core "admin.item" slot);
// the admin module renders a navigable sidebar grouping items by Section, with
// each item opening a dedicated content page. The admin never imports a module's
// implementation, and modules never import the admin — both depend only on this
// contract (like the <module>events packages).
package adminapi

import "context"

// Slot is the core contribution slot admin reads.
const Slot = "admin.item"

// Item is one clickable entry in the admin sidebar, contributed by a module. The admin groups
// items by Section into the menu; opening an item renders Render() into the content area.
type Item struct {
	Section string // sidebar group label, e.g. "Game Content". First item creates it; rest append.
	Label   string // the clickable menu entry + page title, e.g. "Characters"
	Render  func(ctx context.Context) (Content, error)
}

// Content is what a section renders into: an optional KPI row and an optional
// table. The admin owns the look; the module only declares data.
type Content struct {
	KPIs  []KPI
	Table *Table
}

type KPI struct {
	Label string
	Value string
	Sub   string // optional small subtitle, e.g. "linked"
}

type Table struct {
	Columns []string
	Rows    [][]Cell
}

// Cell is one table value. Badge (one of "green","amber","red","blue","grey")
// renders a status pill; Mono renders monospaced (IDs); otherwise plain text.
type Cell struct {
	Text  string
	Badge string
	Mono  bool
}
