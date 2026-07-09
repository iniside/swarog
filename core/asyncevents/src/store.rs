//! V2 event-log storage: the XID-ordered shared log (`asyncevents.events`), the
//! commit-safe append protocol, and the plane's identity/generation metadata.
//! Additive at Step 2 — the legacy `outbox`/`inbox` push path in
//! [`crate::producer`] stays the live delivery mechanism until the pull-worker
//! cutover (plan Step 3); nothing here is wired into `Transport::enqueue_tx` yet.
//!
//! Correctness model (docs/plans/2026-07-09-2234-durable-event-log-fresh-plan.md):
//! a position is `(generation, producer_xid, tie_breaker)` — `bigserial` is never
//! a cursor. `producer_xid` is `pg_current_xact_id()` of the producing top-level
//! transaction; a reader may only observe current-generation rows satisfying
//! `producer_xid < pg_snapshot_xmin(pg_current_snapshot())` (the frontier), so an
//! earlier-xid transaction that commits LATER can never fill a gap behind an
//! advanced cursor. Every writer takes the transaction-scoped SHARED advisory
//! lock on [`WRITER_LOCK_KEY`] before reading `plane_meta.generation`; a
//! generation bump takes the EXCLUSIVE form, waiting every in-flight writer out,
//! so no event of generation N is ever inserted after the bump to N+1 commits.
//!
//! xid8 codec convention: sqlx has no `xid8` codec, so every bind/decode crosses
//! the boundary as text (`$n::xid8` in, `producer_xid::text` out);
//! `bus::EventPosition::xid` is a `u64` parsed from that text; ORDER/comparison
//! happens in SQL (row comparison), never in Rust.

use anyhow::Context as _;
use bus::EventContract;
use sqlx::{PgConnection, PgPool};

/// The one fixed advisory-lock key of the writer protocol. Shared form: every
/// appender (Rust [`append`] and any module-owned SQL calling
/// `asyncevents.append_event`, e.g. config's trigger at plan Step 7). Exclusive
/// form: [`bump_generation`] only. ASCII `"asyncevt"` as a positive i64.
pub const WRITER_LOCK_KEY: i64 = 0x6173_796E_6365_7674;

/// Serializes concurrent [`ensure_schema`] runs (parallel test binaries / split
/// processes booting against one DB): idempotent DDL racing itself can still
/// deadlock on catalog locks or fail `CREATE OR REPLACE FUNCTION` with "tuple
/// concurrently updated". ASCII `"asyncmig"`.
const MIGRATE_LOCK_KEY: i64 = 0x6173_796E_636D_6967;

/// The V2 DDL, normative in the plan. Idempotent; runs in ONE transaction under
/// the exclusive migrate advisory lock (`{migrate_key}`). `asyncevents.append_event`
/// owns the WHOLE writer protocol so there is exactly one implementation:
/// shared advisory lock -> read `plane_meta.generation` -> INSERT stamped with
/// `pg_current_xact_id()` -> return the stable `event_id`. The `AFTER INSERT`
/// trigger fires the wake-up NOTIFY the Step-3 worker will LISTEN on.
/// `tie_breaker` is a table-wide identity: within one transaction it is strictly
/// increasing in append order, which is all the position ordering needs.
const V2_DDL_TEMPLATE: &str = r#"
BEGIN;
SELECT pg_advisory_xact_lock({migrate_key});
CREATE SCHEMA IF NOT EXISTS asyncevents;
CREATE TABLE IF NOT EXISTS asyncevents.plane_meta (
	singleton         bool    PRIMARY KEY DEFAULT true CHECK (singleton),
	generation        bigint  NOT NULL,
	system_identifier numeric NOT NULL
);
CREATE TABLE IF NOT EXISTS asyncevents.events (
	generation       bigint      NOT NULL,
	producer_xid     xid8        NOT NULL,
	tie_breaker      bigint      GENERATED ALWAYS AS IDENTITY,
	event_id         text        NOT NULL UNIQUE DEFAULT gen_random_uuid()::text,
	topic            text        NOT NULL,
	contract_version integer     NOT NULL CHECK (contract_version > 0),
	payload          jsonb       NOT NULL,
	created_at       timestamptz NOT NULL DEFAULT now(),
	PRIMARY KEY (generation, producer_xid, tie_breaker)
);
CREATE INDEX IF NOT EXISTS events_scan
	ON asyncevents.events (topic, generation, producer_xid, tie_breaker);
CREATE TABLE IF NOT EXISTS asyncevents.subscriptions (
	subscription_id      text        PRIMARY KEY,
	topic                text        NOT NULL,
	contract_version     integer     NOT NULL,
	state                text        NOT NULL CHECK (state IN ('active','paused','retired')),
	cursor_generation    bigint      NOT NULL,
	cursor_xid           xid8        NOT NULL,
	cursor_tie           bigint      NOT NULL,
	next_attempt_at      timestamptz,
	consecutive_failures integer     NOT NULL DEFAULT 0,
	last_error           text,
	spec_hash            text        NOT NULL,
	start_kind           text        NOT NULL,
	updated_at           timestamptz NOT NULL
);
CREATE TABLE IF NOT EXISTS asyncevents.history_contracts (
	topic              text    NOT NULL,
	contract_version   integer NOT NULL,
	policy             text    NOT NULL CHECK (policy IN ('min_retention','keep_forever')),
	min_retention_days integer NOT NULL DEFAULT 7,
	PRIMARY KEY (topic, contract_version)
);
CREATE OR REPLACE FUNCTION asyncevents.append_event(_topic text, _version integer, _payload jsonb)
	RETURNS text LANGUAGE plpgsql AS $$
DECLARE
	_generation bigint;
	_event_id   text;
BEGIN
	PERFORM pg_advisory_xact_lock_shared({writer_key});
	SELECT generation INTO STRICT _generation FROM asyncevents.plane_meta WHERE singleton;
	INSERT INTO asyncevents.events (generation, producer_xid, topic, contract_version, payload)
	VALUES (_generation, pg_current_xact_id(), _topic, _version, _payload)
	RETURNING event_id INTO _event_id;
	RETURN _event_id;
END;
$$;
CREATE OR REPLACE FUNCTION asyncevents.notify_events() RETURNS trigger
	LANGUAGE plpgsql AS $$
BEGIN
	PERFORM pg_notify('asyncevents_events', NEW.topic);
	RETURN NULL;
END;
$$;
CREATE OR REPLACE TRIGGER events_notify
	AFTER INSERT ON asyncevents.events
	FOR EACH ROW EXECUTE FUNCTION asyncevents.notify_events();
COMMIT;"#;

/// Creates the V2 schema (idempotent, advisory-locked — see [`V2_DDL_TEMPLATE`])
/// and seeds the `plane_meta` singleton: generation 1, the CURRENT cluster's
/// `pg_control_system().system_identifier`, `ON CONFLICT DO NOTHING` so an
/// existing identity is never overwritten. Reading the identity is therefore a
/// prerequisite of the FIRST migrate, not just of the startup guard — hence the
/// GRANT bootstrap surfaces here too (via [`control_identity`]).
pub(crate) async fn ensure_schema(pool: &PgPool) -> anyhow::Result<()> {
    let ddl = V2_DDL_TEMPLATE
        .replace("{migrate_key}", &MIGRATE_LOCK_KEY.to_string())
        .replace("{writer_key}", &WRITER_LOCK_KEY.to_string());
    sqlx::raw_sql(&ddl)
        .execute(pool)
        .await
        .context("asyncevents: V2 schema DDL failed")?;
    let identity = control_identity(pool).await?;
    sqlx::query(
        "INSERT INTO asyncevents.plane_meta (generation, system_identifier) \
         VALUES (1, $1::numeric) ON CONFLICT DO NOTHING",
    )
    .bind(&identity)
    .execute(pool)
    .await
    .context("asyncevents: plane_meta seed failed")?;
    Ok(())
}

/// Appends one durable event by calling `asyncevents.append_event` — the single
/// writer implementation (shared advisory lock, generation read, xid8 stamp) —
/// on the CALLER's open transaction connection, so the event commits iff the
/// producer's domain change commits. Returns the stable `event_id`.
/// Not wired into `Transport::enqueue_tx` until the plan-Step-3 cutover.
pub async fn append(
    conn: &mut PgConnection,
    contract: &EventContract,
    payload: &[u8],
) -> Result<String, bus::Error> {
    // Bind the payload as text so `::jsonb` parses it (the bus already
    // JSON-encoded it, so it is valid UTF-8).
    let text = std::str::from_utf8(payload).map_err(bus::Error::transport)?;
    let version = i32::try_from(contract.version).map_err(bus::Error::transport)?;
    let (event_id,): (String,) =
        sqlx::query_as("SELECT asyncevents.append_event($1, $2, $3::jsonb)")
            .bind(contract.topic)
            .bind(version)
            .bind(text)
            .fetch_one(&mut *conn)
            .await
            .map_err(bus::Error::transport)?;
    Ok(event_id)
}

/// Bumps the log generation — an offline operator action (`eventctl
/// bump-generation`, plan Step 5), e.g. after restoring onto a new cluster.
/// Takes the EXCLUSIVE form of [`WRITER_LOCK_KEY`], which waits out every
/// in-flight shared writer, then atomically advances `generation` and adopts the
/// current cluster's `system_identifier`: no event of the old generation can
/// commit after the bump does. Returns the new generation.
pub async fn bump_generation(pool: &PgPool) -> anyhow::Result<i64> {
    let identity = control_identity(pool).await?;
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(WRITER_LOCK_KEY)
        .execute(&mut *tx)
        .await?;
    let (generation,): (i64,) = sqlx::query_as(
        "UPDATE asyncevents.plane_meta \
         SET generation = generation + 1, system_identifier = $1::numeric \
         WHERE singleton RETURNING generation",
    )
    .bind(&identity)
    .fetch_one(&mut *tx)
    .await
    .context("asyncevents: generation bump failed (plane_meta unseeded?)")?;
    tx.commit().await?;
    Ok(generation)
}

/// Boot-time invariants of the position model, checked at the earliest point
/// with a pool (`Plane::migrate`). Fails loud on:
/// - a changed cluster `system_identifier` (XID positions are not comparable
///   across clusters — the operator must bump the generation);
/// - `max_prepared_transactions != 0` or any row in `pg_prepared_xacts` (a
///   prepared event-producing tx would hold its xid outside every snapshot
///   indefinitely, stalling the delivery frontier).
pub async fn startup_guards(pool: &PgPool) -> anyhow::Result<()> {
    let mut conn = pool.acquire().await?;
    startup_guards_on(&mut conn).await
}

/// Connection-scoped form so a test can stage an identity mismatch inside an
/// uncommitted transaction (never visible to concurrently running suites).
pub(crate) async fn startup_guards_on(conn: &mut PgConnection) -> anyhow::Result<()> {
    let current = control_identity_on(conn).await?;
    let recorded: Option<String> =
        sqlx::query_scalar("SELECT system_identifier::text FROM asyncevents.plane_meta WHERE singleton")
            .fetch_optional(&mut *conn)
            .await?;
    let recorded = recorded
        .context("asyncevents: plane_meta is unseeded — the V2 migrate must run before the guards")?;
    if current != recorded {
        anyhow::bail!(
            "asyncevents: cluster system_identifier {current} does not match plane_meta \
             ({recorded}) — the database moved to a different cluster (restore/promotion), \
             so recorded XID positions are not comparable; run `eventctl bump-generation` \
             to fence the old positions and adopt the new identity"
        );
    }
    let mpt: String = sqlx::query_scalar("SELECT current_setting('max_prepared_transactions')")
        .fetch_one(&mut *conn)
        .await?;
    if mpt.trim() != "0" {
        anyhow::bail!(
            "asyncevents: max_prepared_transactions = {mpt}, must be 0 — a prepared \
             event-producing transaction sits outside every snapshot indefinitely, \
             stalling the delivery frontier"
        );
    }
    let prepared: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_prepared_xacts")
        .fetch_one(&mut *conn)
        .await?;
    if prepared != 0 {
        anyhow::bail!(
            "asyncevents: {prepared} prepared transaction(s) in pg_prepared_xacts — \
             commit or roll them back before booting the event plane"
        );
    }
    Ok(())
}

/// The current cluster identity as text (numeric has no cheap sqlx codec either;
/// text is exact). A permission failure names the documented one-time superuser
/// bootstrap — `pg_control_system()` is not executable by ordinary roles by
/// default.
async fn control_identity(pool: &PgPool) -> anyhow::Result<String> {
    let mut conn = pool.acquire().await?;
    control_identity_on(&mut conn).await
}

async fn control_identity_on(conn: &mut PgConnection) -> anyhow::Result<String> {
    match sqlx::query_scalar::<_, String>(
        "SELECT system_identifier::numeric::text FROM pg_control_system()",
    )
    .fetch_one(&mut *conn)
    .await
    {
        Ok(v) => Ok(v),
        Err(err) => {
            let role: String = sqlx::query_scalar("SELECT current_user")
                .fetch_one(&mut *conn)
                .await
                .unwrap_or_else(|_| "gamebackend".to_string());
            anyhow::bail!(
                "asyncevents: cannot read pg_control_system() (cluster-identity check): {err}; \
                 apply the one-time superuser bootstrap: \
                 GRANT EXECUTE ON FUNCTION pg_control_system() TO {role};"
            )
        }
    }
}

/// The xid8-as-text decode side of the codec convention: `producer_xid::text`
/// comes out of SQL, `EventPosition.xid` is the parsed `u64`. The encode side is
/// a text bind cast `$n::xid8`. Comparisons stay in SQL.
pub fn parse_xid8(text: &str) -> anyhow::Result<u64> {
    text.parse::<u64>()
        .with_context(|| format!("asyncevents: xid8 text {text:?} is not a u64"))
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod store_tests;
