// Package adminapi is the contract between the admin portal and the modules that
// appear in it. A module contributes an Item (into the core "admin.item" slot);
// the admin module renders a navigable sidebar grouping items by Section, with
// each item opening a dedicated content page. The admin never imports a module's
// implementation, and modules never import the admin — both depend only on this
// contract (like the <module>events packages).
package adminapi

import (
	"context"
	"errors"
)

// Slot is the core contribution slot admin reads.
const Slot = "admin.item"

// ErrItemAbsent signals that a remote item's provider has no admin surface: a
// RemoteFetch returns it (wrapping the edge's "unknown method" outcome) so the
// admin drops the item silently instead of showing an error card. It is the
// RemoteFetch analogue of the old HTTP 404-skip semantics.
var ErrItemAbsent = errors.New("adminapi: remote item has no admin surface")

// Item is one clickable entry in the admin sidebar, contributed by a module. The admin groups
// items by Section into the menu; opening an item renders its Content into the content area.
//
// An item is either LOCAL or REMOTE:
//   - LOCAL (co-located module): Render is set and the admin calls it in-process
//     (an opaque closure passed through the contribution slot — no cross-module import).
//   - REMOTE (a stub standing in for a module hosted in a peer process): RemoteFetch
//     is set — an in-process closure that hops the unified edge transport (the same
//     mTLS QUIC edge as characters.ownerOf) to fetch the peer's ItemData. Section/
//     Label/Render are left zero; the admin learns them from the fetched ItemData.
type Item struct {
	ID          string                                       // stable id, e.g. "characters"; the remote-match key
	Section     string                                       // sidebar group label, e.g. "Game Content". First item creates it; rest append.
	Label       string                                       // the clickable menu entry + page title, e.g. "Characters"
	Render      func(ctx context.Context) (Content, error)   // LOCAL: in-process render; nil for a remote stub item
	RemoteFetch func(ctx context.Context) (ItemData, error)  // REMOTE: fetches ItemData over the edge; nil for local items. ErrItemAbsent ⇒ skip.
}

// ItemData is the wire form the module's adminData edge operation returns. A
// remote admin process fetches it (over the QUIC edge, via the generated glue) to
// learn a remote item's Section/Label AND its Content in a single round-trip, so
// the sidebar and page render from one fetch.
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
