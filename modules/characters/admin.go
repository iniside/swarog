package characters

import (
	"context"
	"strconv"

	"gamebackend/modules/admin/adminapi"
)

// adminItemID/adminSectionName/adminLabel identify this module's admin surface —
// shared by the contributed Item and the adminData edge operation's ItemData reply
// so a remote admin fetches the same Section/Label the local closure carries.
const (
	adminItemID      = "characters"
	adminSectionName = "Game Content"
	adminLabel       = "Characters"
)

// AdminData is the characters module's adminData edge operation (charactersapi.Admin):
// it returns this module's admin content as adminapi.ItemData so a peer's admin
// portal can render it over the unified QUIC edge. It runs the SAME adminSection
// logic the in-process closure uses. No player identity is involved.
func (m *Module) AdminData(ctx context.Context) (adminapi.ItemData, error) {
	content, err := m.adminSection(ctx)
	if err != nil {
		return adminapi.ItemData{}, err
	}
	return adminapi.ItemData{
		ID: adminItemID, Section: adminSectionName, Label: adminLabel, Content: content,
	}, nil
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
