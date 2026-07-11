# Event-plane ops: xmin pinning and delivery staleness

Operational notes for the durable `asyncevents` plane (see CLAUDE.md seam #3 and
lifecycle rule #8) that don't belong in the terse top-level guidance.

## The idle-in-tx belt only covers this plane's OWN sessions

Each worker's delivery session (`core/asyncevents/src/worker.rs::connect`) sets
`idle_in_transaction_session_timeout` to 2x `ASYNCEVENTS_HANDLER_TIMEOUT` on
connect. This is a belt against a worker leaking its OWN open transaction — a
dropped future between statements would otherwise leave the session
idle-in-transaction, holding a row lock and pinning `xmin` indefinitely. The
handler-timeout arm's `pg_terminate_backend` only reaches a backend wedged
INSIDE a statement; the per-session `SET` is what bounds the "silently never
resumed" case.

**It does not, and cannot, cover a rogue idle-in-transaction session anywhere
ELSE in the cluster** — a stuck migration, a forgotten `psql` session, another
service's leaked transaction, an ad-hoc admin query left open in a `BEGIN`.
Any such session still pins `xmin` cluster-wide and can stall the plane's
safe-delete frontier and, transitively, delivery, even though every
`asyncevents` worker session is individually well-behaved.

## Mitigation

- Set a **global** `idle_in_transaction_session_timeout` in `postgresql.conf`
  (or `ALTER SYSTEM SET idle_in_transaction_session_timeout = '...'`) so no
  session anywhere in the cluster — plane-owned or not — can idle-in-tx
  indefinitely. This is a cluster-wide operator decision, not something the
  plane can enforce from inside its own connections.
- Alert on the existing `asyncevents_safe_frontier_age_seconds` gauge: a
  growing frontier age is the first externally visible symptom of an xmin
  pin, whether the cause is a plane worker or an unrelated session.
- `/readyz` independently flags DELIVERY STALENESS (no worker completed a
  healthy pass in 30s) as well as a dead worker task — see CLAUDE.md lifecycle
  rule #8 — but that check is process-local and only catches this process's
  own workers stalling, not the upstream xmin pin causing the stall.

## Related

- `cargo run -p eventctl -- list` — lag/retry/pause/resume/skip/retire per
  subscription.
- CLAUDE.md, "The point of this codebase" seam #3 (durable event bus) and
  lifecycle rule #8 (readyz/idle-in-tx summary).
