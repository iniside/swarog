package config

import (
	"context"
	"database/sql"
	"errors"
)

// schemaDDL creates this module's own schema and nothing else — full logical
// isolation (constraint #10). Idempotent (CREATE ... IF NOT EXISTS / OR REPLACE).
//
// The AFTER INSERT OR UPDATE trigger is the single source of the
// config_changed NOTIFY: ANY writer on the shared Postgres — this service's Set,
// another service's Set, or a raw psql UPDATE — fires it, so every LISTENing
// process reloads. NOTIFY is fired on the same statement and delivered on commit.
// DELETE is deliberately not triggered: the listener has no delete path (getOne
// !found is a no-op), so live-deleting a key is out of scope. RETURN NULL is
// correct for an AFTER trigger (its return value is ignored). The payload
// "namespace:key" matches the listener's split-on-first-':' because ids are
// ^[a-z0-9_]+$. CREATE OR REPLACE TRIGGER (PG14+) keeps re-migration idempotent
// with no drop/create window.
const schemaDDL = `
CREATE SCHEMA IF NOT EXISTS config;
CREATE TABLE IF NOT EXISTS config.settings (
	namespace  text NOT NULL,
	key        text NOT NULL,
	value      text NOT NULL,
	updated_at timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (namespace, key)
);
CREATE OR REPLACE FUNCTION config.notify_changed() RETURNS trigger
	LANGUAGE plpgsql AS $$
BEGIN
	PERFORM pg_notify('config_changed', NEW.namespace || ':' || NEW.key);
	RETURN NULL;
END;
$$;
CREATE OR REPLACE TRIGGER settings_notify
	AFTER INSERT OR UPDATE ON config.settings
	FOR EACH ROW EXECUTE FUNCTION config.notify_changed();`

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
