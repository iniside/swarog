package accounts

import (
	"context"
	"strconv"
	"strings"

	"gamebackend/api/admin/adminapi"
)

// adminItemID/adminSectionName/adminLabel identify this module's admin surface —
// shared by the contributed Item and the adminData edge operation's ItemData reply.
const (
	adminItemID      = "accounts"
	adminSectionName = "Identity"
	adminLabel       = "Players"
)

// AdminData is the accounts module's adminData edge operation (accountsapi.Admin):
// it returns this module's admin content as adminapi.ItemData so a peer's admin
// portal can render it over the unified QUIC edge, using the SAME adminSection
// logic. No player identity is involved.
func (m *Module) AdminData(ctx context.Context) (adminapi.ItemData, error) {
	content, err := m.adminSection(ctx)
	if err != nil {
		return adminapi.ItemData{}, err
	}
	return adminapi.ItemData{
		ID: adminItemID, Section: adminSectionName, Label: adminLabel, Content: content,
	}, nil
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
