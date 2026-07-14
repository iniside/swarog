use std::collections::{BTreeMap, BTreeSet};
#[cfg(windows)]
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use thiserror::Error;

pub const BUILD_ENV_ALLOWLIST: &[&str] = &[
    "ALL_PROXY", "APPDATA", "CARGO_HOME", "CARGO_HTTP_CAINFO", "CARGO_HTTP_PROXY",
    "CARGO_NET_GIT_FETCH_WITH_CLI", "CARGO_TARGET_DIR", "COMSPEC", "GIT_SSL_CAINFO",
    "HOME", "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "PATH", "PATHEXT", "RUSTFLAGS",
    "ProgramFiles(x86)", "RUSTUP_HOME", "SSL_CERT_DIR", "SSL_CERT_FILE", "SYSTEMROOT", "TEMP",
    "TMP", "USERPROFILE", "WINDIR", "all_proxy", "http_proxy", "https_proxy", "no_proxy",
];

const SERVICE_ENV_ALLOWLIST: &[&str] = &[
    "COMSPEC", "HOME", "PATH", "PATHEXT", "RUST_BACKTRACE", "RUST_LOG", "SYSTEMROOT",
    "TEMP", "TMP", "USERPROFILE", "WINDIR",
];

/// Sessions the local Postgres reserves for dev tooling running ALONGSIDE the fleet,
/// carved out of the usable budget before the processes get any. This is a HEURISTIC
/// reserve — an itemized estimate, deliberately not derivation machinery. The named
/// breakdown (add a new always-on consumer by item, never by nudging a bare number):
///
/// | item                                                     | sessions |
/// |----------------------------------------------------------|----------|
/// | splitproof's own sqlx assertion pool                     | ~4       |
/// | devctl psql seeding / adminctl                           | 1        |
/// | eventctl ad-hoc operator session                         | 1        |
/// | asyncevents poison-recovery burst                        | 2 (= [`AE_TRANSIENT_POISON_SESSIONS`]) |
/// | slack                                                    | 2        |
/// | **total**                                                | **10**   |
///
/// The poison-burst term is the mirrored const itself, so that line of the estimate
/// tracks the real mechanism; the other items are hand-estimated.
const HARNESS_RESERVE: u32 = 4 + 1 + 1 + AE_TRANSIENT_POISON_SESSIONS + 2;

/// Usable Postgres sessions the whole fleet + monolith must fit within. Assumes the
/// local Postgres runs stock defaults: `max_connections = 100` minus
/// `superuser_reserved_connections = 3` = 97 sessions for ordinary roles. BOTH are
/// PG-side configurable — an operator who raised `max_connections` has strictly MORE
/// headroom, so this is a conservative floor, not a hard platform limit. [`HARNESS_RESERVE`]
/// is subtracted so the fleet is charged only its own share.
pub(crate) const PG_SESSION_BUDGET: u32 = 97 - HARNESS_RESERVE;

// Local `u32` mirrors of the plane/module session constants that own the real
// mechanism. Kept as plain numbers so the RUNTIME fleet build carries no dependency on
// the heavy `asyncevents`/`invalidation`/`scheduler` crates; the
// `pool_budget_dedicated_matches_exported_session_constants` test (dev-deps on those
// crates) fails the build if any mirror drifts from its source of truth.
/// Mirror of `asyncevents::WORKERS` — dedicated delivery backends per DB process.
pub(crate) const AE_WORKERS: u32 = 2;
/// Mirror of `asyncevents::WAKEUP_SESSIONS` — the one NOTIFY wake-up `PgListener`.
pub(crate) const AE_WAKEUP_SESSIONS: u32 = 1;
/// Mirror of `invalidation::LISTEN_SESSIONS` — the one cache-invalidation `PgListener`.
pub(crate) const INVALIDATION_LISTEN_SESSIONS: u32 = 1;
/// Mirror of `scheduler::DEDICATED_FIRE_SESSIONS` — the scheduler's per-fire connection.
pub(crate) const SCHEDULER_FIRE_SESSIONS: u32 = 1;
/// Mirror of `asyncevents::TRANSIENT_POISON_SESSIONS` — transient poison-recovery burst
/// headroom. NOT charged per service; it rides inside [`HARNESS_RESERVE`]'s breakdown.
pub(crate) const AE_TRANSIENT_POISON_SESSIONS: u32 = 2;

/// Dedicated sessions EVERY DB-backed process reserves: both planes are constructed in
/// any process with a DB (DB ⇒ plane), so the worst case is the delivery workers + the
/// wake-up listener + the invalidation listener. A process without durable subs or cache
/// registrations holds fewer at runtime; reserving the full set is the safe
/// over-approximation for an exhaustion invariant. The COUNTS are drift-proof via the
/// mirror test; what needs a human re-audit is a plane growing a new session CATEGORY
/// (that is exactly how the transient-poison headroom arose) — a new category means a
/// new exported const, a new mirror, and a new term here or in the reserve.
const PLANE_DEDICATED_SESSIONS: u32 =
    AE_WORKERS + AE_WAKEUP_SESSIONS + INVALIDATION_LISTEN_SESSIONS;

/// Per-DB-process pooled-connection cap in the SPLIT. Low by necessity: 11 DB-backed
/// processes share one local Postgres, so each gets a small slice within
/// [`PG_SESSION_BUDGET`]. Comfortably above core/app's migrate floor (2). The pool's
/// concurrent users are the retention GC sweep, the metrics/invalidation poll refreshes,
/// the `/readyz` DB ping, and the HTTP/edge handlers — under the sequential split-proof
/// harness load these overlap only briefly, so 3 suffices; the failure mode of an
/// undersized pool is acquire-wait LATENCY (a slow request/probe), never a correctness
/// break, since every user waits on the pool rather than erroring.
const SPLIT_SERVICE_POOL_MAX: u32 = 3;

/// Pooled-connection cap for the MONOLITH — one process hosting every module + both
/// planes, so it affords a larger pool than a single split peer.
const MONOLITH_POOL_MAX: u32 = 20;

/// Compile-time proof that the monolith's single-process reservation fits the budget —
/// the monolith is built outside `FleetSpec::new` (it's one `ServiceSpec`, not a fleet),
/// so this const assertion is its dedicated budget check. It can never drift because it
/// is evaluated from the same consts the monolith's `pool_budget` is built from.
const _: () = assert!(
    MONOLITH_POOL_MAX + PLANE_DEDICATED_SESSIONS + SCHEDULER_FIRE_SESSIONS <= PG_SESSION_BUDGET,
    "monolith Postgres session reservation exceeds PG_SESSION_BUDGET"
);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FleetInputs {
    pub database_url: String,
    pub edge_ca_cert: PathBuf,
    pub edge_ca_key: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FleetFlavor {
    Development,
    Proof,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceSpec {
    pub name: &'static str,
    pub executable_package: &'static str,
    pub http_port: u16,
    pub edge_port: Option<u16>,
    pub player_port: Option<u16>,
    pub dependencies: Vec<&'static str>,
    pub env: BTreeMap<String, String>,
    /// Application settings that may be overridden from the single inherited
    /// environment snapshot. Topology wiring and bind addresses are never listed.
    pub overrideable_env: &'static [&'static str],
    /// This process's Postgres session reservation. `pool_max` is ALSO the value
    /// injected as `DATABASE_POOL_MAX_CONNECTIONS` (one field feeds BOTH the spawned
    /// process's runtime pool AND the fleet-wide exhaustion invariant), so runtime and
    /// invariant can never disagree. A DB-less process (gateway-svc) reserves `0`/`0`
    /// and gets no env injection.
    pub pool_budget: PoolBudget,
}

/// A process's Postgres session reservation, split into the pooled cap and the
/// dedicated sessions it holds OUTSIDE the pool. Their sum is what the fleet-wide
/// [`PG_SESSION_BUDGET`] invariant charges against one local Postgres.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PoolBudget {
    /// `PgPool` max_connections, injected as `DATABASE_POOL_MAX_CONNECTIONS`. `0` marks
    /// a DB-less process (no pool, env not injected).
    pub pool_max: u32,
    /// Dedicated Postgres sessions held outside the pool (plane delivery workers,
    /// NOTIFY listeners, the scheduler's per-fire connection). Derived from the
    /// exported session constants of the real crates — see the module-level
    /// `AE_*`/`INVALIDATION_*`/`SCHEDULER_*` mirrors and their anti-drift test.
    pub dedicated: u32,
}

#[derive(Clone, Debug)]
pub struct EnvironmentSnapshot {
    inherited: BTreeMap<String, String>,
}

impl EnvironmentSnapshot {
    pub fn capture() -> Self {
        Self { inherited: std::env::vars().collect() }
    }

    /// Constructs a deterministic snapshot, primarily for tooling tests.
    pub fn from_values(values: impl IntoIterator<Item = (String, String)>) -> Self {
        Self { inherited: values.into_iter().collect() }
    }

    pub fn value(&self, key: &str) -> Option<&str> {
        self.lookup(key).map(String::as_str)
    }

    pub fn build_environment(&self) -> BTreeMap<String, String> {
        let mut env = self.filtered(BUILD_ENV_ALLOWLIST);
        // LIB and INCLUDE are synthesized from the locally discovered toolchain.
        // They are not inherited authorities and therefore are not allowlist entries.
        #[cfg(windows)]
        append_msvc_linker_path(&mut env);
        env
    }

    pub fn runtime_environment(&self) -> BTreeMap<String, String> {
        self.filtered(SERVICE_ENV_ALLOWLIST)
    }

    fn filtered(&self, allowlist: &[&str]) -> BTreeMap<String, String> {
        allowlist.iter().filter_map(|key| {
            self.lookup(key).cloned().map(|value| ((*key).to_string(), value))
        }).collect()
    }

    fn lookup(&self, key: &str) -> Option<&String> {
        #[cfg(windows)]
        { self.inherited.iter().find(|(candidate, _)| candidate.eq_ignore_ascii_case(key)).map(|(_, value)| value) }
        #[cfg(not(windows))]
        { self.inherited.get(key) }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FleetSpec {
    services: Vec<ServiceSpec>,
}

#[derive(Debug, Error)]
pub enum FleetError {
    #[error("unknown service {0}")]
    UnknownService(String),
    #[error("duplicate service {0}")]
    DuplicateService(String),
    #[error("fleet Postgres session reservation {total} exceeds budget {budget}")]
    PoolBudgetExceeded { total: u32, budget: u32 },
    #[error("service {service} depends on unknown service {dependency}")]
    UnknownDependency { service: String, dependency: String },
    #[error("service {service} dependency {dependency} must appear earlier in startup order")]
    DependencyNotEarlier { service: String, dependency: String },
    #[error("fleet drift: cmd/*-svc on disk {on_disk:?} != canonical fleet {canonical:?}")]
    DiskDrift {
        on_disk: Vec<String>,
        canonical: Vec<String>,
    },
    #[error("read service directory {path}: {source}")]
    ReadDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("read entry in service directory {path}: {source}")]
    ReadDirectoryEntry {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("read service entry type for {path}: {source}")]
    ReadEntryType {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl FleetSpec {
    pub(crate) fn new(services: Vec<ServiceSpec>) -> Result<Self, FleetError> {
        let names: BTreeSet<_> = services.iter().map(|service| service.name).collect();
        if names.len() != services.len() {
            let mut seen = BTreeSet::new();
            let duplicate = services
                .iter()
                .map(|service| service.name)
                .find(|name| !seen.insert(*name))
                .expect("different lengths imply a duplicate");
            return Err(FleetError::DuplicateService(duplicate.to_string()));
        }
        for (index, service) in services.iter().enumerate() {
            for dependency in &service.dependencies {
                if !names.contains(dependency) {
                    return Err(FleetError::UnknownDependency {
                        service: service.name.to_string(),
                        dependency: (*dependency).to_string(),
                    });
                }
                if !services[..index]
                    .iter()
                    .any(|candidate| candidate.name == *dependency)
                {
                    return Err(FleetError::DependencyNotEarlier {
                        service: service.name.to_string(),
                        dependency: (*dependency).to_string(),
                    });
                }
            }
        }
        // The whole fleet shares ONE local Postgres. Charge every process's pooled cap
        // PLUS its dedicated sessions against the usable budget so the split can never
        // be provisioned into connection exhaustion. `pool_max` here is the SAME value
        // injected as `DATABASE_POOL_MAX_CONNECTIONS`, so this invariant and the running
        // pool size are one number.
        let total: u32 = services
            .iter()
            .map(|service| service.pool_budget.pool_max + service.pool_budget.dedicated)
            .sum();
        if total > PG_SESSION_BUDGET {
            return Err(FleetError::PoolBudgetExceeded {
                total,
                budget: PG_SESSION_BUDGET,
            });
        }
        Ok(Self { services })
    }

    pub fn services(&self) -> &[ServiceSpec] {
        &self.services
    }

    pub fn service(&self, name: &str) -> Result<&ServiceSpec, FleetError> {
        self.services
            .iter()
            .find(|service| service.name == name)
            .ok_or_else(|| FleetError::UnknownService(name.to_string()))
    }

    pub fn validate_disk(&self, cmd_dir: &Path) -> Result<(), FleetError> {
        let entries = std::fs::read_dir(cmd_dir).map_err(|source| FleetError::ReadDirectory {
            path: cmd_dir.to_path_buf(),
            source,
        })?;
        let mut on_disk = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| FleetError::ReadDirectoryEntry {
                path: cmd_dir.to_path_buf(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|source| FleetError::ReadEntryType {
                    path: path.clone(),
                    source,
                })?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if file_type.is_dir() && name.ends_with("-svc") {
                on_disk.push(name);
            }
        }
        self.validate_names(on_disk)
    }

    pub fn validate_names(
        &self,
        names: impl IntoIterator<Item = String>,
    ) -> Result<(), FleetError> {
        let mut on_disk: Vec<_> = names.into_iter().collect();
        on_disk.sort();
        let mut canonical: Vec<_> = self
            .services
            .iter()
            .map(|service| service.name.to_string())
            .collect();
        canonical.sort();
        if on_disk == canonical {
            Ok(())
        } else {
            Err(FleetError::DiskDrift { on_disk, canonical })
        }
    }
}

pub fn build_environment() -> BTreeMap<String, String> {
    EnvironmentSnapshot::capture().build_environment()
}

pub fn runtime_environment() -> BTreeMap<String, String> {
    EnvironmentSnapshot::capture().runtime_environment()
}

#[cfg(windows)]
fn append_msvc_linker_path(env: &mut BTreeMap<String, String>) {
    let Some(program_files) = std::env::var_os("ProgramFiles(x86)") else {
        return;
    };
    let visual_studio = PathBuf::from(program_files).join("Microsoft Visual Studio");
    let Ok(releases) = std::fs::read_dir(visual_studio) else {
        return;
    };
    let mut candidates = Vec::new();
    for release in releases.filter_map(Result::ok) {
        let Ok(editions) = std::fs::read_dir(release.path()) else {
            continue;
        };
        for edition in editions.filter_map(Result::ok) {
            let tools = edition.path().join("VC/Tools/MSVC");
            let Ok(versions) = std::fs::read_dir(tools) else {
                continue;
            };
            for version in versions.filter_map(Result::ok) {
                let tool_root = version.path();
                let bin = tool_root.join("bin/Hostx64/x64");
                if bin.join("link.exe").is_file() {
                    candidates.push((tool_root, bin));
                }
            }
        }
    }
    candidates.sort();
    let Some((msvc_root, linker_dir)) = candidates.pop() else {
        return;
    };
    let sdk_root = PathBuf::from(
        std::env::var_os("ProgramFiles(x86)").expect("ProgramFiles(x86) was present above"),
    )
    .join("Windows Kits/10");
    let sdk_version = newest_directory(&sdk_root.join("Lib"));

    let mut paths = vec![linker_dir];
    if let Some(version) = &sdk_version {
        let sdk_bin = sdk_root.join("bin").join(version).join("x64");
        if sdk_bin.is_dir() {
            paths.push(sdk_bin);
        }
    }
    if let Some(existing) = env.get("PATH") {
        paths.extend(std::env::split_paths(OsStr::new(existing)));
    }
    if let Ok(path) = std::env::join_paths(paths) {
        env.insert("PATH".into(), path.to_string_lossy().into_owned());
    }

    let mut libraries = vec![msvc_root.join("lib/x64")];
    let mut includes = vec![msvc_root.join("include")];
    if let Some(version) = sdk_version {
        libraries.extend(
            ["ucrt/x64", "um/x64"]
                .into_iter()
                .map(|suffix| sdk_root.join("Lib").join(&version).join(suffix)),
        );
        includes.extend(
            ["ucrt", "shared", "um", "winrt", "cppwinrt"]
                .into_iter()
                .map(|suffix| sdk_root.join("Include").join(&version).join(suffix)),
        );
    }
    if let Ok(value) = std::env::join_paths(libraries.into_iter().filter(|path| path.is_dir())) {
        env.insert("LIB".into(), value.to_string_lossy().into_owned());
    }
    if let Ok(value) = std::env::join_paths(includes.into_iter().filter(|path| path.is_dir())) {
        env.insert("INCLUDE".into(), value.to_string_lossy().into_owned());
    }
}

#[cfg(windows)]
fn newest_directory(parent: &Path) -> Option<OsString> {
    let mut directories: Vec<_> = std::fs::read_dir(parent)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name())
        .collect();
    directories.sort();
    directories.pop()
}

pub fn game_backend_fleet(inputs: &FleetInputs, flavor: FleetFlavor) -> FleetSpec {
    game_backend_fleet_with_environment(inputs, flavor, &EnvironmentSnapshot::capture())
}

pub fn game_backend_fleet_with_environment(
    inputs: &FleetInputs,
    flavor: FleetFlavor,
    environment: &EnvironmentSnapshot,
) -> FleetSpec {
    let db = inputs.database_url.clone();
    let cert = inputs.edge_ca_cert.display().to_string();
    let key = inputs.edge_ca_key.display().to_string();
    let base = || {
        let mut env = environment.runtime_environment();
        env.insert("DATABASE_URL".into(), db.clone());
        env.insert("EDGE_CA_CERT".into(), cert.clone());
        env.insert("EDGE_CA_KEY".into(), key.clone());
        // One field feeds BOTH runtime and invariant: the same cap the fleet charges
        // against PG_SESSION_BUDGET is what the spawned process opens its pool with.
        // Only DB-backed svcs go through `base()`; gateway-svc builds its env separately
        // and never gets this key.
        env.insert(
            "DATABASE_POOL_MAX_CONNECTIONS".into(),
            SPLIT_SERVICE_POOL_MAX.to_string(),
        );
        env
    };
    let service = |name, http_port, edge_port: Option<u16>, dependencies: Vec<&'static str>| {
        let mut env = base();
        env.insert("PORT".into(), format!(":{http_port}"));
        if let Some(port) = edge_port {
            env.insert("EDGE_ADDR".into(), format!(":{port}"));
        }
        ServiceSpec {
            name,
            executable_package: name,
            http_port,
            edge_port,
            player_port: None,
            dependencies,
            env,
            overrideable_env: &[],
            pool_budget: PoolBudget {
                pool_max: SPLIT_SERVICE_POOL_MAX,
                dedicated: PLANE_DEDICATED_SESSIONS,
            },
        }
    };
    let peer = |env: &mut BTreeMap<String, String>, key: &str, port: u16| {
        env.insert(format!("{key}_EDGE_ADDR"), format!("127.0.0.1:{port}"));
    };

    let mut accounts = service("accounts-svc", 8084, Some(9003), vec![]);
    let mut apikeys = service("apikeys-svc", 8091, Some(9009), vec![]);
    let audit = service("audit-svc", 8086, Some(9004), vec![]);
    let mut scheduler = service("scheduler-svc", 8087, Some(9005), vec![]);
    // Beyond the two planes, the scheduler holds one dedicated per-fire connection.
    scheduler.pool_budget.dedicated += SCHEDULER_FIRE_SESSIONS;
    let rating = service("rating-svc", 8089, Some(9007), vec![]);
    let leaderboard = service("leaderboard-svc", 8090, Some(9008), vec![]);
    let mut matches = service("match-svc", 8088, Some(9006), vec!["rating-svc"]);
    peer(&mut matches.env, "RATING", 9007);
    let config = service("config-svc", 8083, Some(9002), vec![]);
    let mut characters = service("characters-svc", 8080, Some(9000), vec!["config-svc"]);
    peer(&mut characters.env, "CONFIG", 9002);
    let mut inventory = service(
        "inventory-svc",
        8081,
        Some(9001),
        vec!["characters-svc", "config-svc"],
    );
    peer(&mut inventory.env, "CHARACTERS", 9000);
    peer(&mut inventory.env, "CONFIG", 9002);

    let mut gateway_env = environment.runtime_environment();
    gateway_env.insert("EDGE_CA_CERT".into(), cert.clone());
    gateway_env.insert("EDGE_CA_KEY".into(), key.clone());
    gateway_env.insert("PORT".into(), ":8082".into());
    gateway_env.insert("PLAYER_EDGE_ADDR".into(), ":9100".into());
    gateway_env.insert("TLS_MODE".into(), "off".into());
    for (name, port) in [
        ("CHARACTERS", 9000),
        ("INVENTORY", 9001),
        ("ACCOUNTS", 9003),
        ("MATCH", 9006),
        ("LEADERBOARD", 9008),
        ("APIKEYS", 9009),
    ] {
        peer(&mut gateway_env, name, port);
    }
    gateway_env.insert("ADMIN_HTTP_ADDR".into(), "127.0.0.1:8085".into());
    gateway_env.insert("ACCOUNTS_HTTP_ADDR".into(), "127.0.0.1:8084".into());
    let gateway = ServiceSpec {
        name: "gateway-svc",
        executable_package: "gateway-svc",
        http_port: 8082,
        edge_port: None,
        player_port: Some(9100),
        dependencies: vec![
            "characters-svc", "inventory-svc", "accounts-svc", "match-svc",
            "leaderboard-svc", "apikeys-svc",
        ],
        env: gateway_env,
        overrideable_env: &[],
        // Pure-transport front door: no DB, no pool, no plane — reserves nothing and
        // gets no DATABASE_POOL_MAX_CONNECTIONS (gateway_env never went through base()).
        pool_budget: PoolBudget { pool_max: 0, dedicated: 0 },
    };

    let mut admin = service(
        "admin-svc",
        8085,
        None,
        vec![
            "characters-svc", "inventory-svc", "config-svc", "accounts-svc", "audit-svc",
            "scheduler-svc", "apikeys-svc",
        ],
    );
    for (name, port) in [
        ("CHARACTERS", 9000),
        ("INVENTORY", 9001),
        ("CONFIG", 9002),
        ("ACCOUNTS", 9003),
        ("AUDIT", 9004),
        ("SCHEDULER", 9005),
        ("APIKEYS", 9009),
    ] {
        peer(&mut admin.env, name, port);
    }
    admin.env.insert("ADMIN_COOKIE_SECURE".into(), "0".into());
    admin
        .env
        .insert("TRUSTED_PROXY_CIDRS".into(), "127.0.0.1/32".into());

    accounts.overrideable_env = &["ACCOUNTS_DEV_AUTH"];
    apikeys.overrideable_env = &["APIKEYS_DEV_SEED"];
    scheduler.overrideable_env = &["SCHEDULER_ENABLED"];
    inventory.overrideable_env = &["INVENTORY_DEV_GRANT"];
    admin.overrideable_env = &["ADMIN_COOKIE_SECURE", "TRUSTED_PROXY_CIDRS"];

    accounts.env.insert("ACCOUNTS_DEV_AUTH".into(), "1".into());
    apikeys.env.insert("APIKEYS_DEV_SEED".into(), "1".into());
    inventory.env.insert("INVENTORY_DEV_GRANT".into(), "1".into());

    for service in [&mut accounts, &mut apikeys, &mut scheduler, &mut inventory, &mut admin] {
        for key in service.overrideable_env {
            if let Some(value) = environment.value(key) {
                service.env.insert((*key).to_string(), value.to_string());
            }
        }
    }

    if flavor == FleetFlavor::Proof {
            accounts.env.insert("ACCOUNTS_DEV_AUTH".into(), "1".into());
            accounts.env.insert("EPIC_CLIENT_ID".into(), "test".into());
            accounts.env.insert("EPIC_CLIENT_SECRET".into(), "test".into());
            accounts.env.insert(
                "EPIC_REDIRECT_URI".into(),
                "http://127.0.0.1:8082/accounts/epic/callback".into(),
            );
            accounts
                .env
                .insert("EPIC_TOKEN_URL".into(), "http://127.0.0.1:1/token".into());
            apikeys.env.insert("APIKEYS_DEV_SEED".into(), "1".into());
            scheduler.env.insert("SCHEDULER_ENABLED".into(), "1".into());
            inventory.env.insert("INVENTORY_DEV_GRANT".into(), "1".into());
    }

    FleetSpec::new(vec![
        accounts, apikeys, audit, scheduler, rating, leaderboard, matches, config, characters,
        inventory, gateway, admin,
    ])
    .expect("the built-in game backend fleet is internally valid")
}

pub fn game_backend_monolith(
    inputs: &FleetInputs,
    flavor: FleetFlavor,
    environment: &EnvironmentSnapshot,
) -> ServiceSpec {
    let mut env = environment.runtime_environment();
    for (key, value) in [
        ("PORT", ":8080".into()),
        ("DATABASE_URL", inputs.database_url.clone()),
        // One process, all modules + both planes — a larger pool than a split peer, and
        // the same value the const budget assertion above charges for the monolith.
        ("DATABASE_POOL_MAX_CONNECTIONS", MONOLITH_POOL_MAX.to_string()),
        ("PLAYER_EDGE_ADDR", ":9100".into()),
        ("EDGE_CA_CERT", inputs.edge_ca_cert.display().to_string()),
        ("EDGE_CA_KEY", inputs.edge_ca_key.display().to_string()),
        ("APIKEYS_DEV_SEED", "1".into()),
        ("ACCOUNTS_DEV_AUTH", "1".into()),
        ("INVENTORY_DEV_GRANT", "1".into()),
        ("TLS_MODE", "off".into()),
        ("ADMIN_COOKIE_SECURE", "0".into()),
        ("TRUSTED_PROXY_CIDRS", "127.0.0.1/32".into()),
    ] { env.insert(key.into(), value); }
    let overrideable_env = &[
        "APIKEYS_DEV_SEED", "ACCOUNTS_DEV_AUTH", "INVENTORY_DEV_GRANT",
        "ADMIN_COOKIE_SECURE", "TRUSTED_PROXY_CIDRS",
    ];
    for key in overrideable_env {
        if let Some(value) = environment.value(key) {
            env.insert((*key).to_string(), value.to_string());
        }
    }
    if flavor == FleetFlavor::Proof {
        // Proof-only overlay is intentionally last and cannot be weakened by ambient state.
        env.insert("ACCOUNTS_DEV_AUTH".into(), "1".into());
        env.insert("APIKEYS_DEV_SEED".into(), "1".into());
        env.insert("INVENTORY_DEV_GRANT".into(), "1".into());
    }
    ServiceSpec {
        name: "monolith", executable_package: "server", http_port: 8080,
        edge_port: None, player_port: Some(9100), dependencies: vec![], env,
        overrideable_env,
        // One process hosts every module + both planes + the scheduler's fire
        // connection. Fits PG_SESSION_BUDGET by the const assertion in this module.
        pool_budget: PoolBudget {
            pool_max: MONOLITH_POOL_MAX,
            dedicated: PLANE_DEDICATED_SESSIONS + SCHEDULER_FIRE_SESSIONS,
        },
    }
}
