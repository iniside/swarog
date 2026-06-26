package accounts

import (
	"context"
	"strconv"
	"strings"

	"gamebackend/modules/admin/adminapi"
)

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
