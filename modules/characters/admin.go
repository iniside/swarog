package characters

import (
	"context"
	"strconv"

	"gamebackend/modules/admin/adminapi"
)

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
