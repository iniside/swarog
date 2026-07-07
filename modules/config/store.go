package config

import (
	"context"
	"database/sql"
	"errors"
)

// schemaDDL creates this module's own schema and nothing else — full logical
// isolation (constraint #10). Idempotent (CREATE ... IF NOT EXISTS).
const schemaDDL = `
CREATE SCHEMA IF NOT EXISTS config;
CREATE TABLE IF NOT EXISTS config.settings (
	namespace  text NOT NULL,
	key        text NOT NULL,
	value      text NOT NULL,
	updated_at timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (namespace, key)
);`

// setting is one persisted config row (updated_at is intentionally not carried —
// the getters and admin render only need the value).
type setting struct {
	Namespace string
	Key       string
	Value     string
}

// loadAll reads every setting — the full-reload source used on each (re)connect
// (decision #8) and by tests. Ordering is irrelevant: the cache is a map.
func (s *service) loadAll(ctx context.Context) ([]setting, error) {
	rows, err := s.db.QueryContext(ctx, `SELECT namespace, key, value FROM config.settings`)
	if err != nil {
		return nil, err
	}
	defer func() { _ = rows.Close() }()

	var out []setting
	for rows.Next() {
		var st setting
		if err := rows.Scan(&st.Namespace, &st.Key, &st.Value); err != nil {
			return nil, err
		}
		out = append(out, st)
	}
	return out, rows.Err()
}

// getOne fetches a single setting's value. ok=false with a nil error means the
// row is absent (a deleted key); a real DB error is returned as err.
func (s *service) getOne(ctx context.Context, ns, key string) (string, bool, error) {
	var v string
	err := s.db.QueryRowContext(ctx,
		`SELECT value FROM config.settings WHERE namespace = $1 AND key = $2`, ns, key).Scan(&v)
	if errors.Is(err, sql.ErrNoRows) {
		return "", false, nil
	}
	if err != nil {
		return "", false, err
	}
	return v, true, nil
}

// upsert writes one setting inside the caller's tx (so it commits atomically with
// the pg_notify that follows it in Set — decision #8).
func upsert(ctx context.Context, tx *sql.Tx, ns, key, value string) error {
	_, err := tx.ExecContext(ctx,
		`INSERT INTO config.settings (namespace, key, value) VALUES ($1, $2, $3)
		 ON CONFLICT (namespace, key) DO UPDATE SET value = excluded.value, updated_at = now()`,
		ns, key, value)
	return err
}
