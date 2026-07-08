package config

import (
	"context"
	"database/sql"
	"fmt"
	"log/slog"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"sync"
)

// identRe validates a namespace/key. Restricting ids to [a-z0-9_] makes the ':'
// separator unambiguous everywhere it appears — the pg_notify payload
// (namespace:key) and the admin form field Name (decision #7).
var identRe = regexp.MustCompile(`^[a-z0-9_]+$`)

// cacheKey is the composite (namespace, key) map key backing the read cache.
type cacheKey struct{ ns, key string }

// service is the "config" capability: a read-mostly in-memory cache of settings
// (kept fresh by the listener) plus a transactional Set. Readers Require it
// against their OWN 1-method interface (they need only the getter subset).
type service struct {
	db    *sql.DB
	log   *slog.Logger
	mu    sync.RWMutex
	cache map[cacheKey]string
}

// GetString returns the cached value, or def on a miss — the same degrade-to-
// default shape as the envOr(key, def) idiom this augments.
func (s *service) GetString(ns, key, def string) string {
	if v, ok := s.Get(ns, key); ok {
		return v
	}
	return def
}

// GetBool mirrors the repo's envBool truthiness ("1"/"true"/"on",
// case-insensitive); a miss returns def.
func (s *service) GetBool(ns, key string, def bool) bool {
	v, ok := s.Get(ns, key)
	if !ok {
		return def
	}
	return v == "1" || strings.EqualFold(v, "true") || strings.EqualFold(v, "on")
}

// GetInt parses the cached value as an int; a miss or a parse error returns def.
func (s *service) GetInt(ns, key string, def int) int {
	v, ok := s.Get(ns, key)
	if !ok {
		return def
	}
	n, err := strconv.Atoi(v)
	if err != nil {
		return def
	}
	return n
}

// Get is the raw comma-ok cache lookup.
func (s *service) Get(ns, key string) (string, bool) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	v, ok := s.cache[cacheKey{ns, key}]
	return v, ok
}

// Set validates the ids, then upserts the row in a single autocommit statement.
// The config.settings AFTER-write trigger fires pg_notify('config_changed',
// "ns:key") on the same statement (delivered on commit), so no explicit NOTIFY is
// needed and one statement is atomic on its own — no tx. Set does NOT touch the
// cache: the listener is the single refresh path (decision #8), so a local write
// and an external psql edit are handled identically.
func (s *service) Set(ctx context.Context, ns, key, value string) error {
	if !identRe.MatchString(ns) {
		return fmt.Errorf("config: invalid namespace %q (must match %s)", ns, identRe.String())
	}
	if !identRe.MatchString(key) {
		return fmt.Errorf("config: invalid key %q (must match %s)", key, identRe.String())
	}

	_, err := s.db.ExecContext(ctx,
		`INSERT INTO config.settings (namespace, key, value) VALUES ($1, $2, $3)
		 ON CONFLICT (namespace, key) DO UPDATE SET value = excluded.value, updated_at = now()`,
		ns, key, value)
	return err
}

// all snapshots the cache as a slice sorted by (namespace, key) for a stable
// admin render.
func (s *service) all() []setting {
	s.mu.RLock()
	out := make([]setting, 0, len(s.cache))
	for k, v := range s.cache {
		out = append(out, setting{Namespace: k.ns, Key: k.key, Value: v})
	}
	s.mu.RUnlock()

	sort.Slice(out, func(i, j int) bool {
		if out[i].Namespace != out[j].Namespace {
			return out[i].Namespace < out[j].Namespace
		}
		return out[i].Key < out[j].Key
	})
	return out
}

// replaceCache swaps in a fresh cache from a full load (listener (re)connect) and
// returns the settings whose value changed vs the prior snapshot — a new key or a
// changed value counts as changed; removed keys are ignored (deletes are out of
// scope, decision #8). The diff is computed under the write lock while swapping so
// it reflects exactly the snapshot installed. The listener emits config.changed
// for each returned setting after a RECONNECT so materialized push consumers heal.
func (s *service) replaceCache(settings []setting) []setting {
	m := make(map[cacheKey]string, len(settings))
	for _, st := range settings {
		m[cacheKey{st.Namespace, st.Key}] = st.Value
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	var changed []setting
	for _, st := range settings {
		if prev, ok := s.cache[cacheKey{st.Namespace, st.Key}]; !ok || prev != st.Value {
			changed = append(changed, st)
		}
	}
	s.cache = m
	return changed
}

// setCacheOne updates a single cached key (listener applying one notification).
func (s *service) setCacheOne(ns, key, value string) {
	s.mu.Lock()
	s.cache[cacheKey{ns, key}] = value
	s.mu.Unlock()
}
