package characters

import (
	"context"
	"net/http"
	"strconv"

	"gamebackend/modules/admin/adminapi"
)

// adminItemID/adminSectionName/adminLabel identify this module's admin surface —
// shared by the contributed Item, the /admin-data endpoint, and its ItemData reply
// so a remote admin fetches the same Section/Label the local closure carries.
const (
	adminItemID      = "characters"
	adminSectionName = "Game Content"
	adminLabel       = "Characters"
)

// handleAdminData serves this module's admin content over HTTP as adminapi.ItemData
// so a remote admin process can render it. It runs the SAME adminSection logic the
// in-process closure uses.
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

// adminSection is the live "Characters" block this module contributes to the
// admin portal — reads only its own data, returns the admin's widget types.
func (m *Module) adminSection(ctx context.Context) (adminapi.Content, error) {
	n, err := m.store.count(ctx)
	if err != nil {
		return adminapi.Content{}, err
	}
	rows, err := m.store.listAll(ctx, 50)
	if err != nil {
		return adminapi.Content{}, err
	}

	table := &adminapi.Table{Columns: []string{"NAME", "CLASS", "PLAYER", "CREATED"}}
	for _, c := range rows {
		table.Rows = append(table.Rows, []adminapi.Cell{
			{Text: c.Name},
			{Text: c.Class, Badge: "blue"},
			{Text: c.PlayerID, Mono: true},
			{Text: c.CreatedAt.Format("Jan 2, 15:04")},
		})
	}

	return adminapi.Content{
		KPIs:  []adminapi.KPI{{Label: "Characters", Value: strconv.Itoa(n)}},
		Table: table,
	}, nil
}
