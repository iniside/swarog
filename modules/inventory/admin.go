package inventory

import (
	"context"
	"strconv"
	"strings"

	"gamebackend/api/admin/adminapi"
)

// adminItemID/adminSectionName/adminLabel identify this module's admin surface.
// inventory is always co-hosted with the admin portal (LOCAL render closure), so
// it needs no adminData edge operation — only accounts/characters are fanned out.
const (
	adminItemID      = "inventory"
	adminSectionName = "Game Content"
	adminLabel       = "Inventory"

	adminOwnersLimit = 200
)

// adminSection renders one of two views off the SAME item (slug "inventory"),
// switched by the ?owner= drill-down param the admin shell carries via ctx:
//   - absent  → the owners list (each owner-id cell links to inventory?owner=<type>:<id>)
//   - present → that one owner's items (the sidebar "Inventory" link, no param, is "back")
func (m *Module) adminSection(ctx context.Context) (adminapi.Content, error) {
	if owner := adminapi.Params(ctx)["owner"]; owner != "" {
		return m.adminOwnerDetail(ctx, owner)
	}
	return m.adminOwnersList(ctx)
}

// adminOwnersList is the top-level view: KPIs plus one row per owner, the owner-id
// cell linking to that owner's items page.
func (m *Module) adminOwnersList(ctx context.Context) (adminapi.Content, error) {
	holdings, owners, err := m.store.stats(ctx)
	if err != nil {
		return adminapi.Content{}, err
	}
	rows, err := m.store.listOwners(ctx, adminOwnersLimit)
	if err != nil {
		return adminapi.Content{}, err
	}

	table := &adminapi.Table{Columns: []string{"OWNER", "OWNER ID", "ITEMS", "TOTAL QTY"}}
	for _, o := range rows {
		table.Rows = append(table.Rows, []adminapi.Cell{
			{Text: o.OwnerType, Badge: ownerBadge(o.OwnerType)},
			{Text: o.OwnerID, Mono: true, Link: adminItemID + "?owner=" + o.OwnerType + ":" + o.OwnerID},
			{Text: strconv.Itoa(o.Items)},
			{Text: strconv.Itoa(o.Qty)},
		})
	}

	return adminapi.Content{
		KPIs: []adminapi.KPI{
			{Label: "Holdings", Value: strconv.Itoa(holdings)},
			{Label: "Owners", Value: strconv.Itoa(owners), Sub: "players + characters"},
		},
		Table: table,
	}, nil
}

// adminOwnerDetail is the drill-down view for one owner ("<type>:<id>"): its
// items. A malformed owner param renders an error card (not a 500).
func (m *Module) adminOwnerDetail(ctx context.Context, owner string) (adminapi.Content, error) {
	otype, id, ok := strings.Cut(owner, ":")
	if !ok || (otype != "player" && otype != "character") {
		return errorContent("Invalid owner — expected player:<uuid> or character:<uuid>."), nil
	}
	if !isUUID(id) {
		return errorContent("Invalid owner id — not a uuid."), nil
	}

	holdings, err := m.store.list(ctx, Owner{Type: otype, ID: id})
	if err != nil {
		return adminapi.Content{}, err
	}

	table := &adminapi.Table{Columns: []string{"ITEM", "QTY"}}
	for _, h := range holdings {
		table.Rows = append(table.Rows, []adminapi.Cell{
			{Text: h.ItemName},
			{Text: strconv.Itoa(h.Quantity)},
		})
	}

	return adminapi.Content{
		KPIs: []adminapi.KPI{
			{Label: "Owner", Value: otype, Sub: ownerBadgeSub(otype)},
			{Label: "Owner ID", Value: id},
			{Label: "Items", Value: strconv.Itoa(len(holdings))},
		},
		Table: table,
	}, nil
}

// isUUID reports whether s is a canonical 8-4-4-4-12 hex uuid. It guards the
// drill-down param before it reaches the store's $id::uuid cast, so a malformed
// owner id renders an error card instead of a Postgres cast error (a 500). This
// avoids a uuid dependency; the DB remains the source of truth for real ids.
func isUUID(s string) bool {
	if len(s) != 36 {
		return false
	}
	for i, r := range s {
		if i == 8 || i == 13 || i == 18 || i == 23 {
			if r != '-' {
				return false
			}
			continue
		}
		isHex := (r >= '0' && r <= '9') || (r >= 'a' && r <= 'f') || (r >= 'A' && r <= 'F')
		if !isHex {
			return false
		}
	}
	return true
}

func ownerBadge(ownerType string) string {
	if ownerType == "character" {
		return "blue"
	}
	return "grey"
}

func ownerBadgeSub(ownerType string) string {
	if ownerType == "character" {
		return "character-scoped"
	}
	return "player-scoped"
}

// errorContent renders a single message as an error card (the admin shell shows a
// Content whose only signal is a KPI/text as a panel; we surface it via a KPI so
// the page is a clean card, never a 500).
func errorContent(msg string) adminapi.Content {
	return adminapi.Content{
		KPIs: []adminapi.KPI{{Label: "Error", Value: msg}},
	}
}
