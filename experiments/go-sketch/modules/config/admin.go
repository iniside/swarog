package config

import (
	"context"
	"strconv"

	"gamebackend/api/admin/adminapi"
)

// adminRender is the config editor page: KPIs, a read-only table of the current
// settings, and an editable Form (one field per setting + an add-new triple).
// The admin module owns the POST route/auth/rendering; config only supplies the
// declarative widgets and the applyEdit closure.
func (m *Module) adminRender(_ context.Context) (adminapi.Content, error) {
	rows := m.svc.all()

	namespaces := map[string]struct{}{}
	table := &adminapi.Table{Columns: []string{"Namespace", "Key", "Value"}}
	fields := make([]adminapi.Field, 0, len(rows)+3)
	for _, r := range rows {
		namespaces[r.Namespace] = struct{}{}
		table.Rows = append(table.Rows, []adminapi.Cell{
			{Text: r.Namespace, Mono: true},
			{Text: r.Key, Mono: true},
			{Text: r.Value},
		})
		fields = append(fields, adminapi.Field{
			Name:  r.Namespace + ":" + r.Key,
			Label: r.Namespace + " / " + r.Key,
			Value: r.Value,
		})
	}
	// Add-new triple: config owns the "" -> insert semantics; the adminapi.Form
	// contract stays a generic name/value list.
	fields = append(fields,
		adminapi.Field{Name: "_new_namespace", Label: "New namespace"},
		adminapi.Field{Name: "_new_key", Label: "New key"},
		adminapi.Field{Name: "_new_value", Label: "New value"},
	)

	return adminapi.Content{
		KPIs: []adminapi.KPI{
			{Label: "Settings", Value: strconv.Itoa(len(rows))},
			{Label: "Namespaces", Value: strconv.Itoa(len(namespaces))},
		},
		Table: table,
		Form:  &adminapi.Form{Fields: fields, Submit: m.applyEdit},
	}, nil
}

// applyEdit diffs the posted values against the current cache and Sets ONLY the
// keys that actually changed (each Set is a NOTIFY + a config.changed; rewriting
// every row would emit a storm of false "changed" events — decision #8). It then
// inserts the add-new row if its triple is fully filled. Returns the first error.
func (m *Module) applyEdit(ctx context.Context, values map[string]string) error {
	var firstErr error
	setErr := func(err error) {
		if err != nil && firstErr == nil {
			firstErr = err
		}
	}

	for _, s := range m.svc.all() {
		if v, ok := values[s.Namespace+":"+s.Key]; ok && v != s.Value {
			setErr(m.svc.Set(ctx, s.Namespace, s.Key, v))
		}
	}

	ns, key, val := values["_new_namespace"], values["_new_key"], values["_new_value"]
	if ns != "" && key != "" && val != "" {
		setErr(m.svc.Set(ctx, ns, key, val)) // Set validates the ids
	}
	return firstErr
}
