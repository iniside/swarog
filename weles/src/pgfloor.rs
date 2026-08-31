//! Postgres session-floor preflight — weles's own copy of the check
//! `processctl::require_pg_session_floor` performs for devctl and splitproof.
//!
//! COPIED, NOT IMPORTED: weles is zero-sharing (it depends on no workspace crate), so
//! [`REQUIRED_MAX_CONNECTIONS`], [`PLANE_DEDICATED_SESSIONS`], [`DEFAULT_DATABASE_URL`]
//! and [`CAPACITY_SQL`] are duplicated from `tools/processctl/src/fleet.rs`, which owns
//! the budget derivation. Neither crate's tests can see the other copy; verifyctl's
//! blocking `weles-wire-contract` stage is what pins the two together, the same way it
//! pins the hand-copied agent wire contract.
//!
//! ## What weles charges — and the three things it deliberately does not
//!
//! weles is domain-blind: it reads `DATABASE_POOL_MAX_CONNECTIONS` as the signal that a
//! service opens a pool, because that is the only per-service DB fact an operator writes
//! into a fleet file. `DATABASE_URL` cannot serve as that signal — it is fleet-level
//! `passthrough`, so it reaches every service including the DB-less gateway, and a
//! service that receives none still falls back to [`DEFAULT_DATABASE_URL`] inside
//! `core/app`. Each service that declares a pool is charged `pool +
//! PLANE_DEDICATED_SESSIONS`. Three consequences, none of them silent:
//!
//! 1. **A DB-backed service that leaves the pool size to the process default is not
//!    charged at all.** Preferred over charging every service that merely receives a
//!    `DATABASE_URL`, which would bill the DB-less gateway a pool it never opens and
//!    refuse rollouts that fit.
//! 2. **The scheduler's extra dedicated fire connection is not charged.** processctl
//!    charges it (`SCHEDULER_FIRE_SESSIONS`) because it knows which service is the
//!    scheduler; weles knows service names, never their meaning, so its total is one
//!    session short of processctl's for a fleet containing a scheduler — 98 rather than
//!    99 for `fleet.split.toml`. An under-count of one, recorded rather than guessed at
//!    by name-matching `"scheduler-svc"`, which would be exactly the domain knowledge
//!    the fleet file exists to keep out of weles.
//! 3. **A malformed `DATABASE_POOL_MAX_CONNECTIONS` is REFUSED, not skipped.**
//!    `core/app` falls back to a real pool for an unparseable value, so skipping it
//!    would under-charge a service that goes on to open ten connections.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use sqlx::Connection as _;

use crate::fleet_toml::Fleet;
use crate::manifest;

/// The `max_connections` this repo asks an operator to provision — the copy of
/// `processctl::REQUIRED_MAX_CONNECTIONS`, named only by the remedy.
pub const REQUIRED_MAX_CONNECTIONS: u32 = 150;

/// Dedicated sessions every DB-backed process holds outside its pool (both event-plane
/// delivery workers, the wake-up listener, the invalidation listener) — the copy of
/// `processctl`'s `PLANE_DEDICATED_SESSIONS`.
pub const PLANE_DEDICATED_SESSIONS: u32 = 4;

/// Where a service connects when no `DATABASE_URL` reaches it — the copy of
/// `processctl::DEFAULT_DATABASE_URL`, which is `core/app`'s own default. This is what
/// makes "no DSN in the fleet" mean "the default cluster", never "no cluster".
pub const DEFAULT_DATABASE_URL: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

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

/// Refuses the boot unless the cluster this fleet will use offers what the fleet
/// reserves. A fleet with no pooled service reserves nothing and is not probed; a fleet
/// that reserves anything is probed, and an unreachable cluster aborts the boot rather
/// than passing unverified.
pub fn require_pg_session_floor(fleet: &Fleet) -> Result<()> {
    let required = fleet_session_reservation(fleet, &manifest::lookup_env)?;
    if required == 0 {
        return Ok(());
    }
    let dsn = fleet_dsn(fleet, &manifest::lookup_env)?;
    let capacity = read_capacity(&dsn)?;
    check_pg_session_floor(capacity, required)?;
    println!(
        "weles: Postgres preflight OK: {} usable sessions >= {required} reserved",
        capacity.usable()
    );
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
/// [`PLANE_DEDICATED_SESSIONS`]. An unparseable pool value is an error, not a zero.
pub fn fleet_session_reservation(
    fleet: &Fleet,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<u32> {
    let mut total = 0;
    for service in &fleet.services {
        let Some(declared) = resolved(&service.env, POOL_MAX, &fleet.passthrough, env)? else {
            continue;
        };
        let pool: u32 = declared.trim().parse().with_context(|| {
            format!(
                "service {}: {POOL_MAX} is {declared:?}, which is not a session count",
                service.name
            )
        })?;
        total += pool + PLANE_DEDICATED_SESSIONS;
    }
    Ok(total)
}

/// The ONE cluster this fleet uses. A per-service literal `DATABASE_URL` may name a
/// different cluster than its neighbours; that fleet shape is refused rather than
/// checked, because a reservation summed over the whole fleet says nothing about a
/// cluster only part of it connects to.
pub fn fleet_dsn(fleet: &Fleet, env: &dyn Fn(&str) -> Option<OsString>) -> Result<String> {
    let mut found: Vec<String> = Vec::new();
    let mut candidates: Vec<Option<String>> = Vec::new();
    for service in &fleet.services {
        candidates.push(resolved(&service.env, DATABASE_URL, &fleet.passthrough, env)?);
    }
    for hook in &fleet.prepare {
        candidates.push(resolved(&hook.env, DATABASE_URL, &hook.passthrough, env)?);
    }
    for candidate in candidates.into_iter().flatten() {
        let candidate = candidate.trim().to_string();
        if !candidate.is_empty() && !found.contains(&candidate) {
            found.push(candidate);
        }
    }
    match found.len() {
        // core/app's own fallback: the fleet still opens every session it reserved,
        // against the default cluster.
        0 => Ok(DEFAULT_DATABASE_URL.to_string()),
        1 => Ok(found.remove(0)),
        _ => bail!(
            "this fleet points its processes at {} different Postgres clusters; weles \
             supports one shared cluster per fleet (the session reservation it preflights \
             is a fleet-wide sum, which no single one of those clusters carries)",
            found.len()
        ),
    }
}

/// One key as the spawned process will see it: the literal env table wins (it is
/// composed LAST), otherwise a declared passthrough key is forwarded from weles's own
/// environment through the SAME lookup `manifest::compose_env_with_fleet` uses, so the
/// preflight and the spawn can never resolve a key differently.
fn resolved(
    literal: &BTreeMap<String, String>,
    key: &str,
    passthrough: &[String],
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Option<String>> {
    if let Some(value) = literal.get(key) {
        return Ok(Some(value.clone()));
    }
    if !passthrough.iter().any(|declared| declared == key) {
        return Ok(None);
    }
    let Some(value) = env(key) else {
        return Ok(None);
    };
    value
        .into_string()
        .map(Some)
        .map_err(|value| anyhow::anyhow!("{key} is not valid UTF-8: {value:?}"))
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

#[cfg(test)]
#[path = "pgfloor_tests.rs"]
mod pgfloor_tests;
