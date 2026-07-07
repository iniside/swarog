package inventory

import (
	"context"
	"strconv"

	"gamebackend/modules/admin/adminapi"
)

// adminItemID/adminSectionName/adminLabel identify this module's admin surface.
// inventory is always co-hosted with the admin portal (LOCAL render closure), so
// it needs no adminData edge operation — only accounts/characters are fanned out.
const (
	adminItemID      = "inventory"
	adminSectionName = "Game Content"
	adminLabel       = "Inventory"
)

func (m *Module) adminSection(ctx context.Context) (adminapi.Content, error) {
	holdings, owners, err := m.store.stats(ctx)
	if err != nil {
		return adminapi.Content{}, err
	}
	rows, err := m.store.listAll(ctx, 50)
	if err != nil {
		return adminapi.Content{}, err
	}

	table := &adminapi.Table{Columns: []string{"OWNER", "OWNER ID", "ITEM", "QTY"}}
	for _, h := range rows {
		badge := "grey"
		if h.OwnerType == "character" {
			badge = "blue"
		}
		table.Rows = append(table.Rows, []adminapi.Cell{
			{Text: h.OwnerType, Badge: badge},
			{Text: h.OwnerID, Mono: true},
			{Text: h.ItemName},
			{Text: strconv.Itoa(h.Quantity)},
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
