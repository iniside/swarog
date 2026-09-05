use std::collections::{BTreeMap, BTreeSet};
#[cfg(windows)]
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;

pub const BUILD_ENV_ALLOWLIST: &[&str] = &[
    "ALL_PROXY", "APPDATA", "CARGO_HOME", "CARGO_HTTP_CAINFO", "CARGO_HTTP_PROXY",
    "CARGO_NET_GIT_FETCH_WITH_CLI", "CARGO_TARGET_DIR", "COMSPEC", "GIT_SSL_CAINFO",
    "HOME", "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "PATH", "PATHEXT", "RUSTFLAGS",
    "ProgramFiles(x86)", "RUSTUP_HOME", "SSL_CERT_DIR", "SSL_CERT_FILE", "SYSTEMROOT", "TEMP",
    "TMP", "TMPDIR", "USERPROFILE", "WINDIR", "all_proxy", "http_proxy", "https_proxy", "no_proxy",
];

pub const SERVICE_ENV_ALLOWLIST: &[&str] = &[
    "COMSPEC", "HOME", "PATH", "PATHEXT", "RUST_BACKTRACE", "RUST_LOG", "SYSTEMROOT",
    "TEMP", "TMP", "TMPDIR", "USERPROFILE", "WINDIR",
];

/// The accounts credential-provider configuration an operator may hand a fleet.
/// `runtime_environment` is allowlist-filtered, so a value absent from this list is a
/// value `EPIC_*`/`GOOGLE_*` in the operator's shell can never reach accounts with.
const ACCOUNTS_OVERRIDEABLE_ENV: &[&str] = &[
    "ACCOUNTS_DEV_AUTH",
    "EPIC_CLIENT_ID", "EPIC_JWKS_URL", "EPIC_ISSUER_PREFIX", "EPIC_CLIENT_SECRET",
    "EPIC_REDIRECT_URI", "EPIC_AUTHORIZE_URL", "EPIC_TOKEN_URL",
    "GOOGLE_CLIENT_IDS", "GOOGLE_JWKS_URL",
];

/// Every `MAIL_*` key an operator's shell may reach `mail-svc` with — the SOLE source
/// list for both the split `mail-svc` and (composed below into
/// [`MONOLITH_OVERRIDEABLE_ENV`]) the monolith, so a new mail knob added here reaches
/// both topologies by construction rather than by remembering to edit two lists.
const MAIL_OVERRIDEABLE_ENV: &[&str; 10] = &[
    "MAIL_PROVIDER", "MAIL_FROM", "MAIL_SEND_TIMEOUT_MS", "MAIL_MAX_ATTEMPTS",
    "MAIL_SMTP_HOST", "MAIL_SMTP_PORT", "MAIL_SMTP_USERNAME", "MAIL_SMTP_PASSWORD",
    "MAIL_SMTP_TLS", "MAIL_RETENTION_DAYS",
];

/// The monolith's twin of [`ACCOUNTS_OVERRIDEABLE_ENV`] plus the process-wide knobs
/// other than mail's — one process hosts accounts, so the provider keys are read from
/// the same env map. [`MONOLITH_OVERRIDEABLE_ENV`] appends [`MAIL_OVERRIDEABLE_ENV`] to
/// this at compile time, so the mail keys themselves are written out exactly once.
const MONOLITH_BASE_ENV: &[&str; 16] = &[
    "APIKEYS_DEV_SEED", "ACCOUNTS_DEV_AUTH", "INVENTORY_DEV_GRANT", "WALLET_DEV_SEED",
    "ADMIN_COOKIE_SECURE", "TRUSTED_PROXY_CIDRS", "NOTIFICATIONS_RETENTION_DAYS",
    "EPIC_CLIENT_ID", "EPIC_JWKS_URL", "EPIC_ISSUER_PREFIX", "EPIC_CLIENT_SECRET",
    "EPIC_REDIRECT_URI", "EPIC_AUTHORIZE_URL", "EPIC_TOKEN_URL",
    "GOOGLE_CLIENT_IDS", "GOOGLE_JWKS_URL",
];

/// Concatenates [`MONOLITH_BASE_ENV`] (16 keys) and [`MAIL_OVERRIDEABLE_ENV`] (10 keys)
/// at compile time into [`MONOLITH_OVERRIDEABLE_ENV_ARR`] — const generics cannot
/// express `N + M` as an array length on stable Rust (no `generic_const_exprs`), so this
/// is monomorphic to the two callers' concrete sizes rather than generic; a length
/// mismatch at either input is a compile error here, never a silent truncation.
const fn concat_env_16_10(a: &[&'static str; 16], b: &[&'static str; 10]) -> [&'static str; 26] {
    let mut out = [""; 26];
    let mut i = 0;
    while i < 16 {
        out[i] = a[i];
        i += 1;
    }
    let mut j = 0;
    while j < 10 {
        out[16 + j] = b[j];
        j += 1;
    }
    out
}

const MONOLITH_OVERRIDEABLE_ENV_ARR: [&str; 26] = concat_env_16_10(MONOLITH_BASE_ENV, MAIL_OVERRIDEABLE_ENV);
const MONOLITH_OVERRIDEABLE_ENV: &[&str] = &MONOLITH_OVERRIDEABLE_ENV_ARR;

/// Provider keys the `Proof` overlay CLEARS before pinning its own. `google` is the
/// split-proof fleet's deliberately unconfigured provider: it is the only way to execute
/// the `KnownButUnconfigured` 503 arm that separates "no such provider" (400) from "not
/// configured here" (503), so an ambient `GOOGLE_CLIENT_IDS` would silently delete an
/// assertion rather than fail one.
const PROOF_UNCONFIGURED_PROVIDER_ENV: &[&str] = &["GOOGLE_CLIENT_IDS", "GOOGLE_JWKS_URL"];

/// The `MAIL_SMTP_*` group the `Proof` overlay CLEARS from `mail`'s OWN env (never
/// `accounts.env` — a distinct list from [`PROOF_UNCONFIGURED_PROVIDER_ENV`], whose
/// removal loop only touches `accounts.env`, so reusing that const here would silently
/// clear nothing). Proof always pins `MAIL_PROVIDER=log` afterward, so this is not about
/// exercising an unconfigured-provider arm: it is about never letting an operator's
/// ambient SMTP credentials (developing against a real relay) survive into the
/// verification fleet — a partial ambient `MAIL_SMTP_*` group would otherwise either dial
/// a live relay under `smtp` or, once re-pinned to `log`, fail startup outright (`mail`'s
/// config refuses any `MAIL_SMTP_*` value set while the provider is not `smtp`).
const PROOF_CLEARED_MAIL_SMTP_ENV: &[&str] = &[
    "MAIL_SMTP_HOST", "MAIL_SMTP_PORT", "MAIL_SMTP_USERNAME", "MAIL_SMTP_PASSWORD", "MAIL_SMTP_TLS",
];

/// The loopback OIDC fixture `tools/splitproof` stands up so a FEDERATED credential is
/// mintable without a live identity provider, and the `epic` configuration the `Proof`
/// flavor points at it. Declared here because the fleet env and the harness's signer must
/// agree; the origin is written ONCE and the JWKS url and the bind port are both derived
/// from it, so no edit can point the fleet and the fixture at different places.
macro_rules! proof_oidc_origin {
    () => {
        "http://127.0.0.1:8099"
    };
}

pub const PROOF_OIDC_ISSUER: &str = proof_oidc_origin!();
pub const PROOF_OIDC_JWKS_URL: &str = concat!(proof_oidc_origin!(), "/jwks");
pub const PROOF_OIDC_CLIENT_ID: &str = "splitproof-epic-client";

/// The provider every fleet flavor configures `mail` with, and the value
/// `tools/splitproof` asserts a delivered row was sent BY. The harness reads this
/// const rather than repeating the string, so re-pointing a fleet at another provider
/// cannot leave the proof asserting a value nothing writes. `log` renders the message
/// to the service log and never dials a relay — a verification run must not send real
/// outbound mail.
pub const PROOF_MAIL_PROVIDER: &str = "log";

/// The envelope sender the fleet pins alongside [`PROOF_MAIL_PROVIDER`]. `mail` FAILS
/// STARTUP on a provider without a from-address, so the pair is written together.
pub const PROOF_MAIL_FROM: &str = "dev@localhost";

/// The port [`PROOF_OIDC_ISSUER`] names, for the harness to bind.
pub fn proof_oidc_port() -> u16 {
    PROOF_OIDC_ISSUER
        .rsplit(':')
        .next()
        .and_then(|port| port.parse().ok())
        .expect("PROOF_OIDC_ISSUER carries an explicit port")
}

/// Cap on splitproof's own sqlx assertion pool, consumed BY the harness
/// (`tools/splitproof`) so this reserve line is an enforced bound rather than a guess:
/// sqlx's default cap is 10, which would silently outgrow the itemized estimate below.
pub const SPLITPROOF_ASSERTION_POOL_MAX: u32 = 4;

/// Sessions held by splitproof's `[REPLICAS]` phase, which runs a SECOND
/// leaderboard-svc — cloned from the canonical spec, so it reserves exactly what any
/// DB-backed split service does — alongside the whole fleet. It is deliberately not a
/// fleet member (that would trip the fleet-drift preflight), so the budget must charge
/// it here or the real peak is 15 DB-backed processes against a 14-process model.
pub const SPLITPROOF_REPLICA_SESSIONS: u32 = SPLIT_SERVICE_POOL_MAX + PLANE_DEDICATED_SESSIONS;

/// Sessions the local Postgres reserves for dev tooling running ALONGSIDE the fleet,
/// carved out of the usable budget before the processes get any. This is a HEURISTIC
/// reserve — an itemized estimate, deliberately not derivation machinery. The named
/// breakdown (add a new always-on consumer by item, never by nudging a bare number):
///
/// | item                                                     | sessions |
/// |----------------------------------------------------------|----------|
/// | splitproof's own sqlx assertion pool                     | 4 (= [`SPLITPROOF_ASSERTION_POOL_MAX`]) |
/// | splitproof's `[REPLICAS]` second leaderboard-svc         | 6 (= [`SPLITPROOF_REPLICA_SESSIONS`]) |
/// | devctl psql seeding / adminctl                           | 1        |
/// | eventctl ad-hoc operator session                         | 1        |
/// | asyncevents poison-recovery burst                        | 2 (= [`AE_TRANSIENT_POISON_SESSIONS`]) |
/// | slack                                                    | 2        |
/// | **total**                                                | **16**   |
///
/// The assertion-pool, replica and poison-burst terms are the consts the real mechanisms
/// are built from, so those lines track behavior; the other items are hand-estimated.
pub const HARNESS_RESERVE: u32 = SPLITPROOF_ASSERTION_POOL_MAX
    + SPLITPROOF_REPLICA_SESSIONS
    + 1
    + 1
    + AE_TRANSIENT_POISON_SESSIONS
    + 2;

/// Sessions a stock cluster withholds from ordinary roles
/// (`superuser_reserved_connections`, verified against the local cluster 2026-07-29).
/// It sizes the RECOMMENDED provisioning below; it is never assumed of a live cluster,
/// which reports its own reserved settings to [`PgSessionCapacity`].
pub(crate) const SUPERUSER_RESERVED_CONNECTIONS: u32 = 3;

/// The `max_connections` this repo asks an operator to provision — enough for
/// [`USABLE_PG_SESSIONS`] beside a stock reservation. Nothing in Postgres enforces
/// it, so [`require_pg_session_floor`] asks the live cluster what it actually offers
/// before a rollout spawns anything.
pub const REQUIRED_MAX_CONNECTIONS: u32 = 150;

/// Postgres sessions the FULL split fleet plus [`HARNESS_RESERVE`] is derived within.
/// A rollout is charged its OWN reservation rather than this number, so a smaller
/// topology runs on a smaller cluster.
pub(crate) const USABLE_PG_SESSIONS: u32 =
    REQUIRED_MAX_CONNECTIONS - SUPERUSER_RESERVED_CONNECTIONS;

/// Usable Postgres sessions the whole fleet + monolith must fit within.
/// [`HARNESS_RESERVE`] is subtracted so the fleet is charged only its own share.
pub const PG_SESSION_BUDGET: u32 = USABLE_PG_SESSIONS - HARNESS_RESERVE;

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
pub const PLANE_DEDICATED_SESSIONS: u32 =
    AE_WORKERS + AE_WAKEUP_SESSIONS + INVALIDATION_LISTEN_SESSIONS;

/// Per-DB-process pooled-connection cap in the SPLIT: every DB-backed process plus
/// splitproof's `[REPLICAS]` extra one share a single local Postgres. It sits exactly AT
/// core/app's migrate floor (`MIN_DB_POOL_MAX = 2`) — the smallest cap the two-phase
/// migrate can run with, which is sufficient because boot is sequential: the two-phase
/// migrate holds the schema-lock connection plus at most ONE module connection, and HTTP
/// serves only after `start`. The pool's concurrent users — the retention GC sweep, the
/// metrics/invalidation poll refreshes, the `/readyz` DB ping, the HTTP/edge handlers —
/// all wait on the pool rather than erroring, so an undersized pool costs acquire-wait
/// LATENCY, never correctness. A module whose `migrate` needed two connections at once
/// would need this raised (and the budget re-derived), not a local workaround.
const SPLIT_SERVICE_POOL_MAX: u32 = 2;

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

impl PoolBudget {
    /// Sessions this process holds against the one local Postgres.
    pub fn sessions(self) -> u32 {
        self.pool_max + self.dedicated
    }
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
        // LIB and INCLUDE are synthesized from the locally discovered toolchain.
        // They are not inherited authorities and therefore are not allowlist entries.
        // Only the Windows arm mutates `env`, so the binding is `mut` only there.
        #[cfg(windows)]
        let mut env = self.filtered(BUILD_ENV_ALLOWLIST);
        #[cfg(not(windows))]
        let env = self.filtered(BUILD_ENV_ALLOWLIST);
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
    #[error("{}", pg_session_floor_message(*.capacity, *.required))]
    PgSessionFloor {
        capacity: PgSessionCapacity,
        required: u32,
    },
    #[error("read the Postgres session settings from the configured DATABASE_URL: {0}")]
    PgSessionProbe(String),
}

/// What a live cluster actually offers, as the cluster itself reports it — never
/// assumed from [`SUPERUSER_RESERVED_CONNECTIONS`], which an operator is free to raise
/// alongside `max_connections` and thereby hand the fleet fewer sessions than the same
/// `max_connections` implied here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PgSessionCapacity {
    pub max_connections: u32,
    /// `superuser_reserved_connections` plus PostgreSQL 16+'s `reserved_connections`
    /// (the reservation for `pg_use_reserved_connections`), both withheld from the
    /// fleet's ordinary role.
    pub reserved: u32,
}

impl PgSessionCapacity {
    /// Sessions an ordinary role can actually open.
    pub fn usable(self) -> u32 {
        self.max_connections.saturating_sub(self.reserved)
    }
}

/// The DSN every rollout tool falls back to when `DATABASE_URL` is unset — and the one
/// `core/app` itself defaults to, so a fleet spawned without the variable connects HERE.
/// A preflight that skipped the probe on a missing `DATABASE_URL` would be probing the
/// one case where the fleet still opens every session it reserved.
pub const DEFAULT_DATABASE_URL: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// How long the probe waits for the cluster to answer before the rollout is refused.
const PG_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Refuses the rollout unless the cluster at `database_url` offers `required` sessions
/// to ordinary roles. Blocking (it owns a current-thread runtime) because the
/// supervisors that boot a fleet are synchronous; an async caller reads the same three
/// settings itself and applies [`check_pg_session_floor`] to the answer.
///
/// `required` is the CALLER's own reservation — the fleet it is about to spawn, plus
/// [`HARNESS_RESERVE`] where the harness runs alongside it — so a monolith rollout is
/// not refused for a split fleet's needs.
///
/// Call it BEFORE spawning anything: a cluster too small for the rollout fails here,
/// loudly, instead of exhausting connections part-way through a boot.
pub fn require_pg_session_floor(database_url: &str, required: u32) -> Result<(), FleetError> {
    check_pg_session_floor(read_pg_session_capacity(database_url)?, required)
}

/// The verdict over an already-observed capacity, with no I/O of its own, so the
/// blocking and async callers reach one conclusion carrying one remedy.
pub fn check_pg_session_floor(
    capacity: PgSessionCapacity,
    required: u32,
) -> Result<(), FleetError> {
    if capacity.usable() >= required {
        return Ok(());
    }
    Err(FleetError::PgSessionFloor { capacity, required })
}

/// The remedy names the recommended provisioning, or more where the operator's own
/// reservations put [`REQUIRED_MAX_CONNECTIONS`] out of reach of this rollout.
fn pg_session_floor_message(capacity: PgSessionCapacity, required: u32) -> String {
    let suggested = REQUIRED_MAX_CONNECTIONS.max(required + capacity.reserved);
    format!(
        "Postgres offers {usable} sessions to ordinary roles (max_connections {max}, \
         {reserved} reserved), below the {required} this rollout reserves. Raise the \
         cluster:\n    ALTER SYSTEM SET max_connections = {suggested};\nthen RESTART the \
         Postgres server — max_connections is postmaster-context, so pg_reload_conf() does \
         NOT apply it.",
        usable = capacity.usable(),
        max = capacity.max_connections,
        reserved = capacity.reserved,
    )
}

/// Reads `max_connections` and BOTH reservation settings in one round-trip.
/// `reserved_connections` exists only from PostgreSQL 16, so it is read through the
/// missing-ok form and counts as zero on an older cluster.
pub fn read_pg_session_capacity(database_url: &str) -> Result<PgSessionCapacity, FleetError> {
    use sqlx::Connection as _;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| FleetError::PgSessionProbe(source.to_string()))?;
    let (max_connections, superuser_reserved, reserved) = runtime.block_on(async {
        tokio::time::timeout(PG_PROBE_TIMEOUT, async {
            let mut connection = sqlx::PgConnection::connect(database_url).await?;
            let row: (i32, i32, i32) = sqlx::query_as(PG_SESSION_CAPACITY_SQL)
                .fetch_one(&mut connection)
                .await?;
            connection.close().await?;
            Ok::<(i32, i32, i32), sqlx::Error>(row)
        })
        .await
        .map_err(|_| FleetError::PgSessionProbe(format!("no answer within {PG_PROBE_TIMEOUT:?}")))?
        .map_err(|source| FleetError::PgSessionProbe(source.to_string()))
    })?;
    Ok(PgSessionCapacity {
        max_connections: max_connections.max(0) as u32,
        reserved: (superuser_reserved.max(0) + reserved.max(0)) as u32,
    })
}

/// Shared with the async twin in `tools/splitproof`, which reads the same three
/// settings over its own connection.
pub const PG_SESSION_CAPACITY_SQL: &str = "SELECT current_setting('max_connections')::int, \
     current_setting('superuser_reserved_connections')::int, \
     coalesce(current_setting('reserved_connections', true)::int, 0)";

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
            .map(|service| service.pool_budget.sessions())
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

    /// The Postgres sessions this fleet reserves — the same sum [`FleetSpec::new`]
    /// charges against [`PG_SESSION_BUDGET`], so what a caller preflights against a
    /// live cluster and what the budget invariant admits are one number. A caller that
    /// also runs the harness adds [`HARNESS_RESERVE`].
    pub fn pg_session_reservation(&self) -> u32 {
        self.services
            .iter()
            .map(|service| service.pool_budget.sessions())
            .sum()
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

/// The front door's internal-edge port. It serves ONE inbound face there — the push
/// backplane's `push.deliver` — so a producer process can reach the WebSocket connections
/// it owns. 9013 continues the split's 9000-block; 9012 is mail-svc's.
const GATEWAY_EDGE_PORT: u16 = 9013;

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
    // The `config-svc` dependency is real, not cosmetic: wallet's starter grant reads its
    // knobs through the injected `dyn Config`, and the split's `CachedConfig` is
    // boot-fill-or-fail-startup — wallet-svc cannot come up before config-svc.
    let mut wallet = service("wallet-svc", 8092, Some(9010), vec!["config-svc"]);
    peer(&mut wallet.env, "CONFIG", 9002);
    let mut notifications = service("notifications-svc", 8093, Some(9011), vec![]);
    // The push backplane's one address: notifications-svc fans `ctx.push()` out to the
    // front's `push.deliver`. NOT a fleet dependency — gateway-svc already depends on
    // notifications-svc, and push is best-effort, so this process must start and run with
    // no front door present.
    peer(&mut notifications.env, "GATEWAY", GATEWAY_EDGE_PORT);
    let mut mail = service("mail-svc", 8094, Some(9012), vec![]);

    let mut gateway_env = environment.runtime_environment();
    gateway_env.insert("EDGE_CA_CERT".into(), cert.clone());
    gateway_env.insert("EDGE_CA_KEY".into(), key.clone());
    gateway_env.insert("PORT".into(), ":8082".into());
    // Explicit because this spec is built inline and never goes through `service()`, which
    // is where every other svc gets its `EDGE_ADDR`. Without it `app::run` falls back to
    // its `:9000` default — characters-svc's port.
    gateway_env.insert("EDGE_ADDR".into(), format!(":{GATEWAY_EDGE_PORT}"));
    gateway_env.insert("PLAYER_EDGE_ADDR".into(), ":9100".into());
    gateway_env.insert("TLS_MODE".into(), "off".into());
    for (name, port) in [
        ("CHARACTERS", 9000),
        ("INVENTORY", 9001),
        ("ACCOUNTS", 9003),
        ("MATCH", 9006),
        ("LEADERBOARD", 9008),
        ("APIKEYS", 9009),
        ("WALLET", 9010),
        ("NOTIFICATIONS", 9011),
    ] {
        peer(&mut gateway_env, name, port);
    }
    gateway_env.insert("ADMIN_HTTP_ADDR".into(), "127.0.0.1:8085".into());
    gateway_env.insert("ACCOUNTS_HTTP_ADDR".into(), "127.0.0.1:8084".into());
    let gateway = ServiceSpec {
        name: "gateway-svc",
        executable_package: "gateway-svc",
        http_port: 8082,
        edge_port: Some(GATEWAY_EDGE_PORT),
        player_port: Some(9100),
        dependencies: vec![
            "characters-svc", "inventory-svc", "accounts-svc", "match-svc",
            "leaderboard-svc", "apikeys-svc", "wallet-svc", "notifications-svc",
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
            "scheduler-svc", "apikeys-svc", "wallet-svc", "notifications-svc", "mail-svc",
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
        ("WALLET", 9010),
        ("NOTIFICATIONS", 9011),
        ("MAIL", 9012),
    ] {
        peer(&mut admin.env, name, port);
    }
    admin.env.insert("ADMIN_COOKIE_SECURE".into(), "0".into());
    admin
        .env
        .insert("TRUSTED_PROXY_CIDRS".into(), "127.0.0.1/32".into());

    accounts.overrideable_env = ACCOUNTS_OVERRIDEABLE_ENV;
    apikeys.overrideable_env = &["APIKEYS_DEV_SEED"];
    scheduler.overrideable_env = &["SCHEDULER_ENABLED"];
    inventory.overrideable_env = &["INVENTORY_DEV_GRANT"];
    admin.overrideable_env = &["ADMIN_COOKIE_SECURE", "TRUSTED_PROXY_CIDRS"];
    wallet.overrideable_env = &["WALLET_DEV_SEED"];
    notifications.overrideable_env = &["NOTIFICATIONS_RETENTION_DAYS"];
    mail.overrideable_env = MAIL_OVERRIDEABLE_ENV;

    accounts.env.insert("ACCOUNTS_DEV_AUTH".into(), "1".into());
    apikeys.env.insert("APIKEYS_DEV_SEED".into(), "1".into());
    inventory.env.insert("INVENTORY_DEV_GRANT".into(), "1".into());
    wallet.env.insert("WALLET_DEV_SEED".into(), "1".into());
    // Without these the channel is UNDRAINED (`MailConfig::provider == None`) and
    // `/readyz` is red by design — the no-provider readiness check is permanent.
    mail.env.insert("MAIL_PROVIDER".into(), PROOF_MAIL_PROVIDER.into());
    mail.env.insert("MAIL_FROM".into(), PROOF_MAIL_FROM.into());

    for service in
        [&mut accounts, &mut apikeys, &mut scheduler, &mut inventory, &mut admin, &mut wallet,
         &mut notifications, &mut mail]
    {
        for key in service.overrideable_env {
            if let Some(value) = environment.value(key) {
                service.env.insert((*key).to_string(), value.to_string());
            }
        }
    }

    if flavor == FleetFlavor::Proof {
            for key in PROOF_UNCONFIGURED_PROVIDER_ENV {
                accounts.env.remove(*key);
            }
            accounts.env.insert("ACCOUNTS_DEV_AUTH".into(), "1".into());
            accounts
                .env
                .insert("EPIC_CLIENT_ID".into(), PROOF_OIDC_CLIENT_ID.into());
            accounts
                .env
                .insert("EPIC_JWKS_URL".into(), PROOF_OIDC_JWKS_URL.into());
            accounts
                .env
                .insert("EPIC_ISSUER_PREFIX".into(), PROOF_OIDC_ISSUER.into());
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
            wallet.env.insert("WALLET_DEV_SEED".into(), "1".into());
            for key in PROOF_CLEARED_MAIL_SMTP_ENV {
                mail.env.remove(*key);
            }
            mail.env.insert("MAIL_PROVIDER".into(), PROOF_MAIL_PROVIDER.into());
            mail.env.insert("MAIL_FROM".into(), PROOF_MAIL_FROM.into());
    }

    FleetSpec::new(vec![
        accounts, apikeys, audit, scheduler, rating, leaderboard, matches, config, characters,
        inventory, wallet, notifications, mail, gateway, admin,
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
        ("WALLET_DEV_SEED", "1".into()),
        ("TLS_MODE", "off".into()),
        ("ADMIN_COOKIE_SECURE", "0".into()),
        ("TRUSTED_PROXY_CIDRS", "127.0.0.1/32".into()),
        // Without these the channel is UNDRAINED (`MailConfig::provider == None`) and
        // `/readyz` is red by design — the no-provider readiness check is permanent.
        ("MAIL_PROVIDER", PROOF_MAIL_PROVIDER.into()),
        ("MAIL_FROM", PROOF_MAIL_FROM.into()),
    ] { env.insert(key.into(), value); }
    let overrideable_env = MONOLITH_OVERRIDEABLE_ENV;
    for key in overrideable_env {
        if let Some(value) = environment.value(key) {
            env.insert((*key).to_string(), value.to_string());
        }
    }
    if flavor == FleetFlavor::Proof {
        // Proof-only overlay is intentionally last and cannot be weakened by ambient state.
        for key in PROOF_UNCONFIGURED_PROVIDER_ENV {
            env.remove(*key);
        }
        for key in PROOF_CLEARED_MAIL_SMTP_ENV {
            env.remove(*key);
        }
        env.insert("ACCOUNTS_DEV_AUTH".into(), "1".into());
        env.insert("EPIC_CLIENT_ID".into(), PROOF_OIDC_CLIENT_ID.into());
        env.insert("EPIC_JWKS_URL".into(), PROOF_OIDC_JWKS_URL.into());
        env.insert("EPIC_ISSUER_PREFIX".into(), PROOF_OIDC_ISSUER.into());
        env.insert("APIKEYS_DEV_SEED".into(), "1".into());
        env.insert("INVENTORY_DEV_GRANT".into(), "1".into());
        env.insert("WALLET_DEV_SEED".into(), "1".into());
        env.insert("MAIL_PROVIDER".into(), PROOF_MAIL_PROVIDER.into());
        env.insert("MAIL_FROM".into(), PROOF_MAIL_FROM.into());
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
