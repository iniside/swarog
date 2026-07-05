package accounts

import (
	"context"
	"net/http"
	"strconv"
	"strings"

	"gamebackend/modules/admin/adminapi"
)

// adminItemID/adminSectionName/adminLabel identify this module's admin surface —
// shared by the contributed Item, the /admin-data endpoint, and its ItemData reply.
const (
	adminItemID      = "accounts"
	adminSectionName = "Identity"
	adminLabel       = "Players"
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

// adminSection is the live "Players" block this module contributes to the admin
// portal. It reads only its own data and returns the admin's declarative widget
// types — the admin never touches the accounts schema.
func (m *Module) adminSection(ctx context.Context) (adminapi.Content, error) {
	players, identities, sessions, err := m.store.stats(ctx)
	if err != nil {
		return adminapi.Content{}, err
	}
	rows, err := m.store.listPlayers(ctx, 50)
	if err != nil {
		return adminapi.Content{}, err
	}

	table := &adminapi.Table{Columns: []string{"PLAYER", "PLAYER ID", "PROVIDERS", "STATUS", "CREATED"}}
	for _, p := range rows {
		status := adminapi.Cell{Text: "Offline", Badge: "grey"}
		if p.Online {
			status = adminapi.Cell{Text: "Online", Badge: "green"}
		}
		table.Rows = append(table.Rows, []adminapi.Cell{
			{Text: p.DisplayName},
			{Text: p.ID, Mono: true},
			{Text: orDash(strings.Join(p.Providers, ", "))},
			status,
			{Text: p.CreatedAt.Format("Jan 2, 15:04")},
		})
	}

	return adminapi.Content{
		KPIs: []adminapi.KPI{
			{Label: "Players", Value: strconv.Itoa(players)},
			{Label: "Identities", Value: strconv.Itoa(identities), Sub: "linked credentials"},
			{Label: "Active sessions", Value: strconv.Itoa(sessions)},
		},
		Table: table,
	}, nil
}

func orDash(s string) string {
	if s == "" {
		return "—"
	}
	return s
}
