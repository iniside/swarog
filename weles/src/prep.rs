//! Prep pipeline: decides HOW the fleet gets spawnable — deployed binary
//! paths, artifact staging (`weles deploy`), pre-flight binary validation, and
//! the transient helper runs (`edgeca`, `adminctl create-user`) that must
//! succeed before any long-lived service is spawned. Consumes
//! [`crate::manifest`] (the WHAT — process names/ports/env) and
//! [`crate::platform::spawn`] (the ONLY spawn mechanism in this crate — see the
//! crate-wide invariant documented beside `SPAWN_LOCK` in `platform::mod`).
//! Never `std::process::Command` directly: the Windows spawn path uses blanket
//! handle inheritance with no `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` allow-list,
//! so a `std::process::Command` spawn racing a concurrent `platform::spawn`
//! could cross-inherit the other's transient inheritable stdio duplicates.
//!
//! weles is an orchestrator, not a build system: it never invokes `cargo` and
//! never reads `target/`. It executes ONLY artifacts staged in `<root>/deploy`
//! by `weles deploy`. (Because that removed the old `cargo build` child, this
//! crate no longer carries a `BUILD_ENV_ALLOWLIST`. The sibling
//! `tools/processctl/src/fleet.rs:8-14` allowlist — which devctl DOES use to
//! build — still omits `SYSTEMDRIVE`/`ProgramData`, a latent linker-env gap
//! recorded here as a known sibling; do NOT touch processctl for it.)

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::platform::{self, SpawnSpec};

/// Dev-mode Postgres DSN — matches `tools/devctl/src/supervisor.rs::DEFAULT_DB`
/// exactly (the same local dev role/db/password `devctl` uses).
const DEFAULT_DATABASE_URL: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

const HELPER_SHUTDOWN_GRACE: Duration = Duration::from_secs(0);
const HELPER_SHUTDOWN_FORCE: Duration = Duration::from_secs(5);
const MINT_CA_TIMEOUT: Duration = Duration::from_secs(30);
const SEED_ADMIN_TIMEOUT: Duration = Duration::from_secs(30);

/// The workspace's on-disk layout as weles cares about it: the repo root, its
/// own `run/weles` scratch dir (created on discovery), and `bin_dir` —
/// `<root>/deploy`, the FIXED directory weles executes staged artifacts from.
/// weles never builds and never reads `target/`; an operator stages binaries
/// into `bin_dir` with `weles deploy`.
pub struct Layout {
    pub root: PathBuf,
    pub run_dir: PathBuf,
    pub bin_dir: PathBuf,
}

impl Layout {
    /// Discovers the layout under `root`, creating `root/run/weles` if absent.
    /// `bin_dir` is fixed at `root/deploy` (config-as-code: no env override, no
    /// debug/release heuristic, no `CARGO_TARGET_DIR`) — weles executes ONLY
    /// what `weles deploy` staged there.
    pub fn discover(root: PathBuf) -> Result<Self> {
        let run_dir = root.join("run").join("weles");
        std::fs::create_dir_all(&run_dir)
            .with_context(|| format!("create run dir {}", run_dir.display()))?;

        let bin_dir = root.join("deploy");

        Ok(Layout {
            root,
            run_dir,
            bin_dir,
        })
    }

    /// Path to the deployed binary for cargo package `pkg`
    /// (`deploy/<pkg>[.exe]`).
    pub fn binary(&self, pkg: &str) -> PathBuf {
        self.bin_dir
            .join(format!("{pkg}{}", std::env::consts::EXE_SUFFIX))
    }
}

/// Resolves `database_url()` — `DATABASE_URL` env if set, else the same dev
/// default `devctl` uses (`tools/devctl/src/supervisor.rs::DEFAULT_DB`).
pub fn database_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string())
}

/// The full set of binaries weles stages and may execute: the union of the
/// split and monolith fleet packages plus the two prep helpers (`edgeca`,
/// `adminctl`). Deterministic, deduped, sorted — the authority for `weles
/// deploy`'s copy set.
pub fn deploy_packages() -> Vec<&'static str> {
    let mut pkgs: Vec<&'static str> = crate::manifest::split_fleet()
        .iter()
        .map(|svc| svc.pkg)
        .collect();
    pkgs.push(crate::manifest::monolith().pkg);
    pkgs.push("edgeca");
    pkgs.push("adminctl");
    pkgs.sort_unstable();
    pkgs.dedup();
    pkgs
}

/// Pre-flight gate (didn't-forget style): every binary the chosen run needs
/// must already be staged in `layout.bin_dir` (`<root>/deploy`). Lists EVERY
/// missing binary, one per line, and points the operator at `weles deploy` —
/// weles executes only deployed artifacts and never builds. Called right after
/// the rollout lock, before any other validation, so a run with an incomplete
/// deploy dir dies pre-work instead of half-booting.
pub fn validate_binaries(layout: &Layout, packages: &[&str]) -> Result<()> {
    let mut missing: Vec<PathBuf> = Vec::new();
    for pkg in packages {
        let path = layout.binary(pkg);
        if !path.is_file() {
            missing.push(path);
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let mut message = String::from(
        "missing staged binaries — weles executes only what was deployed, it never builds:\n",
    );
    for path in &missing {
        message.push_str(&format!("  {}\n", path.display()));
    }
    message.push_str("build them and stage with: weles deploy <your-build-output-dir>");
    bail!("{message}")
}

/// `weles deploy <src_dir>`: stages the fleet binaries ([`deploy_packages`])
/// from `src_dir` into `layout.bin_dir` (`<root>/deploy`, created if absent).
/// Prints a per-file report line (copied / missing). This IS a redeploy: an
/// existing staged binary is OVERWRITTEN.
///
/// Failure semantics: ANY missing source binary makes this return an error
/// listing every missing source; the files ALREADY copied this run REMAIN
/// staged (no rollback in M0 — a redeploy simply re-copies everything).
///
/// Live-fleet safety is deliberately NOT enforced in M0: `deploy` takes no
/// rollout lock. On Windows a running service holds an exclusive lock on its
/// own `.exe`, so overwriting a live binary FAILS LOUDLY (the copy errors); on
/// Unix the copy SUCCEEDS silently (the running process keeps its now-unlinked
/// inode). This asymmetry is accepted for M0 — a proper rolling redeploy under
/// a live fleet is M1's job.
pub fn deploy(layout: &Layout, src_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(&layout.bin_dir)
        .with_context(|| format!("create deploy dir {}", layout.bin_dir.display()))?;

    let mut missing: Vec<PathBuf> = Vec::new();
    for pkg in deploy_packages() {
        let file = format!("{pkg}{}", std::env::consts::EXE_SUFFIX);
        let src = src_dir.join(&file);
        let dst = layout.bin_dir.join(&file);
        if !src.is_file() {
            println!("weles: {pkg}: MISSING in {}", src_dir.display());
            missing.push(src);
            continue;
        }
        std::fs::copy(&src, &dst)
            .with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;
        println!("weles: {pkg}: copied -> {}", dst.display());
    }

    if !missing.is_empty() {
        let mut message = String::from(
            "weles deploy: source binaries missing (already-copied files remain staged, no rollback):\n",
        );
        for path in &missing {
            message.push_str(&format!("  {}\n", path.display()));
        }
        bail!("{message}");
    }
    Ok(())
}

/// The internal mTLS CA cert/key pair minted for the fleet.
pub struct CaPaths {
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// Mints the fleet's internal-edge CA material at `run/weles/edge-ca.{crt,key}`
/// via the deployed `edgeca` binary, unless BOTH files already exist (idempotent
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
/// deployed `adminctl` binary. Password crosses ONLY via `ADMINCTL_PASSWORD` env
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

/// The shared timeout branch for every transient helper (`mint_ca`,
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

#[cfg(test)]
#[path = "prep_tests.rs"]
mod prep_tests;
