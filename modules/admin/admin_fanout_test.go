// White-box tests for the admin fan-out: items() dispatches LOCAL items to their
// in-process closure and REMOTE items to their RemoteFetch closure (which, in a
// real split, hops the QUIC edge to the peer's adminData operation). These use
// plain closures (no Postgres, no real peer) to exercise the success,
// absent-skip, and transport-failure (error-card) paths.
package admin

import (
	"context"
	"errors"
	"log/slog"
	"testing"

	"gamebackend/lifecycle"
	"gamebackend/modules/admin/adminapi"
)

// newFanoutModule builds a Module wired with a context, ready for items() without
// any database.
func newFanoutModule() *Module {
	return &Module{
		ctx: lifecycle.NewContext(slog.Default()),
		log: slog.Default(),
	}
}

// remoteFetch returns a RemoteFetch closure that yields the given ItemData/err —
// the in-process stand-in for the generated adminData edge client.
func remoteFetch(data adminapi.ItemData, err error) func(context.Context) (adminapi.ItemData, error) {
	return func(context.Context) (adminapi.ItemData, error) { return data, err }
}

// TestItems_RemoteSuccess: a remote item is fetched and its Section/Label/Content
// come from the peer's ItemData, not from the (empty) contributed Item.
func TestItems_RemoteSuccess(t *testing.T) {
	m := newFanoutModule()
	m.ctx.Contribute(adminapi.Slot, adminapi.Item{ID: "characters", RemoteFetch: remoteFetch(adminapi.ItemData{
		ID:      "characters",
		Section: "Game Content",
		Label:   "Characters",
		Content: adminapi.Content{KPIs: []adminapi.KPI{{Label: "Characters", Value: "7"}}},
	}, nil)})

	items := m.items(context.Background())
	if len(items) != 1 {
		t.Fatalf("items() len = %d; want 1", len(items))
	}
	it := items[0]
	if it.section != "Game Content" || it.label != "Characters" {
		t.Errorf("remote item metadata = (%q,%q); want (Game Content, Characters)", it.section, it.label)
	}
	if it.slug != "characters" {
		t.Errorf("remote item slug = %q; want %q", it.slug, "characters")
	}
	if it.render != nil {
		t.Errorf("remote item render should be nil")
	}
	if it.remote == nil || it.remote.err != nil {
		t.Fatalf("remote item should carry a successful result; got %+v", it.remote)
	}
	if len(it.remote.content.KPIs) != 1 || it.remote.content.KPIs[0].Value != "7" {
		t.Errorf("remote content not carried through: %+v", it.remote.content)
	}
}

// TestItems_RemoteAbsentSkipped: ErrItemAbsent (the peer has no admin surface)
// means the item is dropped silently, not shown as an error.
func TestItems_RemoteAbsentSkipped(t *testing.T) {
	m := newFanoutModule()
	m.ctx.Contribute(adminapi.Slot, adminapi.Item{ID: "ghost",
		RemoteFetch: remoteFetch(adminapi.ItemData{}, adminapi.ErrItemAbsent)})
	// A local item alongside it to prove only the absent one is dropped.
	m.ctx.Contribute(adminapi.Slot, adminapi.Item{ID: "inventory", Section: "Game Content", Label: "Inventory",
		Render: func(context.Context) (adminapi.Content, error) { return adminapi.Content{}, nil }})

	items := m.items(context.Background())
	if len(items) != 1 {
		t.Fatalf("items() len = %d; want 1 (absent item skipped)", len(items))
	}
	if items[0].label != "Inventory" {
		t.Errorf("surviving item = %q; want Inventory", items[0].label)
	}
}

// TestItems_RemoteErrorCard: any non-absent fetch error keeps the item as an error
// card — visible in the sidebar, Label falls back to ID, remote.err set — so a down
// peer never blanks /admin.
func TestItems_RemoteErrorCard(t *testing.T) {
	m := newFanoutModule()
	m.ctx.Contribute(adminapi.Slot, adminapi.Item{ID: "characters",
		RemoteFetch: remoteFetch(adminapi.ItemData{}, errors.New("boom"))})

	items := m.items(context.Background())
	if len(items) != 1 {
		t.Fatalf("items() len = %d; want 1 (error card kept)", len(items))
	}
	it := items[0]
	if it.label != "characters" || it.section != "characters" {
		t.Errorf("error card metadata = (%q,%q); want ID fallback (characters, characters)", it.section, it.label)
	}
	if it.remote == nil || it.remote.err == nil {
		t.Fatalf("error card should carry a fetch error; got %+v", it.remote)
	}
}

// TestItems_LocalVsRemoteDispatch: a mix of a local closure item and a remote item
// resolves each on its own path in one items() call.
func TestItems_LocalVsRemoteDispatch(t *testing.T) {
	m := newFanoutModule()
	m.ctx.Contribute(adminapi.Slot, adminapi.Item{ID: "inventory", Section: "Game Content", Label: "Inventory",
		Render: func(context.Context) (adminapi.Content, error) { return adminapi.Content{}, nil }})
	m.ctx.Contribute(adminapi.Slot, adminapi.Item{ID: "characters",
		RemoteFetch: remoteFetch(adminapi.ItemData{ID: "characters", Section: "Game Content", Label: "Characters"}, nil)})

	items := m.items(context.Background())
	if len(items) != 2 {
		t.Fatalf("items() len = %d; want 2", len(items))
	}
	// First is local: render set, remote nil.
	if items[0].render == nil || items[0].remote != nil {
		t.Errorf("items[0] should be LOCAL (render set, remote nil); got render=%v remote=%v",
			items[0].render != nil, items[0].remote)
	}
	// Second is remote: render nil, remote set.
	if items[1].render != nil || items[1].remote == nil {
		t.Errorf("items[1] should be REMOTE (render nil, remote set); got render=%v remote=%v",
			items[1].render != nil, items[1].remote)
	}
}
