//! Fleet manifest — the single deciding place for WHAT the game-backend fleet
//! is: process names, ports, boot order, and per-process env. This is a
//! faithful, config-as-code PORT of `tools/processctl/src/fleet.rs`'s
//! `game_backend_fleet`/`game_backend_monolith` (Development flavor) —
//! copied, not imported (weles is zero-sharing: it may never depend on a
//! workspace crate). Nothing else in `weles` may hardcode a port or env
//! name; every other module reads the manifest.
//!
//! The `Vec` returned by [`split_fleet`] IS the boot order — dependencies
//! are expressed implicitly by position (a service's peers appear earlier),
//! matching `fleet.rs`'s `dependencies` ordering constraint without needing
//! a separate graph here.
//!
//! Deliberate semantic delta vs the fleet.rs Development flavor: weles's
//! composed env is fully deterministic — the `overrideable_env` seam
//! (`tools/processctl/src/fleet.rs:568-584`, which lets ambient
//! `SCHEDULER_ENABLED`/`ACCOUNTS_DEV_AUTH`/`ADMIN_COOKIE_SECURE`/… override
//! the manifest) was consciously NOT ported, per the config-as-code
//! decision: what a service gets is exactly what this file says, plus the
//! fixed [`SERVICE_ENV_ALLOWLIST`] passthrough.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Parent-process env vars a spawned service may inherit. Mirrors
/// `tools/processctl/src/fleet.rs::SERVICE_ENV_ALLOWLIST` exactly — topology
/// wiring and bind addresses are never inherited, only ambient
/// interpreter/toolchain plumbing.
pub const SERVICE_ENV_ALLOWLIST: &[&str] = &[
    "COMSPEC",
    "HOME",
    "PATH",
    "PATHEXT",
    "RUST_BACKTRACE",
    "RUST_LOG",
    "SYSTEMROOT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "WINDIR",
];

/// A single fleet process: its identity, ports, whether it owns a Postgres
/// pool, and the env pairs unique to it (topology wiring, dev-mode opt-ins).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceDef {
    pub name: &'static str,
    pub pkg: &'static str,
    pub http_port: u16,
    pub edge_port: Option<u16>,
    pub player_port: Option<u16>,
    pub has_db: bool,
    /// `DATABASE_POOL_MAX_CONNECTIONS` for this process. Ignored when
    /// `has_db` is false (gateway-svc: pure-transport, no pool).
    pub pool_max: u32,
    pub env_extra: &'static [(&'static str, &'static str)],
}

/// Runtime values only known at supervisor start (never hardcoded here):
/// the local Postgres DSN and the mTLS CA material path.
pub struct RuntimeInputs {
    pub database_url: String,
    pub ca_cert: PathBuf,
    pub ca_key: PathBuf,
}

/// Per-DB-process pooled-connection cap in the split. Mirrors
/// `tools/processctl/src/fleet.rs::SPLIT_SERVICE_POOL_MAX`.
const SPLIT_SERVICE_POOL_MAX: u32 = 3;

/// Pooled-connection cap for the monolith. Mirrors
/// `tools/processctl/src/fleet.rs::MONOLITH_POOL_MAX`.
const MONOLITH_POOL_MAX: u32 = 20;

/// The 12-process split fleet, in boot order. Each dependency (a peer a
/// service dials over the internal mTLS edge) appears strictly earlier in
/// this list than its dependent, matching
/// `tools/processctl/src/fleet.rs::game_backend_fleet`'s `dependencies`
/// constraint by construction.
pub fn split_fleet() -> Vec<ServiceDef> {
    vec![
        ServiceDef {
            name: "accounts-svc",
            pkg: "accounts-svc",
            http_port: 8084,
            edge_port: Some(9003),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            env_extra: &[("ACCOUNTS_DEV_AUTH", "1")],
        },
        ServiceDef {
            name: "apikeys-svc",
            pkg: "apikeys-svc",
            http_port: 8091,
            edge_port: Some(9009),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            env_extra: &[("APIKEYS_DEV_SEED", "1")],
        },
        ServiceDef {
            name: "audit-svc",
            pkg: "audit-svc",
            http_port: 8086,
            edge_port: Some(9004),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            env_extra: &[],
        },
        ServiceDef {
            name: "scheduler-svc",
            pkg: "scheduler-svc",
            http_port: 8087,
            edge_port: Some(9005),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            // Deliberately NO SCHEDULER_ENABLED here — this manifest is the
            // Development flavor of tools/processctl/src/fleet.rs, which
            // only sets SCHEDULER_ENABLED under FleetFlavor::Proof.
            env_extra: &[],
        },
        ServiceDef {
            name: "rating-svc",
            pkg: "rating-svc",
            http_port: 8089,
            edge_port: Some(9007),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            env_extra: &[],
        },
        ServiceDef {
            name: "leaderboard-svc",
            pkg: "leaderboard-svc",
            http_port: 8090,
            edge_port: Some(9008),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            env_extra: &[],
        },
        ServiceDef {
            name: "match-svc",
            pkg: "match-svc",
            http_port: 8088,
            edge_port: Some(9006),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            env_extra: &[("RATING_EDGE_ADDR", "127.0.0.1:9007")],
        },
        ServiceDef {
            name: "config-svc",
            pkg: "config-svc",
            http_port: 8083,
            edge_port: Some(9002),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            env_extra: &[],
        },
        ServiceDef {
            name: "characters-svc",
            pkg: "characters-svc",
            http_port: 8080,
            edge_port: Some(9000),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            env_extra: &[("CONFIG_EDGE_ADDR", "127.0.0.1:9002")],
        },
        ServiceDef {
            name: "inventory-svc",
            pkg: "inventory-svc",
            http_port: 8081,
            edge_port: Some(9001),
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            env_extra: &[
                ("CHARACTERS_EDGE_ADDR", "127.0.0.1:9000"),
                ("CONFIG_EDGE_ADDR", "127.0.0.1:9002"),
                ("INVENTORY_DEV_GRANT", "1"),
            ],
        },
        ServiceDef {
            name: "gateway-svc",
            pkg: "gateway-svc",
            http_port: 8082,
            edge_port: None,
            player_port: Some(9100),
            has_db: false,
            pool_max: 0,
            env_extra: &[
                ("PLAYER_EDGE_ADDR", ":9100"),
                ("TLS_MODE", "off"),
                ("CHARACTERS_EDGE_ADDR", "127.0.0.1:9000"),
                ("INVENTORY_EDGE_ADDR", "127.0.0.1:9001"),
                ("ACCOUNTS_EDGE_ADDR", "127.0.0.1:9003"),
                ("MATCH_EDGE_ADDR", "127.0.0.1:9006"),
                ("LEADERBOARD_EDGE_ADDR", "127.0.0.1:9008"),
                ("APIKEYS_EDGE_ADDR", "127.0.0.1:9009"),
                ("ADMIN_HTTP_ADDR", "127.0.0.1:8085"),
                ("ACCOUNTS_HTTP_ADDR", "127.0.0.1:8084"),
            ],
        },
        ServiceDef {
            name: "admin-svc",
            pkg: "admin-svc",
            http_port: 8085,
            edge_port: None,
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            env_extra: &[
                ("CHARACTERS_EDGE_ADDR", "127.0.0.1:9000"),
                ("INVENTORY_EDGE_ADDR", "127.0.0.1:9001"),
                ("CONFIG_EDGE_ADDR", "127.0.0.1:9002"),
                ("ACCOUNTS_EDGE_ADDR", "127.0.0.1:9003"),
                ("AUDIT_EDGE_ADDR", "127.0.0.1:9004"),
                ("SCHEDULER_EDGE_ADDR", "127.0.0.1:9005"),
                ("APIKEYS_EDGE_ADDR", "127.0.0.1:9009"),
                ("ADMIN_COOKIE_SECURE", "0"),
                ("TRUSTED_PROXY_CIDRS", "127.0.0.1/32"),
            ],
        },
    ]
}

/// The single-process monolith topology (`cmd/server`, package `server`).
pub fn monolith() -> ServiceDef {
    ServiceDef {
        name: "server",
        pkg: "server",
        http_port: 8080,
        edge_port: None,
        player_port: Some(9100),
        has_db: true,
        pool_max: MONOLITH_POOL_MAX,
        env_extra: &[
            ("PLAYER_EDGE_ADDR", ":9100"),
            ("APIKEYS_DEV_SEED", "1"),
            ("ACCOUNTS_DEV_AUTH", "1"),
            ("INVENTORY_DEV_GRANT", "1"),
            ("TLS_MODE", "off"),
            ("ADMIN_COOKIE_SECURE", "0"),
            ("TRUSTED_PROXY_CIDRS", "127.0.0.1/32"),
        ],
    }
}

/// Builds the full spawn environment for `svc`: parent-env allowlist, then
/// `PORT`/`EDGE_ADDR`, then (if DB-backed, or gateway-svc which dials mTLS
/// edges without owning a pool) `DATABASE_URL`/`DATABASE_POOL_MAX_CONNECTIONS`/
/// `EDGE_CA_CERT`/`EDGE_CA_KEY`, then `env_extra` last (so a service's own
/// wiring always wins over anything synthesized above it).
pub fn compose_env(svc: &ServiceDef, inputs: &RuntimeInputs) -> BTreeMap<OsString, OsString> {
    let mut env: BTreeMap<OsString, OsString> = BTreeMap::new();

    for key in SERVICE_ENV_ALLOWLIST {
        if let Some(value) = std::env::var_os(key) {
            env.insert(OsString::from(*key), value);
        }
    }

    env.insert(OsString::from("PORT"), OsString::from(format!(":{}", svc.http_port)));
    if let Some(port) = svc.edge_port {
        env.insert(OsString::from("EDGE_ADDR"), OsString::from(format!(":{port}")));
    }

    if svc.has_db {
        env.insert(OsString::from("DATABASE_URL"), OsString::from(inputs.database_url.clone()));
        env.insert(
            OsString::from("DATABASE_POOL_MAX_CONNECTIONS"),
            OsString::from(svc.pool_max.to_string()),
        );
        env.insert(OsString::from("EDGE_CA_CERT"), inputs.ca_cert.clone().into_os_string());
        env.insert(OsString::from("EDGE_CA_KEY"), inputs.ca_key.clone().into_os_string());
    } else if svc.name == "gateway-svc" {
        // Pure-transport front door: no pool, but it still dials every peer
        // over the internal mTLS edge, so it needs the CA material despite
        // has_db == false. Verified against tools/processctl/src/fleet.rs
        // game_backend_fleet's `gateway_env` (inserts EDGE_CA_CERT/EDGE_CA_KEY
        // directly, never routes gateway through the DB-only `base()` helper
        // that also injects DATABASE_POOL_MAX_CONNECTIONS).
        env.insert(OsString::from("EDGE_CA_CERT"), inputs.ca_cert.clone().into_os_string());
        env.insert(OsString::from("EDGE_CA_KEY"), inputs.ca_key.clone().into_os_string());
    }

    for (key, value) in svc.env_extra {
        env.insert(OsString::from(*key), OsString::from(*value));
    }

    env
}

/// Fleet-manifest errors. Kept local to `weles` (zero-sharing: never reuses
/// `tools/processctl`'s `FleetError`).
#[derive(Debug)]
pub enum ManifestError {
    /// `cmd/*-svc` on disk disagrees with the canonical [`split_fleet`]
    /// names, in either direction. Lists EVERY drifted entry, not just the
    /// first — a didn't-forget tool dies pre-work with a per-entry log.
    DiskDrift { missing_on_disk: Vec<String>, missing_in_manifest: Vec<String> },
    ReadDir { path: PathBuf, source: std::io::Error },
    ReadEntry { path: PathBuf, source: std::io::Error },
    PoolBudgetExceeded { total: u32, budget: u32, breakdown: String },
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::DiskDrift { missing_on_disk, missing_in_manifest } => {
                writeln!(f, "fleet manifest drift against cmd/*-svc on disk:")?;
                for name in missing_on_disk {
                    writeln!(f, "  on disk but not in manifest: {name}")?;
                }
                for name in missing_in_manifest {
                    writeln!(f, "  in manifest but not on disk: {name}")?;
                }
                Ok(())
            }
            ManifestError::ReadDir { path, source } => {
                write!(f, "read service directory {}: {source}", path.display())
            }
            ManifestError::ReadEntry { path, source } => {
                write!(f, "read entry in service directory {}: {source}", path.display())
            }
            ManifestError::PoolBudgetExceeded { total, budget, breakdown } => {
                write!(
                    f,
                    "fleet Postgres session reservation {total} exceeds budget {budget}\n{breakdown}"
                )
            }
        }
    }
}

impl std::error::Error for ManifestError {}

/// Diffs the canonical [`split_fleet`] names against the `*-svc` directories
/// under `cmd_dir`. Fails loudly, listing every drifted entry, in EITHER
/// direction (a service added to the manifest without its `cmd/*-svc` root,
/// or a `cmd/*-svc` root nobody wired into the manifest).
pub fn validate_disk(cmd_dir: &Path) -> Result<(), ManifestError> {
    let entries = std::fs::read_dir(cmd_dir)
        .map_err(|source| ManifestError::ReadDir { path: cmd_dir.to_path_buf(), source })?;
    let mut on_disk = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(|source| ManifestError::ReadEntry { path: cmd_dir.to_path_buf(), source })?;
        let file_type = entry
            .file_type()
            .map_err(|source| ManifestError::ReadEntry { path: entry.path(), source })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if file_type.is_dir() && name.ends_with("-svc") {
            on_disk.push(name);
        }
    }
    validate_names(on_disk)
}

fn validate_names(names: impl IntoIterator<Item = String>) -> Result<(), ManifestError> {
    use std::collections::BTreeSet;
    let on_disk: BTreeSet<String> = names.into_iter().collect();
    let canonical: BTreeSet<String> =
        split_fleet().into_iter().map(|svc| svc.name.to_string()).collect();

    let missing_on_disk: Vec<String> = canonical.difference(&on_disk).cloned().collect();
    let missing_in_manifest: Vec<String> = on_disk.difference(&canonical).cloned().collect();

    if missing_on_disk.is_empty() && missing_in_manifest.is_empty() {
        Ok(())
    } else {
        Err(ManifestError::DiskDrift { missing_on_disk, missing_in_manifest })
    }
}

/// Per-DB-process dedicated Postgres sessions held OUTSIDE the pool: both
/// durable-event-plane and invalidation-plane are constructed in any process
/// with a DB (DB ⇒ plane — see CLAUDE.md), so every DB-backed process
/// reserves the delivery workers + the wake-up listener + the invalidation
/// listener. Mirrors `tools/processctl/src/fleet.rs::PLANE_DEDICATED_SESSIONS`
/// (`AE_WORKERS`(2) + `AE_WAKEUP_SESSIONS`(1) + `INVALIDATION_LISTEN_SESSIONS`(1)).
const PLANE_DEDICATED_SESSIONS: u32 = 4;

/// scheduler-svc's one dedicated per-fire connection, beyond the two planes.
/// Mirrors `tools/processctl/src/fleet.rs::SCHEDULER_FIRE_SESSIONS`.
const SCHEDULER_FIRE_SESSIONS: u32 = 1;

/// Sessions reserved for dev tooling running ALONGSIDE the fleet (splitproof's
/// own sqlx pool, devctl/adminctl seeding, eventctl, asyncevents poison-burst
/// headroom, slack). Mirrors `tools/processctl/src/fleet.rs::HARNESS_RESERVE`
/// (itemized there; not re-derived here).
const HARNESS_RESERVE: u32 = 10;

/// Usable Postgres sessions the whole fleet + monolith must fit within.
/// Mirrors `tools/processctl/src/fleet.rs::PG_SESSION_BUDGET`
/// (`max_connections`(100) - `superuser_reserved_connections`(3) -
/// `HARNESS_RESERVE`).
pub const PG_SESSION_BUDGET: u32 = 97 - HARNESS_RESERVE;

/// Per-process (pool_max, dedicated) reservation for a manifest entry. Split
/// out as its own function so the arithmetic is unit-testable independent of
/// the real fleet data (see `manifest_tests`'s synthetic-numbers case proving
/// the dedicated term matters, not just the pool sum).
pub fn service_pg_budget(svc: &ServiceDef) -> (u32, u32) {
    if !svc.has_db {
        // Pure-transport front door: no pool, no plane.
        return (0, 0);
    }
    let mut dedicated = PLANE_DEDICATED_SESSIONS;
    // The scheduler's dedicated per-fire connection is charged wherever the
    // scheduler module runs: its own svc in the split, the "server" package
    // in the monolith (one process hosts every module).
    if svc.name == "scheduler-svc" || svc.pkg == "server" {
        dedicated += SCHEDULER_FIRE_SESSIONS;
    }
    (svc.pool_max, dedicated)
}

/// The one budget authority: sums pool_max + dedicated across `services`
/// and fails with the itemized breakdown if the total exceeds
/// [`PG_SESSION_BUDGET`]. Full fleet.rs arithmetic — NOT a pool-only
/// shortcut: the dedicated term (plane workers + listeners, scheduler's
/// fire connection) is charged too. Takes a slice so the failing branch is
/// exercisable with a synthetic fleet in tests (the public wrapper feeds
/// the real manifests).
fn fleet_pg_budget(services: &[ServiceDef]) -> Result<(), ManifestError> {
    let mut breakdown = String::new();
    let mut total = 0u32;
    for svc in services {
        let (pool, dedicated) = service_pg_budget(svc);
        total += pool + dedicated;
        breakdown.push_str(&format!(
            "  {name}: pool={pool} dedicated={dedicated}\n",
            name = svc.name
        ));
    }
    if total > PG_SESSION_BUDGET {
        return Err(ManifestError::PoolBudgetExceeded {
            total,
            budget: PG_SESSION_BUDGET,
            breakdown,
        });
    }
    Ok(())
}

/// Validates BOTH real manifests against [`PG_SESSION_BUDGET`]: the split
/// fleet as one deployment, the monolith as another (each topology runs
/// alone against the shared local Postgres).
pub fn validate_pg_budget() -> Result<(), ManifestError> {
    fleet_pg_budget(&split_fleet())?;
    fleet_pg_budget(&[monolith()])
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod manifest_tests;
