//! Postgres session-floor preflight — weles's own copy of the check
//! `processctl::require_pg_session_floor` performs for devctl and splitproof.
//!
//! COPIED, NOT IMPORTED: weles is zero-sharing (it depends on no workspace crate), so
//! [`REQUIRED_MAX_CONNECTIONS`], [`PLANE_DEDICATED_SESSIONS`] and [`CAPACITY_SQL`] are
//! duplicated from `tools/processctl/src/fleet.rs`, which owns the budget derivation.
//! Neither crate's tests can see the other copy; verifyctl's blocking
//! `weles-wire-contract` stage is what pins the two together, the same way it pins the
//! hand-copied agent wire contract.
//!
//! ## What weles charges, and the gap in it
//!
//! weles is domain-blind: it reads `DATABASE_POOL_MAX_CONNECTIONS` as the signal that a
//! service opens a pool, because that is the only per-service DB fact an operator
//! writes into a fleet file. `DATABASE_URL` is fleet-level `passthrough`, so it reaches
//! every service including the DB-less gateway and cannot serve as that signal. A
//! service that uses a database while leaving the pool size to the process's own
//! default is therefore NOT charged — a known under-count, preferred over charging a
//! DB-less process a pool it never opens and refusing a rollout that fits.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use sqlx::Connection as _;

use crate::fleet_toml::Fleet;

/// The `max_connections` this repo asks an operator to provision — the copy of
/// `processctl::REQUIRED_MAX_CONNECTIONS`, named only by the remedy.
pub const REQUIRED_MAX_CONNECTIONS: u32 = 150;

/// Dedicated sessions every DB-backed process holds outside its pool (both event-plane
/// delivery workers, the wake-up listener, the invalidation listener) — the copy of
/// `processctl`'s `PLANE_DEDICATED_SESSIONS`.
pub const PLANE_DEDICATED_SESSIONS: u32 = 4;

/// The one round-trip that reads what the cluster offers ordinary roles — the copy of
/// `processctl::PG_SESSION_CAPACITY_SQL`. `reserved_connections` exists only from
/// PostgreSQL 16, so it is read through the missing-ok form.
pub const CAPACITY_SQL: &str = "SELECT current_setting('max_connections')::int, \
     current_setting('superuser_reserved_connections')::int, \
     coalesce(current_setting('reserved_connections', true)::int, 0)";

const DATABASE_URL: &str = "DATABASE_URL";
const POOL_MAX: &str = "DATABASE_POOL_MAX_CONNECTIONS";
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// What a live cluster offers ordinary roles, as the cluster reports it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PgSessionCapacity {
    pub max_connections: u32,
    /// `superuser_reserved_connections` plus PostgreSQL 16+'s `reserved_connections`.
    pub reserved: u32,
}

impl PgSessionCapacity {
    pub fn usable(self) -> u32 {
        self.max_connections.saturating_sub(self.reserved)
    }
}

/// Refuses the boot unless every cluster this fleet will use offers what the fleet
/// reserves. A fleet with no pooled service reserves nothing and is not probed.
pub fn require_pg_session_floor(fleet: &Fleet) -> Result<()> {
    let required = fleet_session_reservation(fleet, &lookup_env);
    if required == 0 {
        return Ok(());
    }
    let dsns = dsn_candidates(fleet, &lookup_env);
    if dsns.is_empty() {
        println!(
            "weles: no DATABASE_URL reaches this fleet — {required} reserved session(s) \
             unverified"
        );
        return Ok(());
    }
    for dsn in dsns {
        let capacity = read_capacity(&dsn)?;
        check_pg_session_floor(capacity, required)?;
        println!(
            "weles: Postgres preflight OK: {} usable sessions >= {required} reserved",
            capacity.usable()
        );
    }
    Ok(())
}

/// The verdict, with no I/O of its own.
pub fn check_pg_session_floor(capacity: PgSessionCapacity, required: u32) -> Result<()> {
    if capacity.usable() >= required {
        return Ok(());
    }
    let suggested = REQUIRED_MAX_CONNECTIONS.max(required + capacity.reserved);
    bail!(
        "Postgres offers {} sessions to ordinary roles (max_connections {}, {} reserved), \
         below the {required} this fleet reserves. Raise the cluster:\
         \n    ALTER SYSTEM SET max_connections = {suggested};\
         \nthen RESTART the Postgres server — max_connections is postmaster-context, so \
         pg_reload_conf() does NOT apply it.",
        capacity.usable(),
        capacity.max_connections,
        capacity.reserved,
    )
}

/// Sessions the fleet reserves: every service that declares a pool holds that pool plus
/// [`PLANE_DEDICATED_SESSIONS`].
pub fn fleet_session_reservation(fleet: &Fleet, env: &dyn Fn(&str) -> Option<String>) -> u32 {
    fleet
        .services
        .iter()
        .filter_map(|service| resolved(&service.env, POOL_MAX, &fleet.passthrough, env))
        .filter_map(|value| value.trim().parse::<u32>().ok())
        .map(|pool| pool + PLANE_DEDICATED_SESSIONS)
        .sum()
}

/// Every distinct DSN this fleet's processes will actually receive. A literal
/// `[service.env]` / `[[prepare]].env` value is composed LAST and so wins over the
/// forwarded `passthrough` key — checking only the forwarded one would leave a fleet
/// that writes its own `DATABASE_URL` unprobed.
pub fn dsn_candidates(fleet: &Fleet, env: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut push = |value: Option<String>| {
        if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
            if !found.contains(&value) {
                found.push(value);
            }
        }
    };
    for service in &fleet.services {
        push(resolved(&service.env, DATABASE_URL, &fleet.passthrough, env));
    }
    for hook in &fleet.prepare {
        push(resolved(&hook.env, DATABASE_URL, &hook.passthrough, env));
    }
    found
}

/// One key as the spawned process will see it: the literal env table wins, otherwise a
/// declared passthrough key is forwarded from weles's own environment.
fn resolved(
    literal: &BTreeMap<String, String>,
    key: &str,
    passthrough: &[String],
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    literal
        .get(key)
        .cloned()
        .or_else(|| passthrough.iter().any(|declared| declared == key).then(|| env(key)).flatten())
}

fn lookup_env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

fn read_capacity(dsn: &str) -> Result<PgSessionCapacity> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build the preflight runtime")?;
    let (max_connections, superuser_reserved, reserved): (i32, i32, i32) =
        runtime.block_on(async {
            tokio::time::timeout(PROBE_TIMEOUT, async {
                let mut connection = sqlx::PgConnection::connect(dsn).await?;
                let row: (i32, i32, i32) = sqlx::query_as(CAPACITY_SQL)
                    .fetch_one(&mut connection)
                    .await?;
                connection.close().await?;
                Ok::<(i32, i32, i32), sqlx::Error>(row)
            })
            .await
            .with_context(|| format!("no answer from Postgres within {PROBE_TIMEOUT:?}"))?
            .context("read the Postgres session settings")
        })?;
    Ok(PgSessionCapacity {
        max_connections: max_connections.max(0) as u32,
        reserved: (superuser_reserved.max(0) + reserved.max(0)) as u32,
    })
}
