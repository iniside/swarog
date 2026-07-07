package config

import (
	"context"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"

	"gamebackend/bus"
	"gamebackend/modules/config/configevents"
)

// listen keeps a dedicated pgx connection LISTENing for config_changed and
// refreshes the cache until ctx is cancelled. Raw pgx is used because
// database/sql cannot WaitForNotification. It never dies on a DB outage: each
// (re)connect goes through listenOnce, which backs off on failure.
func (m *Module) listen(ctx context.Context) {
	booted := false
	for ctx.Err() == nil {
		if m.listenOnce(ctx, booted) {
			booted = true // the first successful reload is the boot load; the rest reconnect
		}
	}
}

// reloadAndHeal does the FULL cache reload every (re)connect performs and, on a
// RECONNECT (not the initial boot load), emits config.changed for each key whose
// value differs from the prior snapshot. PG does not queue NOTIFY for a dead
// session, so a reconnect that only reloaded the pull cache would leave a
// materialized push consumer (e.g. inventory's starter spec) stale for any key
// changed during the disconnect — so we replay those changes as events. The boot
// load emits nothing (that would spam one event per key at startup; pull
// consumers lazy-load anyway). This is the single path the listener uses; the
// reconnect flag is the only difference, which is why a test can drive healing by
// calling it with reconnect=true rather than copying the logic.
func (m *Module) reloadAndHeal(ctx context.Context, reconnect bool) error {
	settings, err := m.svc.loadAll(ctx)
	if err != nil {
		return err
	}
	changed := m.svc.replaceCache(settings)
	if reconnect {
		for _, st := range changed {
			bus.Emit(m.bus, configevents.ChangedEvent,
				configevents.Changed{Namespace: st.Namespace, Key: st.Key, Value: st.Value})
		}
	}
	return nil
}

// listenOnce owns exactly one connection for its lifetime: it connects, LISTENs,
// does a FULL cache reload (PG does not queue NOTIFY for a dead session, so a
// reconnect without a full reload would leave gap-changed keys stale forever —
// decision #8), then blocks on notifications until an error or cancellation. The
// connection is always closed on return; the outer loop reconnects. It returns
// true iff it got as far as installing a fresh cache (a successful reload), so
// the caller knows the boot load is done and subsequent reloads should heal.
// booted is passed to reloadAndHeal as its reconnect flag.
func (m *Module) listenOnce(ctx context.Context, booted bool) bool {
	conn, err := pgx.Connect(ctx, m.dsn)
	if err != nil {
		if ctx.Err() == nil {
			m.log.Error("config listener connect failed", "err", err)
		}
		m.backoff(ctx)
		return false
	}
	// Close with a fresh context: during shutdown the loop's ctx is already
	// cancelled, and a cancelled ctx would abort the close.
	//nolint:contextcheck // intentional: close must not use the (possibly-cancelled) loop ctx.
	defer func() { _ = conn.Close(context.Background()) }()

	if _, err := conn.Exec(ctx, "LISTEN config_changed"); err != nil {
		if ctx.Err() == nil {
			m.log.Error("config listener LISTEN failed", "err", err)
		}
		m.backoff(ctx)
		return false
	}

	if err := m.reloadAndHeal(ctx, booted); err != nil {
		if ctx.Err() == nil {
			m.log.Error("config listener reload failed", "err", err)
		}
		m.backoff(ctx)
		return false
	}

	for {
		n, err := conn.WaitForNotification(ctx)
		if ctx.Err() != nil {
			return true // clean shutdown (reload already succeeded)
		}
		if err != nil {
			m.log.Error("config listener wait failed", "err", err)
			m.backoff(ctx)
			return true // reconnect via the outer loop (conn closed by defer)
		}

		ns, key, ok := strings.Cut(n.Payload, ":")
		if !ok {
			m.log.Warn("config listener ignoring malformed payload", "payload", n.Payload)
			continue
		}
		v, found, err := m.svc.getOne(ctx, ns, key)
		if err != nil {
			if ctx.Err() != nil {
				return true
			}
			m.log.Error("config listener getOne failed", "namespace", ns, "key", key, "err", err)
			continue
		}
		if !found {
			continue // a delete (only upserts exist today) — nothing to cache
		}
		m.svc.setCacheOne(ns, key, v)
		bus.Emit(m.bus, configevents.ChangedEvent, configevents.Changed{Namespace: ns, Key: key, Value: v})
	}
}

// backoff waits a short interval, returning early if ctx is cancelled so
// shutdown stays prompt and a reconnect storm never tight-spins.
func (m *Module) backoff(ctx context.Context) {
	t := time.NewTimer(time.Second)
	defer t.Stop()
	select {
	case <-ctx.Done():
	case <-t.C:
	}
}
