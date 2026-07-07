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

// Set validates the ids, then in ONE transaction upserts the row AND fires
// pg_notify('config_changed', "ns:key"). The tx makes the two atomic — two
// Execs on the pool could commit the write yet never notify. Set does NOT touch
// the cache: the listener is the single refresh path (decision #8), so a local
// write and an external psql edit are handled identically.
func (s *service) Set(ctx context.Context, ns, key, value string) error {
	if !identRe.MatchString(ns) {
		return fmt.Errorf("config: invalid namespace %q (must match %s)", ns, identRe.String())
	}
	if !identRe.MatchString(key) {
		return fmt.Errorf("config: invalid key %q (must match %s)", key, identRe.String())
	}

	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer func() { _ = tx.Rollback() }() // no-op after a successful Commit

	if err := upsert(ctx, tx, ns, key, value); err != nil {
		return err
	}
	if _, err := tx.ExecContext(ctx, `SELECT pg_notify('config_changed', $1)`, ns+":"+key); err != nil {
		return err
	}
	return tx.Commit()
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

// replaceCache swaps in a fresh cache from a full load (listener (re)connect).
func (s *service) replaceCache(settings []setting) {
	m := make(map[cacheKey]string, len(settings))
	for _, st := range settings {
		m[cacheKey{st.Namespace, st.Key}] = st.Value
	}
	s.mu.Lock()
	s.cache = m
	s.mu.Unlock()
}

// setCacheOne updates a single cached key (listener applying one notification).
func (s *service) setCacheOne(ns, key, value string) {
	s.mu.Lock()
	s.cache[cacheKey{ns, key}] = value
	s.mu.Unlock()
}
