// Package adminapi is the contract between the admin portal and the modules that
// appear in it. A module contributes a Section (into the core "admin.section"
// slot); the admin module renders it in the GameOps theme. The admin never
// imports a module's implementation, and modules never import the admin — both
// depend only on this contract (like the <module>events packages).
package adminapi

import "context"

// Slot is the core contribution slot admin reads.
const Slot = "admin.section"

// Section is one block on the dashboard, owned by a module. Render is called per
// request so the data is live.
type Section struct {
	Title  string
	Render func(ctx context.Context) (Content, error)
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
