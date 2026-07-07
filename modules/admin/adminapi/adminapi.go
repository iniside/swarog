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
// items by Section into the menu; opening an item renders its Content into the content area.
//
// An item is either LOCAL or REMOTE:
//   - LOCAL (co-located module): Render is set and the admin calls it in-process
//     (an opaque closure passed through the contribution slot — no cross-module import).
//   - REMOTE (a stub standing in for a module hosted in a peer process): RemoteURL
//     is set (the peer's .../admin-data/<ID> URL). Section/Label/Render are left
//     zero — the admin fetches ItemData over HTTP to learn them.
type Item struct {
	ID        string                                     // stable id, e.g. "characters"; the /admin-data/<id> path segment + remote-match key
	Section   string                                     // sidebar group label, e.g. "Game Content". First item creates it; rest append.
	Label     string                                     // the clickable menu entry + page title, e.g. "Characters"
	Render    func(ctx context.Context) (Content, error) // LOCAL: in-process render; nil for a remote stub item
	RemoteURL string                                     // REMOTE: the peer's .../admin-data/<id> URL; empty for local items
}

// ItemData is the wire form GET /admin-data/<id> returns as JSON. A remote admin
// process fetches it to learn a remote item's Section/Label AND its Content in a
// single round-trip, so the sidebar and page render from one fetch.
type ItemData struct {
	ID      string  `json:"id"`
	Section string  `json:"section"`
	Label   string  `json:"label"`
	Content Content `json:"content"`
}

// Content is what a section renders into: an optional KPI row and an optional
// table. The admin owns the look; the module only declares data.
type Content struct {
	KPIs  []KPI
	Table *Table
	Form  *Form // optional editable form; nil = today's read-only behavior
}

// Form is an editable widget a LOCAL item can attach to its Content: the admin
// renders Fields as text inputs and, on POST, invokes Submit in-process with the
// posted values. It is local-only — a remote item's Form arrives over the wire
// with Submit nil (a func can't marshal), so remote forms render read-only.
type Form struct {
	Action string                                                    // page slug this posts back to; the admin fills it in when rendering
	Fields []Field                                                   // inputs to render, in order
	Submit func(ctx context.Context, values map[string]string) error `json:"-"` // LOCAL-only: applies the edit; nil across the remote wire (a func can't marshal)
}

// Field is one input in a Form: a labelled text box pre-filled with Value, whose
// Name is both the HTML input name and the key in the Submit values map.
type Field struct {
	Name  string // form input name + Submit map key
	Label string // shown beside the input
	Value string // current value, pre-filled
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
