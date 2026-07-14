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
use bus::{EventContract, HistoryPolicy};
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

/// Bound on how long the V2 DDL's `pg_advisory_xact_lock` wait may take before
/// giving up loudly (SQLSTATE `55P03`), mirroring `core/lifecycle`'s
/// `MODULE_MIGRATE_LOCK_TIMEOUT` (same 60s convention — module migrate already
/// got this bound; the plane DDL was the missed sibling). `SET LOCAL`, not
/// `SET` + `RESET`: this lock is transaction-scoped (`pg_advisory_xact_lock`,
/// not `_lock`), so `SET LOCAL` self-resets at `COMMIT`/`ROLLBACK` — simpler
/// than lifecycle's session-lock dance (`SET` before, `RESET` after on the same
/// connection) which exists only because that lock spans many independent
/// transactions.
const V2_MIGRATE_LOCK_TIMEOUT: &str = "60s";

/// The V2 DDL, normative in the plan. Idempotent; runs in ONE transaction under
/// the exclusive migrate advisory lock (`{migrate_key}`), bounded by
/// `SET LOCAL lock_timeout = '{lock_timeout}'` (see [`V2_MIGRATE_LOCK_TIMEOUT`]).
/// `asyncevents.append_event`
/// owns the WHOLE writer protocol so there is exactly one implementation:
/// shared advisory lock -> read `plane_meta.generation` -> INSERT stamped with
/// `pg_current_xact_id()` -> return the stable `event_id`. `ensure_history_contract`
/// is the plane-owned history-seed surface a module-owned writer (config's trigger
/// seed) calls WITHOUT touching the plane's tables (archcheck-enforced); it mirrors
/// [`ensure_history_contract`]'s ON-CONFLICT-then-drift-check semantics in SQL. The
/// `AFTER INSERT` trigger fires the wake-up NOTIFY the Step-3 worker will LISTEN on.
/// `tie_breaker` is a table-wide identity: within one transaction it is strictly
/// increasing in append order, which is all the position ordering needs.
/// The exact number of top-level statements in [`V2_DDL_TEMPLATE`], INCLUDING
/// the literal `BEGIN;`/`COMMIT;` pair (which the executor skips). Pinned as a
/// loud tripwire for [`split_sql_statements`]: the splitter is deliberately
/// minimal (dollar-quote-aware only — it does not lex `--` comments or `'...'`
/// string literals, because the template contains no top-level `;` inside
/// either), so if a future edit to the template adds one, the count assert in
/// [`ensure_schema_with_lock_timeout`] fails at the first migrate with a
/// message naming this constant — a missplit statement must never reach
/// Postgres half-parsed. UPDATE THIS COUNT (and re-check the splitter's
/// assumptions) whenever a statement is added to or removed from the template
/// in `core/asyncevents/src/store.rs`.
const V2_DDL_STATEMENT_COUNT: usize = 14;

const V2_DDL_TEMPLATE: &str = r#"
BEGIN;
SET LOCAL lock_timeout = '{lock_timeout}';
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
CREATE OR REPLACE FUNCTION asyncevents.ensure_history_contract(
	_topic text, _version integer, _policy text, _days integer)
	RETURNS void LANGUAGE plpgsql AS $$
DECLARE
	_stored_policy text;
	_stored_days   integer;
BEGIN
	INSERT INTO asyncevents.history_contracts (topic, contract_version, policy, min_retention_days)
	VALUES (_topic, _version, _policy, _days)
	ON CONFLICT (topic, contract_version) DO NOTHING;
	SELECT policy, min_retention_days INTO _stored_policy, _stored_days
		FROM asyncevents.history_contracts WHERE topic = _topic AND contract_version = _version;
	-- Day count only matters under min_retention; keep_forever's stored days are a
	-- NOT-NULL placeholder, never compared. A drifted policy fails loud (a topic's
	-- history promise is immutable), never silently adopts the stored one.
	IF _stored_policy IS DISTINCT FROM _policy
		OR (_policy = 'min_retention' AND _stored_days IS DISTINCT FROM _days) THEN
		RAISE EXCEPTION 'asyncevents: history_contracts for (%, v%) records policy %/%d, but the caller''s contract declares %/%d — a topic''s history policy is immutable',
			_topic, _version, _stored_policy, _stored_days, _policy, _days;
	END IF;
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
    ensure_schema_with_lock_timeout(pool, V2_MIGRATE_LOCK_TIMEOUT).await
}

/// [`ensure_schema`]'s body, parameterized on the `lock_timeout` GUC value so
/// tests can wait milliseconds instead of [`V2_MIGRATE_LOCK_TIMEOUT`]'s real
/// 60s (mirrors `core/lifecycle`'s `App::migrate_with_lock_timeout`).
/// `lock_timeout` is a trusted crate-internal/test string, never user/network
/// input — it is spliced into the DDL text because `SET LOCAL` cannot take a
/// bind parameter; the alphanumeric assert below is a tripwire for a malformed
/// caller value, not a security boundary.
///
/// Runs the DDL statement-by-statement inside ONE `sqlx::Transaction` (an
/// EXPLICIT connection, held for the whole batch, not `.execute(pool)`
/// per-statement, which would borrow a random pooled connection each time):
/// a 55P03 lock-wait abort — or any other mid-batch failure — can leave the
/// session in Postgres's ABORTED-TRANSACTION state, and a later borrower of
/// that same pooled connection would see every statement rejected with
/// "current transaction is aborted" until a ROLLBACK runs (confirmed by
/// `migrate_lock_timeout_fails_fast_without_poisoning_pool`'s
/// `max_connections(1)` reuse probe — sqlx's own `test_before_acquire` ping
/// does NOT clear this on its own). `Transaction`'s `Drop` issues a
/// best-effort ROLLBACK whenever it goes out of scope without `commit()`, so
/// an early `?` return on ANY statement failure cleans up automatically —
/// the pool never gets back a poisoned connection.
///
/// ERRATA — invariant narrowing vs `4b7d41c` ("discard connection on failed
/// rollback"): that commit's `raw_sql` executor handled the
/// rollback-ITSELF-failed-but-connection-alive edge with an explicit
/// `PoolConnection::detach()` so the unknown-state session could never serve
/// a later borrower. This `sqlx::Transaction`-based executor delegates that
/// edge to sqlx: `Transaction::drop`'s best-effort ROLLBACK has no
/// explicit-detach fallback, so if the ROLLBACK itself errors, discarding
/// the connection falls to the pool's DEFAULT `test_before_acquire` ping
/// (a wire-level liveness check the next borrower runs; a session whose
/// ROLLBACK failed is in practice a dead/protocol-broken connection the ping
/// rejects and the pool closes). Note the asymmetry with the paragraph
/// above: the ping does not clear a live-but-aborted transaction (why the
/// Drop-ROLLBACK is still needed), but it DOES catch a dead connection
/// (why the explicit detach no longer is). The production pools here are
/// `PgPool::connect`/`PgPoolOptions` with defaults — `test_before_acquire`
/// stays enabled — so the no-poisoned-borrower guarantee holds; a future
/// custom pool that disables it would silently reopen this edge.
///
/// Individual `sqlx::query(stmt)` calls (extended/prepared-statement
/// protocol) rather than one `sqlx::raw_sql(..)` multi-statement batch
/// (simple-query protocol) — [`split_sql_statements`] below splits the
/// template on top-level `;` (dollar-quote-aware, so a `;` inside a
/// PL/pgSQL function body is never treated as a boundary). This is NOT a
/// style preference: `sqlx::raw_sql(..).execute(&mut PgConnection)` does not
/// produce a generally-`Send` future in this sqlx version (0.8.6) — verified
/// by spawning an equivalent single-call helper in complete isolation
/// (every parameter owned, `'static`, no surrounding generator, no
/// composition with any other future) and still hitting rustc's
/// "implementation of Executor is not general enough" auto-trait leak check.
/// Nothing in production requires this function's future to be generally
/// `Send` today (`core/app::run` is a plain `async fn` its callers `.await`
/// or `select!` on one task), but the raw_sql form makes ANY future
/// Send-requiring composition over `Plane::migrate`'s call graph
/// unprovable; `sqlx::query(..).execute(&mut PgConnection)` has no such
/// problem (used pervasively elsewhere in this crate already —
/// [`control_identity_on`], the `plane_meta` seed insert below).
/// `raw_sql(..).execute(&Pool<DB>)` (the blanket impl every OTHER
/// `SCHEMA_DDL` migration in this workspace uses) also doesn't have the
/// problem, but does not admit holding one connection for the whole batch.
pub(crate) async fn ensure_schema_with_lock_timeout(
    pool: &PgPool,
    lock_timeout: &str,
) -> anyhow::Result<()> {
    // Tripwire, not a security boundary: the value is crate-internal (const or
    // test literal), but it is spliced into quoted SQL, so a malformed value
    // should die loudly here rather than as a cryptic DDL syntax error.
    assert!(
        !lock_timeout.is_empty() && lock_timeout.chars().all(|c| c.is_ascii_alphanumeric()),
        "asyncevents: lock_timeout {lock_timeout:?} must be ASCII alphanumeric \
         (digits + unit, e.g. \"60s\")"
    );
    let ddl = V2_DDL_TEMPLATE
        .replace("{lock_timeout}", lock_timeout)
        .replace("{migrate_key}", &MIGRATE_LOCK_KEY.to_string())
        .replace("{writer_key}", &WRITER_LOCK_KEY.to_string());

    let mut tx = pool
        .begin()
        .await
        .context("asyncevents: acquire V2 schema DDL connection")?;

    // Splitter tripwire (see [`V2_DDL_STATEMENT_COUNT`]): a top-level `;`
    // the minimal splitter does not understand (in a future `--` comment or
    // `'...'` string literal) would missplit a statement — fail loudly here,
    // before anything half-parsed reaches Postgres.
    let statements = split_sql_statements(&ddl);
    assert_eq!(
        statements.len(),
        V2_DDL_STATEMENT_COUNT,
        "asyncevents: split_sql_statements produced {} statements from V2_DDL_TEMPLATE, \
         expected V2_DDL_STATEMENT_COUNT = {} — either the template changed (update the \
         constant in core/asyncevents/src/store.rs) or the template now contains a \
         top-level ';' the splitter missplits (extend the splitter first)",
        statements.len(),
        V2_DDL_STATEMENT_COUNT,
    );

    // `BEGIN`/`COMMIT` are literal statements in the template (it predates
    // this sqlx::Transaction-based executor and was written for
    // `raw_sql`'s simple-query protocol); `sqlx::Transaction` already owns
    // that lifecycle, so skip them here rather than edit the long-lived,
    // carefully reviewed DDL text.
    for stmt in statements {
        let bare = stmt.trim_end_matches(';').trim();
        if bare.eq_ignore_ascii_case("BEGIN") || bare.eq_ignore_ascii_case("COMMIT") {
            continue;
        }
        if let Err(err) = sqlx::query(&stmt).execute(&mut *tx).await {
            // `tx` drops here (the `?`/early-`return` below), issuing sqlx's
            // own best-effort ROLLBACK — no manual detach/rollback dance
            // needed.
            let timed_out = matches!(
                &err,
                sqlx::Error::Database(db) if db.code().as_deref() == Some("55P03")
            );
            return if timed_out {
                Err(err).with_context(|| {
                    format!(
                        "asyncevents: V2 plane advisory lock not acquired within {lock_timeout} — \
                         another process is stuck mid plane-DDL; see pg_stat_activity"
                    )
                })
            } else {
                Err(err).context("asyncevents: V2 schema DDL failed")
            };
        }
    }
    tx.commit().await.context("asyncevents: V2 schema DDL commit failed")?;

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

/// Splits a semicolon-separated SQL script into individual top-level
/// statements, treating text between a matching pair of bare `$$` dollar
/// quotes as opaque — so a `;` inside a PL/pgSQL function body (this
/// crate's DDL uses only the bare `$$...$$` form, never a tagged
/// `$tag$...$tag$`) is never mistaken for a statement boundary. Blank
/// segments (trailing/leading whitespace between statements) are dropped.
/// See [`store_tests::split_sql_statements_respects_dollar_quoting`] for the
/// case this exists to get right.
fn split_sql_statements(script: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut in_dollar_quote = false;
    let mut chars = script.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'$') {
            current.push(c);
            current.push(chars.next().expect("peeked Some"));
            in_dollar_quote = !in_dollar_quote;
            continue;
        }
        if c == ';' && !in_dollar_quote {
            let stmt = current.trim();
            if !stmt.is_empty() {
                statements.push(stmt.to_string());
            }
            current.clear();
            continue;
        }
        current.push(c);
    }
    let tail = current.trim();
    if !tail.is_empty() {
        statements.push(tail.to_string());
    }
    // Guard: an ODD number of `$$` markers means the scan ended inside what it
    // believed was a dollar-quoted body — every split decision after the
    // unmatched `$$` was made in the wrong mode. Fail loudly instead of
    // returning silently-misgrouped statements.
    assert!(
        !in_dollar_quote,
        "asyncevents: split_sql_statements ended inside an unterminated $$ dollar quote — \
         the script's $$ markers are unbalanced (or use a tagged $tag$ form this minimal \
         splitter does not understand)"
    );
    statements
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

/// The `(policy, min_retention_days)` column pair for a [`HistoryPolicy`].
/// `KeepForever` carries no meaningful day count, so it takes the table default
/// (7) purely to satisfy `NOT NULL` — retention never reads it for that policy.
pub(crate) fn policy_columns(history: HistoryPolicy) -> (&'static str, i32) {
    match history {
        HistoryPolicy::MinRetention { days } => {
            ("min_retention", i32::try_from(days).unwrap_or(i32::MAX))
        }
        HistoryPolicy::KeepForever => ("keep_forever", 7),
    }
}

/// Seeds this event stream's row in `asyncevents.history_contracts` — the
/// retention GC's per-`(topic, version)` policy source — on the CALLER's open
/// connection, `ON CONFLICT (topic, contract_version) DO NOTHING`. Then reads the
/// stored row back and FAILS LOUDLY ([`bus::Error`]) if it records a DIFFERENT
/// policy than this code's contract: a topic's history promise is immutable, so a
/// drifted code contract must break the emit, never silently adopt the stored one.
/// Both native producers (via [`crate::transport::LogTransport::enqueue_tx`]) and
/// typed-subscription reconcile ([`crate::catalog`]) call this so the row appears
/// as soon as either side touches the topic. Takes primitives (not
/// `&EventContract`) so the reconcile path need not leak a `&'static str`.
pub async fn ensure_history_contract(
    conn: &mut PgConnection,
    topic: &str,
    version: u32,
    history: HistoryPolicy,
) -> Result<(), bus::Error> {
    let (policy, days) = policy_columns(history);
    let version = i32::try_from(version).map_err(bus::Error::transport)?;
    sqlx::query(
        "INSERT INTO asyncevents.history_contracts \
             (topic, contract_version, policy, min_retention_days) \
         VALUES ($1, $2, $3, $4) ON CONFLICT (topic, contract_version) DO NOTHING",
    )
    .bind(topic)
    .bind(version)
    .bind(policy)
    .bind(days)
    .execute(&mut *conn)
    .await
    .map_err(bus::Error::transport)?;

    let (stored_policy, stored_days): (String, i32) = sqlx::query_as(
        "SELECT policy, min_retention_days FROM asyncevents.history_contracts \
         WHERE topic = $1 AND contract_version = $2",
    )
    .bind(topic)
    .bind(version)
    .fetch_one(&mut *conn)
    .await
    .map_err(bus::Error::transport)?;

    // Day count only matters under `min_retention`; `keep_forever`'s stored days
    // are a NOT-NULL placeholder, never compared.
    let drifted = stored_policy != policy
        || (policy == "min_retention" && stored_days != days);
    if drifted {
        return Err(bus::Error::transport(std::io::Error::other(format!(
            "asyncevents: history_contracts for ({topic}, v{version}) records policy \
             {stored_policy}/{stored_days}d, but this process's code contract declares \
             {policy}/{days}d — a topic's history policy is immutable; reconcile the code \
             or migrate the row deliberately (never adopt silently)"
        ))));
    }
    Ok(())
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
pub(crate) mod store_tests;
