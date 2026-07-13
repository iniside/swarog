//! Cross-platform split-proof harness — replacement for the retired shell harnesses.
//!
//! The shell harnesses are structurally fragile on Windows (PowerShell native-arg
//! quote-stripping, MSYS `wait` hangs, winctrl exit-code false-throws). This harness
//! removes the shell entirely: the 12-service fleet is spawned via `processctl`
//! with a TYPED env map and a kill-on-drop guard, health-checked over `reqwest`,
//! DB-asserted via `sqlx`, and the player QUIC front driven through the `edge` crate as
//! a library. No `curl.exe`, no `psql.exe`, no `playercli.exe`, no `winctrl`.
//!
//! The harness runs the full named split assertion set, then reboots the monolith for
//! parity and proves its native graceful shutdown. See
//! docs/plans/2026-07-11-1730-rust-splitproof-harness-plan.md.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use edge::{DevCA, PlayerClient};
use processctl::{
    game_backend_fleet_with_environment, game_backend_monolith, rollout_lock_path, EnvironmentSnapshot, BorrowedLease, FleetFlavor,
    FleetInputs, FleetSpec, OutputDestination, OwnedChild, OwnedLease, ProcessGroupPolicy,
    RolloutLock, ServiceSpec, ShutdownOutcome, ShutdownPolicy, SpawnSpec,
};
use sqlx::{PgPool, Row};

const DEFAULT_DB: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

struct Running {
    name: &'static str,
    child: OwnedChild,
}

struct Ctx {
    bin_dir: PathBuf,
    root: PathBuf,
    run_dir: PathBuf,
    ca_cert: PathBuf,
    ca_key: PathBuf,
    db_url: String,
    fleet: FleetSpec,
    http: reqwest::Client,
    /// A client that does NOT follow redirects, so a 303 (epic callback, admin login)
    /// is observable as a status + Location instead of being transparently chased.
    http_noredirect: reqwest::Client,
    environment: EnvironmentSnapshot,
}

impl Ctx {
    fn service(&self, name: &str) -> &ServiceSpec {
        self.fleet.service(name).expect("canonical service name")
    }

    fn http_port(&self, name: &str) -> u16 {
        self.service(name).http_port
    }

    fn player_port(&self) -> u16 {
        self.service("gateway-svc")
            .player_port
            .expect("gateway has player port")
    }

    fn spawn(&self, svc: &ServiceSpec) -> Result<Running> {
        let bin = self
            .bin_dir
            .join(format!("{}{}", svc.executable_package, std::env::consts::EXE_SUFFIX));
        if !bin.exists() {
            bail!("binary not found: {} (run `cargo build` first)", bin.display());
        }
        let child = OwnedChild::spawn(spawn_spec(
            svc.name,
            bin,
            Vec::new(),
            &svc.env,
            &self.root,
            &self.run_dir.join(format!("{}.out.log", svc.name)),
            &self.run_dir.join(format!("{}.err.log", svc.name)),
        ))
            .with_context(|| format!("spawn {}", svc.name))?;
        Ok(Running { name: svc.name, child })
    }

    async fn wait_healthy(&self, svc: &ServiceSpec) -> Result<()> {
        let url = format!("http://127.0.0.1:{}/readyz", svc.http_port);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(resp) = self.http.get(&url).send().await {
                if resp.status().is_success() {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                bail!("{} did not become healthy on :{}", svc.name, svc.http_port);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

/// Tiny assertion recorder: prints PASS/FAIL per check, keeps a failure list, and the
/// process exits non-zero iff any check failed.
#[derive(Default)]
struct Proof {
    pass: u32,
    fail: Vec<String>,
}

impl Proof {
    fn check(&mut self, name: &str, ok: bool, detail: impl std::fmt::Display) {
        if ok {
            self.pass += 1;
            println!("  PASS  {name} — {detail}");
        } else {
            self.fail.push(name.to_string());
            println!("  FAIL  {name} — {detail}");
        }
    }
}

fn spawn_spec(
    label: impl Into<String>,
    executable: PathBuf,
    args: Vec<OsString>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
    stdout: &Path,
    stderr: &Path,
) -> SpawnSpec {
    SpawnSpec {
        label: label.into(),
        executable,
        args,
        env: env
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect(),
        cwd: cwd.to_path_buf(),
        stdout: OutputDestination::File(stdout.to_path_buf()),
        stderr: OutputDestination::File(stderr.to_path_buf()),
        process_group: ProcessGroupPolicy::Owned,
    }
}

fn wait_for_exit(child: &mut OwnedChild) -> Result<std::process::ExitStatus> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn executable_on_path(name: &str, env: &BTreeMap<String, String>) -> Result<PathBuf> {
    let path = env.get("PATH").context("PATH is absent from the build environment")?;
    let extensions: Vec<&str> = if cfg!(windows) {
        env.get("PATHEXT")
            .map(|value| value.split(';').collect())
            .unwrap_or_else(|| vec![".COM", ".EXE", ".BAT", ".CMD"])
    } else {
        vec![""]
    };
    for directory in std::env::split_paths(OsStr::new(path)) {
        for extension in &extensions {
            let candidate = directory.join(format!("{name}{extension}"));
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    bail!("{name} executable not found in the explicit build PATH")
}

/// Extract `<input name="X" value="Y">` pairs from an admin form page (for the M3b
/// no-op form resubmit — a tiny hand parser avoids a regex dep).
fn extract_form_fields(html: &str) -> Vec<(String, String)> {
    let attr = |tag: &str, key: &str| -> Option<String> {
        let pat = format!("{key}=\"");
        let start = tag.find(&pat)? + pat.len();
        let end = tag[start..].find('"')? + start;
        Some(tag[start..end].to_string())
    };
    let mut out = Vec::new();
    for input in html.split("<input").skip(1) {
        let tag = &input[..input.find('>').unwrap_or(input.len())];
        if let Some(name) = attr(tag, "name") {
            out.push((name, attr(tag, "value").unwrap_or_default()));
        }
    }
    out
}

/// Monolith-parity phase: boot cmd/server (all modules Local) on the split's player
/// front and re-prove register/QUIC/auth/admin work identically (M0-M3b).
async fn monolith_parity(ctx: &Ctx, pool: &PgPool, p: &mut Proof) -> Result<()> {
    println!("\n[splitproof] === MONOLITH PARITY (cmd/server, all Local) ===");
    sqlx::query("DELETE FROM admin.sessions").execute(pool).await.ok();
    sqlx::query("DELETE FROM admin.login_attempts").execute(pool).await.ok();
    let bin = ctx.bin_dir.join(format!("server{}", std::env::consts::EXE_SUFFIX));
    if !bin.exists() {
        bail!("monolith binary not found: {}", bin.display());
    }
    let characters_port = ctx.http_port("characters-svc");
    let env = game_backend_monolith(
        &FleetInputs { database_url: ctx.db_url.clone(), edge_ca_cert: ctx.ca_cert.clone(), edge_ca_key: ctx.ca_key.clone() },
        FleetFlavor::Proof,
        &ctx.environment,
    ).env;
    let child = OwnedChild::spawn(spawn_spec(
        "server",
        bin,
        Vec::new(),
        &env,
        &ctx.root,
        &ctx.run_dir.join("monolith.out.log"),
        &ctx.run_dir.join("monolith.err.log"),
    ))
    .context("spawn monolith")?;
    let mut mono = Running { name: "server", child };
    let m = format!("http://127.0.0.1:{characters_port}");
    // wait healthy
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(r) = ctx.http.get(format!("{m}/readyz")).send().await {
            if r.status().is_success() {
                break;
            }
        }
        if Instant::now() >= deadline {
            bail!("monolith did not become healthy on :{characters_port}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    println!("[splitproof] monolith healthy on :{characters_port}");
    let suffix = std::process::id();

    // [M0] register a player on the monolith (accounts module local, real session).
    let mtoken = register_login(ctx, &m, &format!("mono-{suffix}@test.local")).await.ok();
    p.check("[M0] monolith register -> real bearer", mtoken.is_some(), "");
    if let Some(tok) = &mtoken {
        // [M1] QUIC characters.create 'solo' (all ops Local).
        let m1 = player_call(ctx, Some(tok), "characters.create", r#"{"name":"solo","class":""}"#).await;
        p.check("[M1] monolith QUIC create -> Ok", status_or_err(&m1, "Ok"), "");
        // [M2] a dev- token is rejected by the real local accounts verifier.
        let m2 = player_call(ctx, Some(&format!("dev-{suffix}")), "characters.create", r#"{"name":"x","class":""}"#).await;
        p.check("[M2] monolith dev- token -> Unauthorized", status_or_err(&m2, "Unauthorized"), "");
    }

    // [M3] admin portal parity: fresh jar logs in -> 303, LOCAL characters page shows 'solo'.
    let jar = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let m3l = jar.post(format!("{m}/admin/login")).form(&[("username", "proofadmin"), ("password", "proofpass")]).send().await?;
    let m3 = jar.get(format!("{m}/admin/characters")).send().await?;
    let (m3c, m3b) = (m3.status().as_u16(), m3.text().await.unwrap_or_default());
    p.check(
        "[M3] monolith admin login + characters shows solo",
        m3l.status().as_u16() == 303 && m3c == 200 && m3b.contains("solo"),
        format!("login={} chars={m3c}", m3l.status().as_u16()),
    );

    // [M3b] LOCAL apikeys form-submit WITH _csrf -> a NEW admin.action{form-submit} event
    // (remote forms in the split are read-only, so this is the only place it's exercised).
    let before: Option<i64> = sqlx::query_scalar("SELECT count(*) FROM asyncevents.events WHERE topic='admin.action' AND payload->>'action'='form-submit'").fetch_optional(pool).await.ok().flatten();
    let page = jar.get(format!("{m}/admin/api-keys")).send().await?.text().await.unwrap_or_default();
    let fields = extract_form_fields(&page);
    if fields.iter().any(|(k, _)| k == "_csrf") {
        let form: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let _ = jar.post(format!("{m}/admin/api-keys")).form(&form).send().await;
        let mut ok = false;
        for _ in 0..30 {
            let after: Option<i64> = sqlx::query_scalar("SELECT count(*) FROM asyncevents.events WHERE topic='admin.action' AND payload->>'action'='form-submit'").fetch_optional(pool).await.ok().flatten();
            if after.unwrap_or(0) > before.unwrap_or(0) {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        p.check("[M3b] local form-submit -> new admin.action event", ok, format!("before={before:?}"));
    } else {
        p.check("[M3b] local form-submit form present (_csrf)", false, "no _csrf field on apikeys page");
    }

    // [W2] graceful shutdown: a native Ctrl-Break (Windows) / SIGTERM (unix) must drain
    // in-flight work and exit 0 within the grace window — no force-kill. This is the
    // proof winctrl gave, now native (the app's shutdown_signal listens for ctrl_break).
    let shutdown = mono.child.shutdown(ShutdownPolicy {
        graceful_timeout: Duration::from_secs(15),
        force_timeout: Duration::from_secs(5),
    });
    let (sent, clean) = match shutdown {
        Ok(ShutdownOutcome::Graceful(status)) => (true, status.success()),
        Ok(ShutdownOutcome::AlreadyExited(status)) => (false, status.success()),
        Ok(ShutdownOutcome::Forced(_)) | Err(_) => (true, false),
    };
    p.check(
        "[W2] monolith graceful shutdown -> clean exit",
        sent && clean,
        format!("sent={sent} clean={clean}"),
    );
    // mono drops here: if it exited, kill() is a no-op; otherwise force-kill (cleanup).
    Ok(())
}

fn workspace_dirs() -> Result<(PathBuf, PathBuf)> {
    // splitproof.exe lives in target/debug (or target/release); its siblings are the
    // svc binaries, and the workspace root is two levels up.
    let exe = std::env::current_exe()?;
    let bin_dir = exe.parent().context("no bin dir")?.to_path_buf();
    let root = bin_dir
        .parent()
        .and_then(Path::parent)
        .context("no workspace root")?
        .to_path_buf();
    Ok((bin_dir, root))
}

enum ActiveLease {
    Borrowed(BorrowedLease),
    Owned(OwnedLease),
}

impl ActiveLease {
    fn description(&self) -> (&'static str, &str) {
        match self {
            Self::Borrowed(lease) => ("borrowed", lease.run_id()),
            Self::Owned(lease) => ("owned", lease.run_id()),
        }
    }
}

fn main() -> std::process::ExitCode {
    if let Some(exit) = processctl::dispatch_guardian_from_current_exe() {
        return exit;
    }
    let (bin_dir, root) = match workspace_dirs() {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("splitproof: fatal: {error:#}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let run_dir = root.join("run");
    if let Err(error) = std::fs::create_dir_all(&run_dir) {
        eprintln!("splitproof: fatal: create {}: {error}", run_dir.display());
        return std::process::ExitCode::FAILURE;
    }
    let lease = match BorrowedLease::consume_inherited_if_present("splitproof") {
        Ok(Some(lease)) => ActiveLease::Borrowed(lease),
        Ok(None) => match RolloutLock::acquire(
            rollout_lock_path(&root),
            format!("splitproof-{}", std::process::id()),
            "splitproof",
        ) {
            Ok(lease) => ActiveLease::Owned(lease),
            Err(error) => {
                eprintln!("splitproof: fatal: acquire rollout lease: {error}");
                return std::process::ExitCode::FAILURE;
            }
        },
        Err(error) => {
            eprintln!("splitproof: fatal: consume inherited rollout lease: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("splitproof: fatal: create Tokio runtime: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (lease_kind, run_id) = lease.description();
    println!("[splitproof] rollout lease: {lease_kind} ({run_id})");
    let result = runtime.block_on(run(bin_dir, root, run_dir));
    drop(runtime);
    drop(lease);
    match result {
        Ok(0) => std::process::ExitCode::SUCCESS,
        Ok(n) => {
            eprintln!("splitproof: {n} assertion(s) failed");
            std::process::ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("splitproof: fatal: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(bin_dir: PathBuf, root: PathBuf, run_dir: PathBuf) -> Result<u32> {
    let environment = EnvironmentSnapshot::capture();
    let db_url = environment.value("DATABASE_URL").map(str::to_owned).unwrap_or_else(|| DEFAULT_DB.to_string());
    let fleet = game_backend_fleet_with_environment(
        &FleetInputs {
            database_url: db_url.clone(),
            edge_ca_cert: run_dir.join("edge-ca.crt"),
            edge_ca_key: run_dir.join("edge-ca.key"),
        },
        FleetFlavor::Proof,
        &environment,
    );
    let ctx = Ctx {
        ca_cert: run_dir.join("edge-ca.crt"),
        ca_key: run_dir.join("edge-ca.key"),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?,
        http_noredirect: reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        bin_dir,
        root: root.clone(),
        run_dir,
        db_url,
        fleet,
        environment,
    };

    // Fleet-drift tripwire: the harness svc list must equal cmd/*-svc on disk.
    preflight_fleet(&root, &ctx)?;

    // Build the fleet (svcs + monolith + adminctl) so a bare `cargo run -p splitproof`
    // is self-contained — no dependency on a prior verify stage having built them.
    // Skippable for fast dev iteration (SPLITPROOF_SKIP_BUILD=1).
    if std::env::var("SPLITPROOF_SKIP_BUILD").is_err() {
        build_fleet(&ctx, &root)?;
    }

    println!("[splitproof] minting shared edge dev CA -> {}", ctx.ca_cert.display());
    let ca_cert_str = ctx.ca_cert.to_str().context("CA cert path not UTF-8")?;
    let ca_key_str = ctx.ca_key.to_str().context("CA key path not UTF-8")?;
    DevCA::generate()
        .context("generate CA")?
        .write_pem(ca_cert_str, ca_key_str)
        .context("write CA")?;

    // Seed the admin logins PRE-BOOT (session auth): adminctl ensures schema `admin`
    // itself and upserts the login (password over stdin, never argv).
    seed_admin(&ctx, "proofadmin", "proofpass")?;
    seed_admin(&ctx, "prooflock", "lockpass")?;

    let pool = PgPool::connect(&ctx.db_url).await.context("connect DB")?;
    reset_config_baseline(&pool).await?;

    // Boot the fleet; each guard lives in `fleet` so a `?` below drops them all (kill).
    let mut fleet: Vec<Running> = Vec::new();
    for svc in ctx.fleet.services() {
        println!("[splitproof] starting {} on :{} ...", svc.name, svc.http_port);
        // config-svc must boot AFTER the baseline reset (done above) so its first
        // snapshot is the default; the ordering in `fleet()` already places it late.
        fleet.push(ctx.spawn(svc)?);
        ctx.wait_healthy(svc).await?;
        println!("[splitproof] {} healthy", svc.name);
    }
    println!("[splitproof] fleet up: {}/{} processes healthy\n", fleet.len(), ctx.fleet.services().len());

    let mut p = Proof::default();
    assertions(&ctx, &pool, &mut p).await?;

    // [I-GATE] live security proof: the harness boots the whole fleet with
    // INVENTORY_DEV_GRANT=1 (see `fleet()` above), so `assertions` structurally
    // cannot see the split bypass Step 1 closed. Restart ONLY inventory-svc without
    // the flag and prove a fully-authed grant call now 404s through the front door.
    i_gate(&ctx, &mut fleet, &mut p).await?;

    // --- Monolith parity: tear the split down (frees :8080 + :9100), boot cmd/server on
    // the same player front, and re-prove a subset (never-monolith-only-features). ---
    drop(fleet);
    tokio::time::sleep(Duration::from_millis(800)).await;
    if let Err(e) = monolith_parity(&ctx, &pool, &mut p).await {
        p.check("[M0-M3b] monolith parity phase", false, format!("fatal: {e:#}"));
    }

    println!(
        "\n[splitproof] {} passed, {} failed",
        p.pass,
        p.fail.len()
    );
    for f in &p.fail {
        println!("  - FAILED: {f}");
    }
    // fleet drops here → every child is killed (no orphans).
    Ok(p.fail.len() as u32)
}

/// Build every fleet svc + the monolith + adminctl (cargo caches, so this is a fast
/// no-op after the first build).
fn build_fleet(ctx: &Ctx, root: &Path) -> Result<()> {
    println!("[splitproof] building fleet (cargo build) ...");
    let mut args = vec!["build".to_string()];
    for svc in ctx.fleet.services() {
        args.push("-p".into());
        args.push(svc.executable_package.into());
    }
    for extra in ["server", "adminctl"] {
        args.push("-p".into());
        args.push(extra.into());
    }
    let env = ctx.environment.build_environment();
    let cargo = executable_on_path("cargo", &env)?;
    let mut child = OwnedChild::spawn(SpawnSpec {
        label: "splitproof-cargo-build".into(),
        executable: cargo,
        args: args.into_iter().map(OsString::from).collect(),
        env: env
            .into_iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect(),
        cwd: root.to_path_buf(),
        stdout: OutputDestination::Inherit,
        stderr: OutputDestination::Inherit,
        process_group: ProcessGroupPolicy::Owned,
    })
    .context("run cargo build")?;
    let status = wait_for_exit(&mut child)?;
    if !status.success() {
        bail!("cargo build of the fleet failed");
    }
    Ok(())
}

fn preflight_fleet(root: &Path, ctx: &Ctx) -> Result<()> {
    ctx.fleet.validate_disk(&root.join("cmd"))?;
    println!(
        "[splitproof] fleet preflight OK: {} svcs == cmd/*-svc on disk",
        ctx.fleet.services().len()
    );
    Ok(())
}

/// Seed an admin login via adminctl (password in its supported private environment
/// input, never argv). adminctl ensures its schema before admin-svc migrates.
fn seed_admin(ctx: &Ctx, user: &str, pass: &str) -> Result<()> {
    let bin = ctx.bin_dir.join(format!("adminctl{}", std::env::consts::EXE_SUFFIX));
    let mut env = ctx.environment.runtime_environment();
    env.insert("DATABASE_URL".into(), ctx.db_url.clone());
    env.insert("ADMINCTL_PASSWORD".into(), pass.to_string());
    let mut child = OwnedChild::spawn(SpawnSpec {
        label: format!("adminctl-{user}"),
        executable: bin,
        args: ["create-user", user].into_iter().map(OsString::from).collect(),
        env: env
            .into_iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect(),
        cwd: ctx.root.clone(),
        stdout: OutputDestination::Null,
        stderr: OutputDestination::Null,
        process_group: ProcessGroupPolicy::Owned,
    })
    .with_context(|| format!("spawn adminctl for {user}"))?;
    if !wait_for_exit(&mut child)?.success() {
        bail!("adminctl create-user {user} failed");
    }
    Ok(())
}

async fn reset_config_baseline(pool: &PgPool) -> Result<()> {
    // Inventory's starter must default to starter_sword so a later live change proves a
    // reload; proof.* rows from a prior run must not leak into assertions.
    // Two statements → two query() calls (sqlx's extended protocol runs only one each).
    sqlx::query("DELETE FROM config.settings WHERE namespace='inventory' AND key='starter_item'")
        .execute(pool).await.ok(); // config schema may not exist yet on a fresh DB — best-effort.
    sqlx::query("DELETE FROM config.settings WHERE namespace='proof'").execute(pool).await.ok();
    Ok(())
}

async fn assertions(ctx: &Ctx, pool: &PgPool, p: &mut Proof) -> Result<()> {
    let g = format!("http://127.0.0.1:{}", ctx.http_port("gateway-svc"));
    let suffix = std::process::id();

    // [RDY] gateway readyz with the full fleet up.
    let rdy = ctx.http.get(format!("{g}/readyz")).send().await?;
    p.check("[RDY] gateway /readyz", rdy.status().is_success(), rdy.status());

    // [A1] register through the front door (G -> D over the mTLS edge).
    let email = format!("proof-{suffix}@test.local");
    let reg = ctx
        .http
        .post(format!("{g}/accounts/register"))
        .header("X-Api-Key", "dev-key-client")
        .json(&serde_json::json!({"email": email, "password": "pw", "displayName": "Proof"}))
        .send()
        .await?;
    let reg_code = reg.status();
    let reg_body: serde_json::Value = reg.json().await.unwrap_or(serde_json::Value::Null);
    let player_id = reg_body.get("player_id").and_then(|v| v.as_str()).map(str::to_string);
    p.check(
        "[A1] register -> 201 + player_id",
        reg_code.as_u16() == 201 && player_id.is_some(),
        format!("code={reg_code} player_id={player_id:?}"),
    );

    // [A2] login -> 200 + bearer.
    let login = ctx
        .http
        .post(format!("{g}/accounts/login"))
        .header("X-Api-Key", "dev-key-client")
        .json(&serde_json::json!({"email": email, "password": "pw"}))
        .send()
        .await?;
    let login_code = login.status();
    let login_body: serde_json::Value = login.json().await.unwrap_or(serde_json::Value::Null);
    let token = login_body.get("token").and_then(|v| v.as_str()).map(str::to_string);
    p.check(
        "[A2] login -> 200 + token",
        login_code.as_u16() == 200 && token.is_some(),
        format!("code={login_code} token={}", token.is_some()),
    );

    // [A3] me with the real bearer -> 200 (auth-once verified over the edge).
    if let Some(tok) = &token {
        let me = ctx
            .http
            .get(format!("{g}/accounts/me"))
            .header("X-Api-Key", "dev-key-client")
            .header("Authorization", format!("Bearer {tok}"))
            .send()
            .await?;
        let me_code = me.status();
        let me_body = me.text().await.unwrap_or_default();
        let ok = me_code.as_u16() == 200
            && player_id.as_deref().map(|id| me_body.contains(id)).unwrap_or(false);
        p.check("[A3] me (Bearer) -> 200 with player", ok, format!("code={me_code}"));
    }

    // [K5] key-verifier under distinct-key spam: every response 401/403/429, never a
    // 5xx crash (the guaranteed observable of the 503-shed fix; the 503 path itself is
    // unit-tested). Fired concurrently through tokio — the flow that hung the bash
    // harness's `wait`.
    let mut handles = Vec::new();
    for i in 0..16 {
        let http = ctx.http.clone();
        let g = g.clone();
        handles.push(tokio::spawn(async move {
            http.get(format!("{g}/leaderboard"))
                .header("X-Api-Key", format!("bogus-{i}-{}", std::process::id()))
                .send()
                .await
                .map(|r| r.status().as_u16())
        }));
    }
    let mut codes = Vec::new();
    for h in handles {
        if let Ok(Ok(code)) = h.await {
            codes.push(code);
        }
    }
    let clean = !codes.is_empty()
        && codes.iter().all(|&c| matches!(c, 401 | 403 | 429))
        && codes.contains(&401);
    p.check(
        "[K5] distinct bogus keys -> 401/403/429, no 5xx",
        clean,
        format!("{} responses: {:?}", codes.len(), codes),
    );

    // [C4] config large-value: a >8 KB value must NOT abort the write (the pg_notify
    // payload is value-less now) and the revision must advance.
    let rev0 = current_revision(pool).await.unwrap_or(0);
    let big = "x".repeat(9000);
    let wrote = sqlx::query(
        "INSERT INTO config.settings (namespace, key, value) VALUES ('proof','big',$1) \
         ON CONFLICT (namespace, key) DO UPDATE SET value = EXCLUDED.value",
    )
    .bind(&big)
    .execute(pool)
    .await;
    let readback: Option<i64> = sqlx::query_scalar(
        "SELECT length(value)::bigint FROM config.settings WHERE namespace='proof' AND key='big'",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let rev1 = current_revision(pool).await.unwrap_or(rev0);
    p.check(
        "[C4] >8KB config write commits + bumps revision",
        wrote.is_ok() && readback == Some(9000) && rev1 > rev0,
        format!("wrote_ok={} len={:?} rev {rev0}->{rev1}", wrote.is_ok(), readback),
    );

    // [L1] leaderboard with a VALID key -> 200 (positive control for K5's negatives).
    let lb = ctx
        .http
        .get(format!("{g}/leaderboard"))
        .header("X-Api-Key", "dev-key-client")
        .send()
        .await?;
    p.check("[L1] leaderboard (valid key) -> 200", lb.status().as_u16() == 200, lb.status());

    // --- Auth negatives: a bearer the real verifier rejects is 401 on every plane. ---
    // [A4] garbage bearer -> 401.
    let a4 = ctx
        .http
        .get(format!("{g}/characters"))
        .header("X-Api-Key", "dev-key-client")
        .header("Authorization", "Bearer totally-bogus-token")
        .send()
        .await?;
    p.check("[A4] garbage token -> 401", a4.status().as_u16() == 401, a4.status());

    // [A5] a dev-<uuid> token -> 401 (gateway-svc never sets ACCOUNTS_DEV_AUTH, so the
    // real accounts verifier rejects it — dev auth is not a bearer bypass at the front).
    let a5 = ctx
        .http
        .get(format!("{g}/characters"))
        .header("X-Api-Key", "dev-key-client")
        .header("Authorization", format!("Bearer dev-{suffix}"))
        .send()
        .await?;
    p.check("[A5] dev-<uuid> token -> 401", a5.status().as_u16() == 401, a5.status());

    // --- Epic OAuth passthrough (keyless; gateway proxies /accounts/epic/* to D). ---
    // One browser-like client carries the host-only binding cookie across both
    // requests while leaving the callback's relayed 303 observable.
    let epic_http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .cookie_store(true)
        .build()?;
    // [EP1] start -> authorize_url carrying a state param.
    let ep1 = epic_http.post(format!("{g}/accounts/epic/start")).send().await?;
    let ep1_body = ep1.text().await.unwrap_or_default();
    let state = ep1_body
        .split("state=")
        .nth(1)
        .map(|s| s.split(['&', '"']).next().unwrap_or("").to_string());
    p.check(
        "[EP1] epic start -> authorize_url with state",
        state.as_deref().map(|s| !s.is_empty()).unwrap_or(false),
        format!("state={:?}", state.as_deref().map(|s| &s[..s.len().min(8)])),
    );
    // [EP2] callback with a bad code -> 303 relayed verbatim to /?epic=error (no follow).
    if let Some(st) = &state {
        let ep2 = epic_http
            .get(format!("{g}/accounts/epic/callback?code=x&state={st}"))
            .send()
            .await?;
        let loc = ep2
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        p.check(
            "[EP2] epic callback -> 303 /?epic=error",
            ep2.status().as_u16() == 303 && loc == "/?epic=error",
            format!("code={} loc={loc}", ep2.status().as_u16()),
        );
    }

    // --- API-key policy (gateway enforces X-Api-Key + per-key method allow-list). ---
    // [K1] no key -> 401.
    let k1 = ctx.http.get(format!("{g}/leaderboard")).send().await?;
    p.check("[K1] no api key -> 401", k1.status().as_u16() == 401, k1.status());
    // [K2] bogus key -> 401.
    let k2 = ctx
        .http
        .get(format!("{g}/leaderboard"))
        .header("X-Api-Key", "totally-bogus-key")
        .send()
        .await?;
    p.check("[K2] bogus api key -> 401", k2.status().as_u16() == 401, k2.status());
    // [K3] dev-key-client on match.report -> 403 (player policy omits match.report).
    let k3 = ctx
        .http
        .post(format!("{g}/match/report"))
        .header("X-Api-Key", "dev-key-client")
        .json(&serde_json::json!({"ReportId": format!("k3-{suffix}"), "Winner": "k3-w", "Loser": "k3-l"}))
        .send()
        .await?;
    p.check("[K3] client key on match.report -> 403", k3.status().as_u16() == 403, k3.status());
    // [K4] dev-key-server on match.report -> 202 (full policy).
    let k4 = ctx
        .http
        .post(format!("{g}/match/report"))
        .header("X-Api-Key", "dev-key-server")
        .json(&serde_json::json!({"ReportId": format!("k4-{suffix}"), "Winner": "k4-w", "Loser": "k4-l"}))
        .send()
        .await?;
    p.check("[K4] server key on match.report -> 202", k4.status().as_u16() == 202, k4.status());
    // [K5b] a fresh distinct key AFTER the K5 burst -> 401 (permits/flights released,
    // shed is transient not sticky).
    let k5b = ctx
        .http
        .get(format!("{g}/leaderboard"))
        .header("X-Api-Key", format!("k5b-{suffix}"))
        .send()
        .await?;
    p.check("[K5b] post-burst fresh key -> 401", k5b.status().as_u16() == 401, k5b.status());

    // --- Characters/inventory: plain-id relations + durable character.created/deleted. ---
    let mut created_cid: Option<String> = None;
    if let Some(tok) = token.clone() {
        let other = register_login(ctx, &g, &format!("other-{suffix}@test.local")).await.ok();
        // [1] create through G -> A.
        let cid = create_character(ctx, &g, &tok, "Aria").await;
        p.check("[1] create character -> 201 + id", cid.is_some(), format!("cid={cid:?}"));
        if let Some(cid) = cid {
            created_cid = Some(cid.clone());
            // [1b] list through G -> A and prove the newly-created row crossed the
            // characters.list remote binding, not merely that some JSON returned.
            let list = ctx
                .http
                .get(format!("{g}/characters"))
                .header("X-Api-Key", "dev-key-client")
                .header("Authorization", format!("Bearer {tok}"))
                .send()
                .await?;
            let list_status = list.status();
            let list_body: serde_json::Value =
                list.json().await.unwrap_or(serde_json::Value::Null);
            let contains_created = list_body.as_array().is_some_and(|characters| {
                characters.iter().any(|character| {
                    character.get("id").and_then(serde_json::Value::as_str)
                        == Some(cid.as_str())
                })
            });
            p.check(
                "[1b] list characters -> 200 + created id",
                list_status.as_u16() == 200 && contains_created,
                format!("code={list_status} cid={cid}"),
            );
            // [2] starter grant appears (character.created -> inventory, durable).
            let starter = poll_inventory_has(ctx, &g, &tok, &cid, "starter_sword").await;
            p.check("[2] starter_sword granted via event", starter, format!("cid={cid}"));
            // [3] a DIFFERENT player is denied (owner_of over QUIC gates).
            if let Some(other) = &other {
                let (nc, _) = inventory_of(ctx, &g, other, &cid).await;
                p.check("[3] other player -> 403/404", nc == 403 || nc == 404, format!("code={nc}"));
            }
            // [4] delete.
            let del = ctx
                .http
                .delete(format!("{g}/characters/{cid}"))
                .header("X-Api-Key", "dev-key-client")
                .header("Authorization", format!("Bearer {tok}"))
                .send()
                .await?;
            p.check("[4] delete character -> 204", del.status().as_u16() == 204, del.status());
            // [5] holdings wiped in B (integrity via character.deleted, not FK cascade).
            let wiped = poll_count(
                pool,
                "SELECT count(*) FROM inventory.holdings WHERE owner_type='character' AND owner_id::text=$1",
                &cid,
                0,
            )
            .await;
            p.check("[5] holdings wiped via character.deleted", wiped, format!("cid={cid}"));
            // [5t] wipe planted the tombstone in the same delivery tx.
            let tomb: Option<i64> = sqlx::query_scalar(
                "SELECT count(*) FROM inventory.wiped_characters WHERE character_id::text=$1",
            )
            .bind(&cid)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
            p.check("[5t] wipe tombstone planted", tomb == Some(1), format!("rows={tomb:?}"));
            // [5b] gone via owner_of over QUIC too.
            let (w2, _) = inventory_of(ctx, &g, &tok, &cid).await;
            p.check("[5b] post-delete inventory -> 404", w2 == 404, format!("code={w2}"));
        }

        // --- Config live-reload (C1-C3, C4b): revision + NOTIFY + durable config.changed. ---
        // [C1] baseline: B booted with no config row -> default starter_sword.
        if let Some(bcid) = create_character(ctx, &g, &tok, "Baseline").await {
            let base = poll_inventory_has(ctx, &g, &tok, &bcid, "starter_sword").await;
            p.check("[C1] baseline starter is starter_sword", base, format!("cid={bcid}"));
        }
        // [C2] runtime change on the shared config DB.
        let c2 = sqlx::query(
            "INSERT INTO config.settings (namespace,key,value) VALUES ('inventory','starter_item','health_potion') \
             ON CONFLICT (namespace,key) DO UPDATE SET value=excluded.value",
        )
        .execute(pool)
        .await;
        p.check("[C2] set inventory/starter_item=health_potion", c2.is_ok(), "");
        // [C3] live reload: a fresh character is eventually granted health_potion.
        p.check(
            "[C3] live config reload -> health_potion",
            poll_fresh_grant(ctx, &g, &tok, "Reloaded", "health_potion").await,
            "",
        );
        // [C4b] reset -> fresh characters revert to starter_sword (reload still works).
        sqlx::query("DELETE FROM config.settings WHERE namespace='inventory' AND key='starter_item'")
            .execute(pool)
            .await
            .ok();
        p.check(
            "[C4b] config reset -> revert to starter_sword",
            poll_fresh_grant(ctx, &g, &tok, "Reverted", "starter_sword").await,
            "",
        );
    }

    // --- Match / rating / leaderboard: durable match.finished projection + idempotency. ---
    let winner = format!("champ-{suffix}");
    let loser = format!("chump-{suffix}");
    let mt1_rid = format!("mt1-{suffix}");
    let mt4_rid = format!("mt4-{suffix}");
    // [MT1] report -> 202 (AuthNone, capitalized body keys; emits durable match.finished).
    let mt1 = report(ctx, &g, &mt1_rid, &winner, &loser).await;
    p.check("[MT1] match.report -> 202", mt1 == 202, format!("code={mt1}"));
    // [MT2] leaderboard shows winner wins=1 (I->K durable + upsert; G routes Remote to K).
    p.check("[MT2] leaderboard winner wins=1", poll_leaderboard_wins(ctx, &g, &winner, 1).await, "");
    // [MT3] audit recorded match.finished (I->F durable, exactly-once).
    let mt3 = poll_count(
        pool,
        "SELECT count(*) FROM audit.log WHERE topic='match.finished' AND payload->>'winner'=$1",
        &winner,
        1,
    )
    .await;
    p.check("[MT3] audit match.finished recorded", mt3, "");
    // [MT4] a second report -> leaderboard wins=2 (accumulating upsert).
    let mt4 = report(ctx, &g, &mt4_rid, &winner, &loser).await;
    p.check(
        "[MT4] second report -> wins=2",
        mt4 == 202 && poll_leaderboard_wins(ctx, &g, &winner, 2).await,
        format!("code={mt4}"),
    );
    // [MT5] rating projection persisted (winner +15+15=1030, loser -15-15=970).
    let mt5 = {
        let mut ok = false;
        for _ in 0..30 {
            let w: Option<i64> = sqlx::query_scalar("SELECT mmr::bigint FROM rating.ratings WHERE player=$1")
                .bind(&winner).fetch_optional(pool).await.ok().flatten();
            let l: Option<i64> = sqlx::query_scalar("SELECT mmr::bigint FROM rating.ratings WHERE player=$1")
                .bind(&loser).fetch_optional(pool).await.ok().flatten();
            if w == Some(1030) && l == Some(970) {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        ok
    };
    p.check("[MT5] rating projection 1030/970", mt5, "");
    // [MT6] re-POST MT1's ReportId -> 202 no-op: exactly one match row (the strong dedup
    // proof — a caller replay after an ambiguous result must not double-commit).
    let mt6 = report(ctx, &g, &mt1_rid, &winner, &loser).await;
    let rows: Option<i64> = sqlx::query_scalar("SELECT count(*) FROM match.matches WHERE report_id=$1")
        .bind(&mt1_rid).fetch_optional(pool).await.ok().flatten();
    p.check(
        "[MT6] duplicate report -> 202, one match row",
        mt6 == 202 && rows == Some(1),
        format!("code={mt6} rows={rows:?}"),
    );

    // --- Player QUIC front (P1-P6) over the edge lib (no playercli subprocess). ---
    if let Some(tok) = token.clone() {
        // [P1] create over QUIC -> Ok; capture the fresh character id for P2/P3.
        let p1 = player_call(ctx, Some(&tok), "characters.create", r#"{"name":"hero","class":""}"#).await;
        let pcid = p1.as_ref().ok().and_then(find_id).unwrap_or_default();
        p.check("[P1] QUIC characters.create -> Ok", status_or_err(&p1, "Ok"), format!("pcid={pcid}"));
        // [P2] inventory.listCharacter over QUIC (G -> Remote B -> owner_of QUIC -> A) -> Ok.
        let p2 = player_call(ctx, Some(&tok), "inventory.listCharacter", &format!("{{\"character_id\":\"{pcid}\"}}")).await;
        p.check("[P2] QUIC inventory.listCharacter -> Ok", status_or_err(&p2, "Ok"), "");
        // [P3] the HTTP front routes inventory.* Remote to B -> 200.
        let (p3, _) = inventory_of(ctx, &g, &tok, &pcid).await;
        p.check("[P3] HTTP front inventory -> 200", p3 == 200, format!("code={p3}"));
        // [P4] no token -> Unauthorized (bearer required at the front).
        let p4 = player_call(ctx, None, "characters.create", r#"{"name":"x","class":""}"#).await;
        p.check("[P4] no-token op -> Unauthorized", status_or_err(&p4, "Unauthorized"), "");
        // [P4b] bad token -> Unauthorized (token verified, not just present).
        let p4b = player_call(ctx, Some("nope-x"), "characters.create", r#"{"name":"x","class":""}"#).await;
        p.check("[P4b] bad-token op -> Unauthorized", status_or_err(&p4b, "Unauthorized"), "");
        // [P5] a wire-only method absent from the player allow-list -> NotFound.
        let p5 = player_call(ctx, Some(&tok), "characters.ownerOf", &format!("{{\"character_id\":\"{pcid}\"}}")).await;
        p.check("[P5] wire-only method -> NotFound", status_or_err(&p5, "NotFound"), "");
        // [P6] per-connection rate-limit + refill.
        p.check("[P6] player rate-limit + refill", player_burst(ctx).await, "");
    }

    // --- Admin portal (session auth) + audit ledger, cross-process over QUIC. ---
    let admin = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let aproof = format!("AdminProof-{suffix}");
    // [AD0] a character for the admin table to render (through G -> A).
    if let Some(tok) = token.clone() {
        let acid = create_character(ctx, &g, &tok, &aproof).await;
        p.check("[AD0] admin-proof character created", acid.is_some(), format!("id={acid:?}"));
    }
    // [AD1] unauthenticated /admin -> 303 to /admin/login (session gate live on E).
    let ad1 = ctx.http_noredirect.get(format!("{g}/admin")).send().await?;
    let ad1_loc = ad1.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    p.check(
        "[AD1] unauthenticated /admin -> 303 /admin/login",
        ad1.status().as_u16() == 303 && ad1_loc.ends_with("/admin/login"),
        format!("code={} loc={ad1_loc}", ad1.status().as_u16()),
    );

    // [AD2] asymmetric lockout: prooflock 6x wrong -> each 401; user locks at 5, ip not.
    sqlx::query("DELETE FROM admin.login_attempts WHERE subject='user:prooflock' OR subject LIKE 'ip:%'")
        .execute(pool).await.ok();
    let mut ad2_all401 = true;
    for i in 0..6 {
        let pw = format!("wrong-{i}");
        let r = ctx.http_noredirect.post(format!("{g}/admin/login"))
            .form(&[("username", "prooflock"), ("password", pw.as_str())]).send().await?;
        if r.status().as_u16() != 401 { ad2_all401 = false; }
    }
    let ad2_fails: Option<i64> = sqlx::query_scalar("SELECT fails::bigint FROM admin.login_attempts WHERE subject='user:prooflock'").fetch_optional(pool).await.ok().flatten();
    let ad2_locked: Option<bool> = sqlx::query_scalar("SELECT locked_until > now() FROM admin.login_attempts WHERE subject='user:prooflock'").fetch_optional(pool).await.ok().flatten();
    let ad2_ip_locked: Option<i64> = sqlx::query_scalar("SELECT count(*) FROM admin.login_attempts WHERE subject LIKE 'ip:%' AND locked_until > now()").fetch_optional(pool).await.ok().flatten();
    p.check(
        "[AD2] user locks at 5, ip does not",
        ad2_all401 && ad2_fails.map(|f| f >= 5).unwrap_or(false) && ad2_locked == Some(true) && ad2_ip_locked == Some(0),
        format!("all401={ad2_all401} fails={ad2_fails:?} locked={ad2_locked:?} ip_locked={ad2_ip_locked:?}"),
    );

    // [AD2b] 12 CONCURRENT wrong logins -> advisory-lock serializes to exactly 5 fails +
    // one login-locked event (the flow that HUNG the bash harness; deadlock-free in tokio).
    // Hit admin-svc DIRECTLY (:8085, which trusts XFF from 127.0.0.1): this exercises the
    // same lockout logic without the gateway's per-IP rate limiter — the harness fires
    // truly concurrently and would otherwise trip the gateway's 127.0.0.1 bucket, which
    // the slower curl-per-process shell never hit.
    let admin_direct = format!(
        "http://127.0.0.1:{}/admin/login",
        ctx.http_port("admin-svc")
    );
    // A long-timeout client for the concurrent admin bursts: each login holds the
    // advisory lock across a 64 MiB Argon2 (~300-500ms) and 12/40 requests serialize,
    // so the tail can take several seconds — well past the 5s default (the curl-per-
    // process shell never saw this because process-spawn latency spread its requests).
    let slow = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    // NB: sqlx's extended protocol runs only ONE statement per query() — split the two.
    sqlx::query("DELETE FROM admin.login_attempts WHERE subject IN ('user:prooflock','ip:198.51.100.42')")
        .execute(pool).await.ok();
    sqlx::query("DELETE FROM asyncevents.events WHERE topic='admin.action' AND payload->>'actor'='prooflock' AND payload->>'action'='login-locked'")
        .execute(pool).await.ok();
    let mut hs = Vec::new();
    for i in 0..12 {
        let http = slow.clone();
        let url = admin_direct.clone();
        hs.push(tokio::spawn(async move {
            let pw = format!("wrong-{i}");
            http.post(url).header("X-Forwarded-For", "198.51.100.42")
                .form(&[("username", "prooflock"), ("password", pw.as_str())]).send().await
                .map(|r| r.status().as_u16()).unwrap_or(0)
        }));
    }
    let mut ad2b_codes = Vec::new();
    for h in hs { if let Ok(c) = h.await { ad2b_codes.push(c); } }
    ad2b_codes.sort_unstable();
    let ad2b_fails: Option<i64> = sqlx::query_scalar("SELECT fails::bigint FROM admin.login_attempts WHERE subject='user:prooflock'").fetch_optional(pool).await.ok().flatten();
    let ad2b_locked: Option<bool> = sqlx::query_scalar("SELECT locked_until > now() FROM admin.login_attempts WHERE subject='user:prooflock'").fetch_optional(pool).await.ok().flatten();
    let ad2b_ev: Option<i64> = sqlx::query_scalar("SELECT count(*) FROM asyncevents.events WHERE topic='admin.action' AND payload->>'actor'='prooflock' AND payload->>'action'='login-locked'").fetch_optional(pool).await.ok().flatten();
    p.check(
        "[AD2b] concurrent lockout -> fails=5, one lock event",
        ad2b_fails == Some(5) && ad2b_locked == Some(true) && ad2b_ev == Some(1),
        format!("fails={ad2b_fails:?} locked={ad2b_locked:?} ev={ad2b_ev:?} codes={ad2b_codes:?}"),
    );

    // [AD2c] 40 CONCURRENT logins from one IP -> some 429, each carrying Retry-After: 1.
    let mut hs = Vec::new();
    for i in 0..40 {
        let http = slow.clone();
        let url = admin_direct.clone();
        hs.push(tokio::spawn(async move {
            let user = format!("ghost-{i}");
            match http.post(url).header("X-Forwarded-For", "198.51.100.43")
                .form(&[("username", user.as_str()), ("password", "wrong")]).send().await
            {
                Ok(r) => {
                    let code = r.status().as_u16();
                    let ra = r.headers().get("retry-after").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                    (code, ra)
                }
                Err(_) => (0, String::new()),
            }
        }));
    }
    let (mut n429, mut n429_retry) = (0u32, 0u32);
    for h in hs {
        if let Ok((code, ra)) = h.await {
            if code == 429 {
                n429 += 1;
                if ra == "1" { n429_retry += 1; }
            }
        }
    }
    p.check(
        "[AD2c] login burst -> 429 + Retry-After: 1",
        n429 >= 1 && n429 == n429_retry,
        format!("429={n429} retry={n429_retry}"),
    );

    // [AD3] session login -> 303 + admin_session cookie (AD3a proves the cookie works).
    let ad3 = admin.post(format!("{g}/admin/login")).form(&[("username", "proofadmin"), ("password", "proofpass")]).send().await?;
    p.check("[AD3] admin login -> 303 + session", ad3.status().as_u16() == 303, ad3.status());
    // [AD3a] /admin/characters WITH session -> 200 + AProof (G passthrough -> E -> A QUIC).
    let ad3a = admin.get(format!("{g}/admin/characters")).send().await?;
    let (ad3a_code, ad3a_body) = (ad3a.status().as_u16(), ad3a.text().await.unwrap_or_default());
    p.check("[AD3a] /admin/characters -> 200 + AProof", ad3a_code == 200 && ad3a_body.contains(&aproof), format!("code={ad3a_code}"));
    // [AD3b] /admin/api-keys WITH session -> 200 + dev-client (E -> L QUIC, two hops).
    let ad3b = admin.get(format!("{g}/admin/api-keys")).send().await?;
    let (ad3b_code, ad3b_body) = (ad3b.status().as_u16(), ad3b.text().await.unwrap_or_default());
    p.check("[AD3b] /admin/api-keys -> 200 + dev-client", ad3b_code == 200 && ad3b_body.contains("dev-client"), format!("code={ad3b_code}"));
    // [AD4] POST /admin/api-keys with session but NO _csrf -> 403 (CSRF before editability).
    let ad4 = admin.post(format!("{g}/admin/api-keys")).form(&[("dummy", "1")]).send().await?;
    p.check("[AD4] no-CSRF admin POST -> 403", ad4.status().as_u16() == 403, ad4.status());
    // [AD5] admin.action durable trail: >=2 asyncevents rows AND audit.log has them.
    let ad5_events: Option<i64> = sqlx::query_scalar("SELECT count(*) FROM asyncevents.events WHERE topic='admin.action'").fetch_optional(pool).await.ok().flatten();
    let ad5_audit: Option<i64> = sqlx::query_scalar("SELECT count(*) FROM audit.log WHERE topic='admin.action'").fetch_optional(pool).await.ok().flatten();
    p.check(
        "[AD5] admin.action durable trail",
        ad5_events.map(|e| e >= 2).unwrap_or(false) && ad5_audit.map(|a| a >= 1).unwrap_or(false),
        format!("events={ad5_events:?} audit={ad5_audit:?}"),
    );

    // --- Audit ledger (F pulls six subscriptions from the shared log). ---
    // [AU1] character.created + character.deleted recorded for the Batch B character.
    if let Some(cid) = &created_cid {
        let created = poll_count(pool, "SELECT count(*) FROM audit.log WHERE topic='character.created' AND payload->>'character_id'=$1", cid, 1).await;
        let deleted = poll_count(pool, "SELECT count(*) FROM audit.log WHERE topic='character.deleted' AND payload->>'character_id'=$1", cid, 1).await;
        p.check("[AU1] audit character.created + deleted", created && deleted, format!("cid={cid}"));
    }
    // [AU2] player.registered recorded for the registered player.
    if let Some(pid) = &player_id {
        let reg = poll_count(pool, "SELECT count(*) FROM audit.log WHERE topic='player.registered' AND payload->>'player_id'=$1", pid, 1).await;
        p.check("[AU2] audit player.registered", reg, format!("pid={pid}"));
    }
    // [AU3] /admin/audit-log WITH session -> 200 + a logged topic (E -> F QUIC).
    let au3 = admin.get(format!("{g}/admin/audit-log")).send().await?;
    let (au3_code, au3_body) = (au3.status().as_u16(), au3.text().await.unwrap_or_default());
    let au3_ok = au3_code == 200
        && (au3_body.contains("character.created") || au3_body.contains("character.deleted") || au3_body.contains("player.registered"));
    p.check("[AU3] /admin/audit-log renders ledger", au3_ok, format!("code={au3_code}"));

    // --- Scheduler: data-driven schedule fires durably; audit pulls scheduler.fired. ---
    // [SC0] seed an immediately-due 2s schedule (epoch last_fired).
    sqlx::query("DELETE FROM asyncevents.events WHERE topic='scheduler.fired' AND payload->>'name'='proof-tick'").execute(pool).await.ok();
    sqlx::query("INSERT INTO scheduler.schedules (name, interval_seconds, last_fired) VALUES ('proof-tick', 2, to_timestamp(0)) ON CONFLICT (name) DO UPDATE SET interval_seconds=2, last_fired=to_timestamp(0)").execute(pool).await.ok();
    // [SC1] proof-tick fires durably AND audit's prune subscription cursor advances past it.
    let sc = {
        let mut ok = false;
        for _ in 0..30 {
            let fired: Option<i64> = sqlx::query_scalar("SELECT count(*) FROM asyncevents.events WHERE topic='scheduler.fired' AND payload->>'name'='proof-tick'").fetch_optional(pool).await.ok().flatten();
            let consumed: Option<i64> = sqlx::query_scalar("SELECT count(*) FROM asyncevents.subscriptions s, asyncevents.events e WHERE s.subscription_id='audit.prune-on-scheduler.v1' AND e.topic='scheduler.fired' AND e.payload->>'name'='proof-tick' AND (s.cursor_generation, s.cursor_xid, s.cursor_tie) >= (e.generation, e.producer_xid, e.tie_breaker)").fetch_optional(pool).await.ok().flatten();
            if fired.map(|f| f >= 1).unwrap_or(false) && consumed.map(|c| c >= 1).unwrap_or(false) {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        ok
    };
    p.check("[SC1] scheduler.fired proof-tick + audit cursor advanced", sc, "");

    // --- Session prune: scheduler fires accounts-sessions-prune; D prunes on delivery. ---
    let sp_token = format!("prune-proof-{suffix}");
    // [SP0] plant a throwaway player + an EXPIRED session (FK needs a real player).
    let sp_pid: Option<String> = sqlx::query_scalar("INSERT INTO accounts.players (display_name) VALUES ($1) RETURNING id::text")
        .bind(format!("prune-proof-{suffix}")).fetch_optional(pool).await.ok().flatten();
    if let Some(pid) = &sp_pid {
        sqlx::query("INSERT INTO accounts.sessions (token, player_id, expires_at) VALUES ($1, $2::uuid, now() - interval '1 day')")
            .bind(&sp_token).bind(pid).execute(pool).await.ok();
        // [SP1] force the seeded prune schedule due NOW.
        sqlx::query("UPDATE scheduler.schedules SET last_fired = to_timestamp(0) WHERE name = 'accounts-sessions-prune'").execute(pool).await.ok();
        // [SP2] poll until D's prune handler removes the expired row (durable H -> D).
        let sp = poll_count(pool, "SELECT count(*) FROM accounts.sessions WHERE token=$1", &sp_token, 0).await;
        p.check("[SP2] expired session pruned (scheduler -> accounts)", sp, "");
    } else {
        p.check("[SP0] plant throwaway player", false, "insert failed");
    }

    // --- Metrics ---
    // [MX1] characters-svc /metrics -> 200 + http_requests_total (one recorded hit first).
    let characters_port = ctx.http_port("characters-svc");
    let _ = ctx.http.get(format!("http://127.0.0.1:{characters_port}/__metrics_probe")).send().await;
    let mx1 = ctx.http.get(format!("http://127.0.0.1:{characters_port}/metrics")).send().await?;
    let (mx1c, mx1b) = (mx1.status().as_u16(), mx1.text().await.unwrap_or_default());
    p.check("[MX1] characters-svc /metrics -> http_requests_total", mx1c == 200 && mx1b.contains("http_requests_total"), format!("code={mx1c}"));
    // [MX2] gateway-svc /metrics -> 200 + a per-op route label.
    let mx2 = ctx.http.get(format!("{g}/metrics")).send().await?;
    let (mx2c, mx2b) = (mx2.status().as_u16(), mx2.text().await.unwrap_or_default());
    p.check("[MX2] gateway-svc /metrics -> http_requests_total + route label", mx2c == 200 && mx2b.contains("http_requests_total") && mx2b.contains("/leaderboard"), format!("code={mx2c}"));

    // --- Rate limiting (gateway always-on 20rps/burst40; /healthz SkipInfra). ---
    // [RL1] 60 parallel /leaderboard -> >=1 429.
    let rl1 = burst_429(ctx, &format!("{g}/leaderboard"), Some("dev-key-client"), 60).await;
    p.check("[RL1] 60 parallel /leaderboard -> >=1 429", rl1 >= 1, format!("429={rl1}"));
    // [RL2] 60 parallel /healthz -> 0 429 (SkipInfra holds).
    let rl2 = burst_429(ctx, &format!("{g}/healthz"), None, 60).await;
    p.check("[RL2] 60 parallel /healthz -> 0 429", rl2 == 0, format!("429={rl2}"));
    // [RL3] pause -> bucket refills -> 200.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let rl3 = ctx.http.get(format!("{g}/leaderboard")).header("X-Api-Key", "dev-key-client").send().await?;
    p.check("[RL3] post-pause /leaderboard -> 200", rl3.status().as_u16() == 200, rl3.status());

    Ok(())
}

/// `[I-GATE]` — proves Step 1's impl-side `INVENTORY_DEV_GRANT` guard live in the
/// split, where `assertions` (fleet-wide `INVENTORY_DEV_GRANT=1`) structurally cannot:
/// drop ONLY the running inventory-svc, respawn it from the canonical named spec with
/// the flag stripped out, and prove a FULLY-AUTHED grant still 404s through gateway-svc.
async fn i_gate(ctx: &Ctx, fleet: &mut Vec<Running>, p: &mut Proof) -> Result<()> {
    println!("\n[splitproof] === [I-GATE] restart inventory-svc WITHOUT INVENTORY_DEV_GRANT ===");
    let idx = fleet
        .iter()
        .position(|running| running.name == "inventory-svc")
        .context("inventory-svc missing from fleet (preflight_fleet should have caught this)")?;

    // Kill only inventory-svc (Drop kills + waits) and give the OS a moment to free
    // its HTTP + edge ports before rebinding — gateway-svc's `remote::Stub` re-resolves
    // the peer on its next dial, so this restart is transparent to the front door.
    fleet.remove(idx);
    tokio::time::sleep(Duration::from_millis(800)).await;

    let original = ctx.service("inventory-svc");
    let env: BTreeMap<String, String> = original
        .env
        .iter()
        .filter(|(key, _)| key.as_str() != "INVENTORY_DEV_GRANT")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let mut restarted = original.clone();
    restarted.env = env;
    println!("[splitproof] restarting {} on :{} without the dev-grant flag ...", restarted.name, restarted.http_port);
    let running = ctx.spawn(&restarted)?;
    ctx.wait_healthy(&restarted).await?;
    fleet.insert(idx, running);
    println!("[splitproof] {} healthy (dev-grant OFF)", restarted.name);

    // A FULLY-AUTHED caller (real X-Api-Key + real player bearer, per M1 an unauthed
    // call is now 401) still gets 404 — the impl guard, not a key/auth failure.
    //
    // gateway-svc's cached `Reconnecting` conn to inventory-svc has no way to learn
    // its old peer died until it actually tries the dead connection (QUIC is UDP —
    // there is no TCP RST). `grant` is RetryMode::Never (a mutation), so the FIRST
    // post-restart call may transport-fail or hang past our client timeout while that
    // dead conn is detected and reset; only the call AFTER that redials fresh and
    // reaches the new process. Poll instead of asserting on a single shot.
    let g = format!("http://127.0.0.1:{}", ctx.http_port("gateway-svc"));
    let email = format!("igate-{}@test.local", std::process::id());
    let token = register_login(ctx, &g, &email).await.context("i-gate register/login")?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last: Option<u16> = None;
    let mut ok = false;
    loop {
        if let Ok(r) = ctx
            .http
            .post(format!("{g}/inventory/me/grant"))
            .header("X-Api-Key", "dev-key-client")
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({"item_id": "coin", "qty": 1}))
            .send()
            .await
        {
            // Err(_) falls through: transient — gateway's cached conn to the killed
            // process is dying and hasn't been reset+redialed yet.
            let code = r.status().as_u16();
            last = Some(code);
            if code == 404 {
                ok = true;
                break;
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    p.check(
        "[I-GATE] fully-authed grant -> 404 with INVENTORY_DEV_GRANT off",
        ok,
        format!("last_code={last:?}"),
    );
    Ok(())
}

/// After a config change, create fresh characters until one is granted `needle` (the
/// grant spec reloads eventually-consistently, so early characters may still get the
/// old item).
async fn poll_fresh_grant(ctx: &Ctx, g: &str, token: &str, name: &str, needle: &str) -> bool {
    for _ in 0..30 {
        if let Some(cc) = create_character(ctx, g, token, name).await {
            for _ in 0..4 {
                let (_, b) = inventory_of(ctx, g, token, &cc).await;
                if b.contains(needle) {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    false
}

async fn current_revision(pool: &PgPool) -> Option<i64> {
    sqlx::query("SELECT revision FROM config.revision")
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get::<i64, _>("revision").ok())
}

/// Register + login a player through the gateway front, returning the bearer.
/// Retries past a transient gateway 429 (see `create_character`).
async fn register_login(ctx: &Ctx, g: &str, email: &str) -> Result<String> {
    for _ in 0..15 {
        let reg = ctx.http.post(format!("{g}/accounts/register"))
            .header("X-Api-Key", "dev-key-client")
            .json(&serde_json::json!({"email": email, "password": "pw", "displayName": "P"}))
            .send().await?;
        if reg.status().as_u16() == 429 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }
        break;
    }
    for _ in 0..15 {
        let login = ctx.http.post(format!("{g}/accounts/login"))
            .header("X-Api-Key", "dev-key-client")
            .json(&serde_json::json!({"email": email, "password": "pw"}))
            .send().await?;
        if login.status().as_u16() == 429 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }
        let body: serde_json::Value = login.json().await.unwrap_or(serde_json::Value::Null);
        return body.get("token").and_then(|v| v.as_str()).map(str::to_string).context("no token from login");
    }
    bail!("login rate-limited out")
}

/// Create a character through G -> A, returning its id. Retries on the gateway's
/// always-on 429 (the harness drives requests far faster than the curl-per-process
/// shell, so a preceding burst can transiently empty the 127.0.0.1 token bucket).
async fn create_character(ctx: &Ctx, g: &str, token: &str, name: &str) -> Option<String> {
    for _ in 0..15 {
        let r = ctx
            .http
            .post(format!("{g}/characters"))
            .header("X-Api-Key", "dev-key-client")
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({"name": name, "class": "mage"}))
            .send()
            .await
            .ok()?;
        match r.status().as_u16() {
            429 => {
                tokio::time::sleep(Duration::from_millis(300)).await;
                continue;
            }
            201 => {
                let body: serde_json::Value = r.json().await.ok()?;
                return body.get("id").and_then(|v| v.as_str()).map(str::to_string);
            }
            _ => return None,
        }
    }
    None
}

/// GET a character's inventory through G -> B: (status, body).
async fn inventory_of(ctx: &Ctx, g: &str, token: &str, cid: &str) -> (u16, String) {
    match ctx
        .http
        .get(format!("{g}/inventory/character/{cid}"))
        .header("X-Api-Key", "dev-key-client")
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
    {
        Ok(r) => {
            let c = r.status().as_u16();
            (c, r.text().await.unwrap_or_default())
        }
        Err(_) => (0, String::new()),
    }
}

/// Poll a character's inventory (through G) until its body contains `needle`.
async fn poll_inventory_has(ctx: &Ctx, g: &str, token: &str, cid: &str, needle: &str) -> bool {
    for _ in 0..30 {
        let (code, body) = inventory_of(ctx, g, token, cid).await;
        if code == 200 && body.contains(needle) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

/// Poll a scalar count query until it equals `want`.
async fn poll_count(pool: &PgPool, sql: &str, cid: &str, want: i64) -> bool {
    for _ in 0..30 {
        let n: Option<i64> = sqlx::query_scalar(sql).bind(cid).fetch_optional(pool).await.ok().flatten();
        if n == Some(want) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

async fn player_call(
    ctx: &Ctx,
    token: Option<&str>,
    method: &str,
    payload: &str,
) -> Result<serde_json::Value> {
    let ca = ctx.ca_cert.to_str().context("CA cert path not UTF-8")?;
    let trust = DevCA::load_cert_only(ca).map_err(|e| anyhow::anyhow!("load CA: {e}"))?;
    let addr = format!("127.0.0.1:{}", ctx.player_port()).parse().context("player addr")?;
    let client = PlayerClient::dial(addr, &trust)
        .await
        .map_err(|e| anyhow::anyhow!("dial: {e}"))?;
    let resp = client
        .call(method, token, Some("dev-key-client"), payload.as_bytes())
        .await
        .map_err(|e| anyhow::anyhow!("call: {e}"))?;
    Ok(serde_json::from_slice(&resp).unwrap_or(serde_json::Value::Null))
}

/// Recursively finds the first `"id": "<string>"` field in a JSON value (the QUIC
/// characters.create envelope nests the created character under a status wrapper).
fn find_id(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Object(m) => {
            if let Some(id) = m.get("id").and_then(|x| x.as_str()) {
                return Some(id.to_string());
            }
            m.values().find_map(find_id)
        }
        serde_json::Value::Array(a) => a.iter().find_map(find_id),
        _ => None,
    }
}

/// POST a match report (server key) and return the HTTP status. Retries past a
/// transient gateway 429 (see `create_character`).
async fn report(ctx: &Ctx, g: &str, rid: &str, winner: &str, loser: &str) -> u16 {
    for _ in 0..15 {
        let code = match ctx
            .http
            .post(format!("{g}/match/report"))
            .header("X-Api-Key", "dev-key-server")
            .json(&serde_json::json!({"ReportId": rid, "Winner": winner, "Loser": loser}))
            .send()
            .await
        {
            Ok(r) => r.status().as_u16(),
            Err(_) => 0,
        };
        if code == 429 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }
        return code;
    }
    429
}

/// Poll the leaderboard (through G) until `winner` shows exactly `wins` wins.
async fn poll_leaderboard_wins(ctx: &Ctx, g: &str, winner: &str, wins: u32) -> bool {
    let needle = format!("\"player\":\"{winner}\",\"wins\":{wins}");
    for _ in 0..30 {
        if let Ok(r) = ctx
            .http
            .get(format!("{g}/leaderboard"))
            .header("X-Api-Key", "dev-key-client")
            .send()
            .await
        {
            if r.text().await.unwrap_or_default().contains(&needle) {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

/// Fire `n` concurrent GETs at `url` (optional api key) and return how many got 429.
async fn burst_429(ctx: &Ctx, url: &str, api_key: Option<&str>, n: u32) -> u32 {
    let mut hs = Vec::new();
    for _ in 0..n {
        let http = ctx.http.clone();
        let url = url.to_string();
        let key = api_key.map(|s| s.to_string());
        hs.push(tokio::spawn(async move {
            let mut req = http.get(url);
            if let Some(k) = key {
                req = req.header("X-Api-Key", k);
            }
            req.send().await.map(|r| r.status().as_u16()).unwrap_or(0)
        }));
    }
    let mut n429 = 0;
    for h in hs {
        if let Ok(429) = h.await {
            n429 += 1;
        }
    }
    n429
}

/// True if a player call's DOMAIN status equals `want` (auth/routing failures ride the
/// Ok envelope as `{"status":"..."}`), or a transport Err mentions it.
fn status_or_err(r: &Result<serde_json::Value>, want: &str) -> bool {
    match r {
        Ok(v) => v.get("status").and_then(|s| s.as_str()) == Some(want),
        Err(e) => e.to_string().contains(want),
    }
}

/// [P6] one persistent player connection fires a 22-call burst (per-connection limit is
/// burst 20): the burst must be rate-limited at least once, then — after a refill pause
/// before the last call — succeed again (>=21 Ok). Proves the limiter is per-connection
/// and transient, not sticky.
async fn player_burst(ctx: &Ctx) -> bool {
    let Some(ca) = ctx.ca_cert.to_str() else { return false };
    let Ok(trust) = DevCA::load_cert_only(ca) else { return false };
    let Ok(addr) = format!("127.0.0.1:{}", ctx.player_port()).parse() else { return false };
    let Ok(client) = PlayerClient::dial(addr, &trust).await else { return false };
    let mut ok = 0u32;
    let mut limited = false;
    for i in 0..22 {
        if i == 21 {
            tokio::time::sleep(Duration::from_millis(2000)).await;
        }
        match client.call("leaderboard.topScores", None, Some("dev-key-client"), b"{}").await {
            Ok(resp) => {
                let is_ok = serde_json::from_slice::<serde_json::Value>(&resp)
                    .ok()
                    .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(|s| s == "Ok"))
                    .unwrap_or(false);
                if is_ok {
                    ok += 1;
                }
            }
            Err(e) => {
                if e.to_string().contains("rate limit") {
                    limited = true;
                }
            }
        }
    }
    limited && ok >= 21
}
