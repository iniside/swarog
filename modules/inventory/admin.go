package inventory

import (
	"context"
	"net/http"
	"strconv"

	"gamebackend/modules/admin/adminapi"
)

// adminItemID/adminSectionName/adminLabel identify this module's admin surface —
// shared by the contributed Item, the /admin-data endpoint, and its ItemData reply.
const (
	adminItemID      = "inventory"
	adminSectionName = "Game Content"
	adminLabel       = "Inventory"
)

// handleAdminData serves this module's admin content over HTTP as adminapi.ItemData
// so a remote admin process can render it, using the SAME adminSection logic.
func (m *Module) handleAdminData(w http.ResponseWriter, r *http.Request) {
	content, err := m.adminSection(r.Context())
	if err != nil {
		m.log.Error("admin-data render failed", "err", err)
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	writeJSON(w, http.StatusOK, adminapi.ItemData{
		ID: adminItemID, Section: adminSectionName, Label: adminLabel, Content: content,
	})
}

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
