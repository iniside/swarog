//! Prep pipeline: decides HOW the fleet gets buildable/spawnable — binary
//! paths, filtered build/seed env, and the transient helper runs (`cargo
//! build`, `edgeca`, `adminctl create-user`) that must succeed before any
//! long-lived service is spawned. Consumes [`crate::manifest`] (the WHAT —
//! process names/ports/env) and [`crate::platform::spawn`] (the ONLY spawn
//! mechanism in this crate — see the crate-wide invariant documented beside
//! `SPAWN_LOCK` in `platform::mod`). Never `std::process::Command` directly:
//! the Windows spawn path uses blanket handle inheritance with no
//! `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` allow-list, so a `std::process::Command`
//! spawn racing a concurrent `platform::spawn` could cross-inherit the other's
//! transient inheritable stdio duplicates.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::platform::{self, SpawnSpec};

/// Parent-process env vars a `cargo build` child may inherit — a superset of
/// [`crate::manifest::SERVICE_ENV_ALLOWLIST`] covering toolchain/proxy
/// plumbing a build (but not a running service) needs. Ported verbatim from
/// `tools/processctl/src/fleet.rs::BUILD_ENV_ALLOWLIST` (that list is itself
/// the canonical source — copied, not imported, per weles's zero-sharing
/// rule).
pub const BUILD_ENV_ALLOWLIST: &[&str] = &[
    "ALL_PROXY",
    "APPDATA",
    "CARGO_HOME",
    "CARGO_HTTP_CAINFO",
    "CARGO_HTTP_PROXY",
    "CARGO_NET_GIT_FETCH_WITH_CLI",
    "CARGO_TARGET_DIR",
    "COMSPEC",
    "GIT_SSL_CAINFO",
    "HOME",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "PATH",
    "PATHEXT",
    "RUSTFLAGS",
    "ProgramFiles(x86)",
    "RUSTUP_HOME",
    "SSL_CERT_DIR",
    "SSL_CERT_FILE",
    "SYSTEMROOT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "WINDIR",
    "all_proxy",
    "http_proxy",
    "https_proxy",
    "no_proxy",
];

/// Dev-mode Postgres DSN — matches `tools/devctl/src/supervisor.rs::DEFAULT_DB`
/// exactly (the same local dev role/db/password `devctl` uses).
const DEFAULT_DATABASE_URL: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

const BUILD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const HELPER_SHUTDOWN_GRACE: Duration = Duration::from_secs(0);
const HELPER_SHUTDOWN_FORCE: Duration = Duration::from_secs(5);
const MINT_CA_TIMEOUT: Duration = Duration::from_secs(30);
const SEED_ADMIN_TIMEOUT: Duration = Duration::from_secs(30);

/// The workspace's on-disk layout as weles cares about it: the repo root, its
/// own `run/weles` scratch dir (created on discovery), and the `target/debug`
/// directory holding every `cargo build`-produced binary.
pub struct Layout {
    pub root: PathBuf,
    pub run_dir: PathBuf,
    pub target_debug: PathBuf,
}

impl Layout {
    /// Discovers the layout under `root`, creating `root/run/weles` if
    /// absent. `target_debug` honors `CARGO_TARGET_DIR` (absolute, or
    /// resolved relative to `root`) exactly like Cargo itself does; absent,
    /// it falls back to `root/target`. NOTE: resolving a RELATIVE
    /// `CARGO_TARGET_DIR` against `root` here is only correct because
    /// [`build`] pins the `cargo build` child's cwd to `layout.root` — Cargo
    /// resolves a relative `CARGO_TARGET_DIR` against its invocation cwd.
    pub fn discover(root: PathBuf) -> Result<Self> {
        let run_dir = root.join("run").join("weles");
        std::fs::create_dir_all(&run_dir)
            .with_context(|| format!("create run dir {}", run_dir.display()))?;

        let target_root = match std::env::var_os("CARGO_TARGET_DIR") {
            Some(value) => {
                let dir = PathBuf::from(value);
                if dir.is_absolute() {
                    dir
                } else {
                    root.join(dir)
                }
            }
            None => root.join("target"),
        };
        let target_debug = target_root.join("debug");

        Ok(Layout {
            root,
            run_dir,
            target_debug,
        })
    }

    /// Path to a built binary for cargo package `pkg` (`<pkg>[.exe]` under
    /// `target_debug`).
    pub fn binary(&self, pkg: &str) -> PathBuf {
        self.target_debug
            .join(format!("{pkg}{}", std::env::consts::EXE_SUFFIX))
    }
}

/// Resolves `database_url()` — `DATABASE_URL` env if set, else the same dev
/// default `devctl` uses (`tools/devctl/src/supervisor.rs::DEFAULT_DB`).
pub fn database_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string())
}

/// Runs `cargo build -p <pkg>...` (one spawn, every package at once) via
/// [`platform::spawn`] with the [`BUILD_ENV_ALLOWLIST`]-filtered environment.
/// Logs go to `run_dir/build.{out,err}.log`. Waits up to 10 minutes
/// (poll `try_wait` every 100ms); on timeout the build is shut down
/// (0s grace, 5s force) and an error is returned; a nonzero exit is also an
/// error, both naming the log paths for the operator to inspect.
pub fn build(layout: &Layout, packages: &[&str]) -> Result<()> {
    let cargo = resolve_on_path("cargo").context("resolve cargo on PATH")?;

    let mut args: Vec<OsString> = vec![OsString::from("build")];
    for package in packages {
        args.push(OsString::from("-p"));
        args.push(OsString::from(*package));
    }

    let env = filtered_env(BUILD_ENV_ALLOWLIST);

    let out_path = layout.run_dir.join("build.out.log");
    let err_path = layout.run_dir.join("build.err.log");
    let stdout = File::create(&out_path)
        .with_context(|| format!("create {}", out_path.display()))?;
    let stderr = File::create(&err_path)
        .with_context(|| format!("create {}", err_path.display()))?;

    let mut proc = platform::spawn(SpawnSpec {
        program: cargo,
        args,
        env,
        cwd: Some(layout.root.clone()),
        stdout: Some(stdout),
        stderr: Some(stderr),
    })
    .context("spawn cargo build")?;

    match wait_for_helper(&mut proc, BUILD_TIMEOUT)? {
        Some(status) if status.success() => Ok(()),
        Some(status) => bail!(
            "cargo build exited with status {:?} — see {} / {}",
            status.code(),
            out_path.display(),
            err_path.display()
        ),
        None => Err(helper_timeout_failure(
            &mut proc,
            "cargo build",
            BUILD_TIMEOUT,
            &out_path,
            &err_path,
        )),
    }
}

/// The internal mTLS CA cert/key pair minted for the fleet.
pub struct CaPaths {
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// Mints the fleet's internal-edge CA material at `run/weles/edge-ca.{crt,key}`
/// via the built `edgeca` binary, unless BOTH files already exist (idempotent
/// re-up — a second `weles up` must not rotate the CA under a running fleet).
/// 30s deadline; logs to `run_dir/edgeca.{out,err}.log`; verifies both files
/// exist after the helper exits successfully.
pub fn mint_ca(layout: &Layout) -> Result<CaPaths> {
    let cert = layout.run_dir.join("edge-ca.crt");
    let key = layout.run_dir.join("edge-ca.key");

    if cert.is_file() && key.is_file() {
        return Ok(CaPaths { cert, key });
    }

    let edgeca = layout.binary("edgeca");
    let args: Vec<OsString> = vec![
        OsString::from("--cert"),
        cert.clone().into_os_string(),
        OsString::from("--key"),
        key.clone().into_os_string(),
    ];

    let out_path = layout.run_dir.join("edgeca.out.log");
    let err_path = layout.run_dir.join("edgeca.err.log");
    let stdout = File::create(&out_path)
        .with_context(|| format!("create {}", out_path.display()))?;
    let stderr = File::create(&err_path)
        .with_context(|| format!("create {}", err_path.display()))?;

    let mut proc = platform::spawn(SpawnSpec {
        program: edgeca,
        args,
        env: filtered_env(crate::manifest::SERVICE_ENV_ALLOWLIST),
        cwd: Some(layout.root.clone()),
        stdout: Some(stdout),
        stderr: Some(stderr),
    })
    .context("spawn edgeca")?;

    match wait_for_helper(&mut proc, MINT_CA_TIMEOUT)? {
        Some(status) if status.success() => {}
        Some(status) => bail!(
            "edgeca exited with status {:?} — see {} / {}",
            status.code(),
            out_path.display(),
            err_path.display()
        ),
        None => {
            return Err(helper_timeout_failure(
                &mut proc,
                "edgeca",
                MINT_CA_TIMEOUT,
                &out_path,
                &err_path,
            ))
        }
    }

    if !cert.is_file() || !key.is_file() {
        bail!(
            "edgeca reported success but did not produce both {} and {}",
            cert.display(),
            key.display()
        );
    }

    Ok(CaPaths { cert, key })
}

/// Seeds (or password-resets) the dev admin login `admin`/`admin` via the
/// built `adminctl` binary. Password crosses ONLY via `ADMINCTL_PASSWORD` env
/// — never argv (house rule). 30s deadline; logs to
/// `run_dir/adminctl.{out,err}.log`; a nonzero exit is an error naming the
/// log paths.
pub fn seed_admin(layout: &Layout, database_url: &str) -> Result<()> {
    let adminctl = layout.binary("adminctl");
    let args: Vec<OsString> = vec![OsString::from("create-user"), OsString::from("admin")];

    let mut env = filtered_env(crate::manifest::SERVICE_ENV_ALLOWLIST);
    env.insert(OsString::from("DATABASE_URL"), OsString::from(database_url));
    env.insert(OsString::from("ADMINCTL_PASSWORD"), OsString::from("admin"));

    let out_path = layout.run_dir.join("adminctl.out.log");
    let err_path = layout.run_dir.join("adminctl.err.log");
    let stdout = File::create(&out_path)
        .with_context(|| format!("create {}", out_path.display()))?;
    let stderr = File::create(&err_path)
        .with_context(|| format!("create {}", err_path.display()))?;

    let mut proc = platform::spawn(SpawnSpec {
        program: adminctl,
        args,
        env,
        cwd: Some(layout.root.clone()),
        stdout: Some(stdout),
        stderr: Some(stderr),
    })
    .context("spawn adminctl create-user")?;

    match wait_for_helper(&mut proc, SEED_ADMIN_TIMEOUT)? {
        Some(status) if status.success() => Ok(()),
        Some(status) => bail!(
            "adminctl create-user exited with status {:?} — see {} / {}",
            status.code(),
            out_path.display(),
            err_path.display()
        ),
        None => Err(helper_timeout_failure(
            &mut proc,
            "adminctl create-user",
            SEED_ADMIN_TIMEOUT,
            &out_path,
            &err_path,
        )),
    }
}

/// Poll-with-deadline wait on a transient helper (never a blocking platform
/// wait, so it can never hang past `timeout`). Public so the integration
/// test in `tests/prep.rs` can drive the timeout branch with the
/// `__test-child` fixture.
pub fn wait_for_helper(
    proc: &mut platform::OwnedProc,
    timeout: Duration,
) -> Result<Option<platform::ExitInfo>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = proc.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The shared timeout branch for every transient helper (`build`, `mint_ca`,
/// `seed_admin`): forcibly stops the still-running helper (0s grace / 5s
/// force — it already blew its deadline) and produces the operator-facing
/// error naming BOTH log paths. Public so the integration test in
/// `tests/prep.rs` can pin the branch: the error names the logs AND the
/// child is dead afterwards.
pub fn helper_timeout_failure(
    proc: &mut platform::OwnedProc,
    what: &str,
    timeout: Duration,
    out_path: &Path,
    err_path: &Path,
) -> anyhow::Error {
    if let Err(error) = proc.shutdown(HELPER_SHUTDOWN_GRACE, HELPER_SHUTDOWN_FORCE) {
        eprintln!("weles: stopping timed-out {what} failed: {error:#}");
    }
    anyhow::anyhow!(
        "{what} did not finish within {timeout:?} — see {} / {}",
        out_path.display(),
        err_path.display()
    )
}

/// Builds a child environment from the parent process's env, keeping only
/// `allowlist` keys (case-insensitive on Windows to match `%VAR%` lookup
/// semantics, exact-case on Unix).
fn filtered_env(allowlist: &[&str]) -> BTreeMap<OsString, OsString> {
    let mut env = BTreeMap::new();
    for key in allowlist {
        if let Some(value) = lookup_env(key) {
            // On Windows the lookup above is case-insensitive, so
            // case-variant allowlist entries (`HTTP_PROXY` / `http_proxy`)
            // resolve to the SAME parent variable — keep only the
            // first-inserted spelling instead of emitting a pair differing
            // only by case in the child's environment block. On Unix the
            // lookup is exact-case and the variants are genuinely distinct
            // variables, so no dedupe.
            if cfg!(windows)
                && env.keys().any(|existing: &OsString| {
                    existing
                        .to_str()
                        .is_some_and(|existing| existing.eq_ignore_ascii_case(key))
                })
            {
                continue;
            }
            env.insert(OsString::from(*key), value);
        }
    }
    env
}

#[cfg(windows)]
fn lookup_env(key: &str) -> Option<OsString> {
    std::env::vars_os().find_map(|(candidate, value)| {
        candidate
            .to_str()
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(key))
            .then_some(value)
    })
}

#[cfg(not(windows))]
fn lookup_env(key: &str) -> Option<OsString> {
    std::env::var_os(key)
}

/// Resolves a bare executable name (e.g. `"cargo"`) to an absolute path by
/// searching `PATH` (with `PATHEXT` on Windows). Required because
/// `platform::spawn` calls `CreateProcessW`/`Command::new` with an explicit
/// `program` path: on Windows, `CreateProcessW` given a non-null
/// `lpApplicationName` searches only the current directory, NOT `PATH` — it
/// does not fall back to execvp-style PATH search the way a NULL
/// `lpApplicationName` + bare command-line token would. Mirrors
/// `tools/devctl/src/supervisor.rs::executable_on_path`.
pub fn resolve_on_path(name: &str) -> Result<PathBuf> {
    let path = std::env::var_os("PATH").context("PATH is not set in this process's environment")?;
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var_os("PATHEXT")
            .map(|value| {
                value
                    .to_string_lossy()
                    .split(';')
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_else(|| vec![".EXE".to_string(), ".CMD".to_string(), ".BAT".to_string()])
    } else {
        vec![String::new()]
    };

    for directory in std::env::split_paths(&path) {
        for extension in &extensions {
            let candidate: PathBuf = directory.join(format!("{name}{extension}"));
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    bail!("{name} not found on PATH")
}

#[cfg(test)]
#[path = "prep_tests.rs"]
mod prep_tests;
