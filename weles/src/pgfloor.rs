//! Postgres session-floor preflight — weles's own copy of the check
//! `processctl::require_pg_session_floor` performs for devctl and splitproof.
//!
//! COPIED, NOT IMPORTED: weles is zero-sharing (it depends on no workspace crate),
//! so the two numbers below are duplicated from `tools/processctl/src/fleet.rs`, which
//! owns the budget derivation. weles needs the check most — its `fleet.split.toml`
//! pins a LARGER `DATABASE_POOL_MAX_CONNECTIONS` per service than the split-proof
//! fleet does, so a stock `max_connections = 100` cluster runs out mid-boot.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use sqlx::Connection as _;

/// `max_connections` the cluster must be running with — the copy of
/// `processctl::REQUIRED_MAX_CONNECTIONS`.
const REQUIRED_MAX_CONNECTIONS: u32 = 150;

/// The env key a DB-backed fleet forwards to its services. weles is domain-blind about
/// its VALUE; it recognizes the key only to know whether this fleet has a cluster to
/// check at all.
const DATABASE_URL: &str = "DATABASE_URL";

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Refuses the boot unless the cluster the fleet will use is provisioned for it.
///
/// A fleet that forwards no `DATABASE_URL` has no cluster to check, so it passes
/// without a probe; one that does is checked before any service or prepare hook runs.
pub fn require_pg_session_floor(passthrough: &[String]) -> Result<()> {
    if !passthrough.iter().any(|key| key == DATABASE_URL) {
        return Ok(());
    }
    let Some(url) = std::env::var(DATABASE_URL).ok().filter(|url| !url.trim().is_empty()) else {
        return Ok(());
    };
    let observed = read_max_connections(&url)?;
    if observed < REQUIRED_MAX_CONNECTIONS {
        bail!(
            "Postgres max_connections is {observed}, below the {REQUIRED_MAX_CONNECTIONS} this \
             fleet requires. Raise the cluster:\
             \n    ALTER SYSTEM SET max_connections = {REQUIRED_MAX_CONNECTIONS};\
             \nthen RESTART the Postgres server — max_connections is postmaster-context, so \
             pg_reload_conf() does NOT apply it."
        );
    }
    println!("weles: Postgres preflight OK: max_connections {observed} >= {REQUIRED_MAX_CONNECTIONS}");
    Ok(())
}

fn read_max_connections(url: &str) -> Result<u32> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build the preflight runtime")?;
    let raw: String = runtime.block_on(async {
        tokio::time::timeout(PROBE_TIMEOUT, async {
            let mut connection = sqlx::PgConnection::connect(url).await?;
            let raw: String = sqlx::query_scalar("SHOW max_connections")
                .fetch_one(&mut connection)
                .await?;
            connection.close().await?;
            Ok::<String, sqlx::Error>(raw)
        })
        .await
        .with_context(|| format!("no answer from Postgres within {PROBE_TIMEOUT:?}"))?
        .context("read max_connections from DATABASE_URL")
    })?;
    raw.trim()
        .parse()
        .with_context(|| format!("max_connections is not a number: {raw}"))
}
