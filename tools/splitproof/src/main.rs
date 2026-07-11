//! Cross-platform split-proof harness — replaces `split-proof.sh` / `split-proof.ps1`.
//!
//! The shell harnesses are structurally fragile on Windows (PowerShell native-arg
//! quote-stripping, MSYS `wait` hangs, winctrl exit-code false-throws). This harness
//! removes the shell entirely: the 12-svc fleet is spawned via `std::process::Command`
//! with a TYPED env map and a kill-on-drop guard, health-checked over `reqwest`,
//! DB-asserted via `sqlx`, and the player QUIC front driven through the `edge` crate as
//! a library. No `curl.exe`, no `psql.exe`, no `playercli.exe`, no `winctrl`.
//!
//! MVP scope: boot the fleet + a core assertion set (auth, key-verifier shed, config
//! large-value, QUIC create, leaderboard). Full named-assertion parity + the
//! graceful-shutdown / monolith-parity proofs are follow-ups. See
//! docs/plans/2026-07-11-1730-rust-splitproof-harness-plan.md.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use edge::{DevCA, PlayerClient};
use sqlx::{PgPool, Row};

const DEFAULT_DB: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

// HTTP ports (CLAUDE.md fleet map).
const P_CHARACTERS: u16 = 8080;
const P_INVENTORY: u16 = 8081;
const P_GATEWAY: u16 = 8082;
const P_CONFIG: u16 = 8083;
const P_ACCOUNTS: u16 = 8084;
const P_ADMIN: u16 = 8085;
const P_AUDIT: u16 = 8086;
const P_SCHEDULER: u16 = 8087;
const P_MATCH: u16 = 8088;
const P_RATING: u16 = 8089;
const P_LEADERBOARD: u16 = 8090;
const P_APIKEYS: u16 = 8091;
// Internal mTLS edge ports.
const E_CHARACTERS: u16 = 9000;
const E_INVENTORY: u16 = 9001;
const E_CONFIG: u16 = 9002;
const E_ACCOUNTS: u16 = 9003;
const E_AUDIT: u16 = 9004;
const E_SCHEDULER: u16 = 9005;
const E_MATCH: u16 = 9006;
const E_RATING: u16 = 9007;
const E_LEADERBOARD: u16 = 9008;
const E_APIKEYS: u16 = 9009;
const PLAYER_PORT: u16 = 9100;

/// A fleet member: its binary name, HTTP readiness port, and the exact env map the
/// composition root reads (topology wiring lives ONLY here, like the cmd/* mains).
struct Svc {
    name: &'static str,
    http_port: u16,
    env: Vec<(String, String)>,
}

/// Kills the child on drop, so a panic or an early `?` return tears the whole fleet
/// down — there is no code path that leaves an orphaned `-svc` process behind (the
/// exact failure mode that plagued the winctrl harness).
struct Running {
    child: Child,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Ctx {
    bin_dir: PathBuf,
    run_dir: PathBuf,
    ca_cert: PathBuf,
    ca_key: PathBuf,
    db_url: String,
    http: reqwest::Client,
}

fn edge_addr(port: u16) -> (String, String) {
    ("EDGE_ADDR".into(), format!(":{port}"))
}

fn peer(name: &str, port: u16) -> (String, String) {
    (format!("{name}_EDGE_ADDR"), format!("127.0.0.1:{port}"))
}

impl Ctx {
    fn base_env(&self) -> Vec<(String, String)> {
        vec![
            ("DATABASE_URL".into(), self.db_url.clone()),
            ("EDGE_CA_CERT".into(), self.ca_cert.display().to_string()),
            ("EDGE_CA_KEY".into(), self.ca_key.display().to_string()),
        ]
    }

    /// The boot ORDER mirrors the dependency graph the scripts use: providers before
    /// their consumers, the front door (gateway) after every peer it dials.
    fn fleet(&self) -> Vec<Svc> {
        let svc = |name, http_port, edge_port: Option<u16>, extra: Vec<(String, String)>| {
            let mut env = self.base_env();
            env.push(("PORT".into(), format!(":{http_port}")));
            if let Some(e) = edge_port {
                env.push(edge_addr(e));
            }
            env.extend(extra);
            Svc { name, http_port, env }
        };
        vec![
            svc("accounts-svc", P_ACCOUNTS, Some(E_ACCOUNTS), vec![
                ("ACCOUNTS_DEV_AUTH".into(), "1".into()),
                ("EPIC_CLIENT_ID".into(), "test".into()),
                ("EPIC_CLIENT_SECRET".into(), "test".into()),
                ("EPIC_TOKEN_URL".into(), "http://127.0.0.1:1/token".into()),
            ]),
            svc("apikeys-svc", P_APIKEYS, Some(E_APIKEYS), vec![
                ("APIKEYS_DEV_SEED".into(), "1".into()),
            ]),
            svc("audit-svc", P_AUDIT, Some(E_AUDIT), vec![]),
            svc("scheduler-svc", P_SCHEDULER, Some(E_SCHEDULER), vec![]),
            svc("rating-svc", P_RATING, Some(E_RATING), vec![]),
            svc("leaderboard-svc", P_LEADERBOARD, Some(E_LEADERBOARD), vec![]),
            svc("match-svc", P_MATCH, Some(E_MATCH), vec![peer("RATING", E_RATING)]),
            svc("characters-svc", P_CHARACTERS, Some(E_CHARACTERS), vec![]),
            svc("config-svc", P_CONFIG, Some(E_CONFIG), vec![]),
            svc("inventory-svc", P_INVENTORY, Some(E_INVENTORY), vec![
                peer("CHARACTERS", E_CHARACTERS),
                peer("CONFIG", E_CONFIG),
                ("INVENTORY_DEV_GRANT".into(), "1".into()),
            ]),
            // gateway-svc: without_db (no DATABASE_URL), only stubs; every op resolves
            // Remote over the mTLS edge. It also serves the player QUIC front.
            Svc {
                name: "gateway-svc",
                http_port: P_GATEWAY,
                env: {
                    let mut env = vec![
                        ("EDGE_CA_CERT".into(), self.ca_cert.display().to_string()),
                        ("EDGE_CA_KEY".into(), self.ca_key.display().to_string()),
                        ("PORT".into(), format!(":{P_GATEWAY}")),
                        ("PLAYER_EDGE_ADDR".into(), format!(":{PLAYER_PORT}")),
                        peer("CHARACTERS", E_CHARACTERS),
                        peer("INVENTORY", E_INVENTORY),
                        peer("ACCOUNTS", E_ACCOUNTS),
                        peer("MATCH", E_MATCH),
                        peer("LEADERBOARD", E_LEADERBOARD),
                        peer("APIKEYS", E_APIKEYS),
                        ("ADMIN_HTTP_ADDR".into(), format!("127.0.0.1:{P_ADMIN}")),
                        ("ACCOUNTS_HTTP_ADDR".into(), format!("127.0.0.1:{P_ACCOUNTS}")),
                    ];
                    env.sort_by(|a, b| a.0.cmp(&b.0));
                    env
                },
            },
            svc("admin-svc", P_ADMIN, None, vec![
                peer("CHARACTERS", E_CHARACTERS),
                peer("INVENTORY", E_INVENTORY),
                peer("CONFIG", E_CONFIG),
                peer("ACCOUNTS", E_ACCOUNTS),
                peer("AUDIT", E_AUDIT),
                peer("SCHEDULER", E_SCHEDULER),
                peer("APIKEYS", E_APIKEYS),
                ("TRUSTED_PROXY_CIDRS".into(), "127.0.0.1/32".into()),
                ("ADMIN_COOKIE_SECURE".into(), "0".into()),
            ]),
        ]
    }

    fn spawn(&self, svc: &Svc) -> Result<Running> {
        let bin = self
            .bin_dir
            .join(format!("{}{}", svc.name, std::env::consts::EXE_SUFFIX));
        if !bin.exists() {
            bail!("binary not found: {} (run `cargo build` first)", bin.display());
        }
        let out = File::create(self.run_dir.join(format!("{}.out.log", svc.name)))?;
        let err = File::create(self.run_dir.join(format!("{}.err.log", svc.name)))?;
        let child = Command::new(&bin)
            .envs(svc.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdout(out)
            .stderr(err)
            .spawn()
            .with_context(|| format!("spawn {}", svc.name))?;
        Ok(Running { child })
    }

    async fn wait_healthy(&self, svc: &Svc) -> Result<()> {
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

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
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

async fn run() -> Result<u32> {
    let (bin_dir, root) = workspace_dirs()?;
    let run_dir = root.join("run");
    std::fs::create_dir_all(&run_dir)?;
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DB.to_string());
    let ctx = Ctx {
        ca_cert: run_dir.join("edge-ca.crt"),
        ca_key: run_dir.join("edge-ca.key"),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()?,
        bin_dir,
        run_dir,
        db_url,
    };

    // Fleet-drift tripwire: the harness svc list must equal cmd/*-svc on disk.
    preflight_fleet(&root, &ctx)?;

    println!("[splitproof] minting shared edge dev CA -> {}", ctx.ca_cert.display());
    let ca_cert_str = ctx.ca_cert.to_str().context("CA cert path not UTF-8")?;
    let ca_key_str = ctx.ca_key.to_str().context("CA key path not UTF-8")?;
    DevCA::generate()
        .context("generate CA")?
        .write_pem(ca_cert_str, ca_key_str)
        .context("write CA")?;

    let pool = PgPool::connect(&ctx.db_url).await.context("connect DB")?;
    reset_config_baseline(&pool).await?;

    // Boot the fleet; each guard lives in `fleet` so a `?` below drops them all (kill).
    let all = ctx.fleet();
    let mut fleet: Vec<Running> = Vec::new();
    for svc in &all {
        println!("[splitproof] starting {} on :{} ...", svc.name, svc.http_port);
        // config-svc must boot AFTER the baseline reset (done above) so its first
        // snapshot is the default; the ordering in `fleet()` already places it late.
        fleet.push(ctx.spawn(svc)?);
        ctx.wait_healthy(svc).await?;
        println!("[splitproof] {} healthy", svc.name);
    }
    println!("[splitproof] fleet up: {}/{} processes healthy\n", fleet.len(), all.len());

    let mut p = Proof::default();
    assertions(&ctx, &pool, &mut p).await?;

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

fn preflight_fleet(root: &Path, ctx: &Ctx) -> Result<()> {
    let mut on_disk: Vec<String> = std::fs::read_dir(root.join("cmd"))?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with("-svc"))
        .collect();
    on_disk.sort();
    let mut booted: Vec<String> = ctx.fleet().iter().map(|s| s.name.to_string()).collect();
    booted.sort();
    if on_disk != booted {
        bail!(
            "fleet drift: cmd/*-svc on disk {:?} != harness fleet {:?} — add the missing svc's boot block + env",
            on_disk, booted
        );
    }
    println!("[splitproof] fleet preflight OK: {} svcs == cmd/*-svc on disk", booted.len());
    Ok(())
}

async fn reset_config_baseline(pool: &PgPool) -> Result<()> {
    // Inventory's starter must default to starter_sword so a later live change proves a
    // reload; proof.* rows from a prior run must not leak into assertions.
    sqlx::query(
        "DELETE FROM config.settings WHERE namespace='inventory' AND key='starter_item'; \
         DELETE FROM config.settings WHERE namespace='proof';",
    )
    .execute(pool)
    .await
    .ok(); // config schema may not exist yet on a truly fresh DB — best-effort.
    Ok(())
}

async fn assertions(ctx: &Ctx, pool: &PgPool, p: &mut Proof) -> Result<()> {
    let g = format!("http://127.0.0.1:{P_GATEWAY}");
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

    // [P1] QUIC player front: characters.create through gateway :9100 over the edge
    // lib (no playercli subprocess). Domain status must be "Ok".
    if let Some(tok) = &token {
        match player_call(ctx, tok, "characters.create", r#"{"name":"hero","class":""}"#).await {
            Ok(resp) => {
                let status = resp.get("status").and_then(|s| s.as_str()).unwrap_or("");
                p.check("[P1] QUIC characters.create -> Ok", status == "Ok", format!("status={status}"));
            }
            Err(e) => p.check("[P1] QUIC characters.create -> Ok", false, format!("transport error: {e}")),
        }
    }

    // [L1] leaderboard with a VALID key -> 200 (positive control for K5's negatives).
    let lb = ctx
        .http
        .get(format!("{g}/leaderboard"))
        .header("X-Api-Key", "dev-key-client")
        .send()
        .await?;
    p.check("[L1] leaderboard (valid key) -> 200", lb.status().as_u16() == 200, lb.status());

    Ok(())
}

async fn current_revision(pool: &PgPool) -> Option<i64> {
    sqlx::query("SELECT revision FROM config.revision")
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get::<i64, _>("revision").ok())
}

async fn player_call(ctx: &Ctx, token: &str, method: &str, payload: &str) -> Result<serde_json::Value> {
    let ca = ctx.ca_cert.to_str().context("CA cert path not UTF-8")?;
    let trust = DevCA::load_cert_only(ca).map_err(|e| anyhow::anyhow!("load CA: {e}"))?;
    let addr = format!("127.0.0.1:{PLAYER_PORT}").parse().context("player addr")?;
    let client = PlayerClient::dial(addr, &trust)
        .await
        .map_err(|e| anyhow::anyhow!("dial: {e}"))?;
    let resp = client
        .call(method, Some(token), Some("dev-key-client"), payload.as_bytes())
        .await
        .map_err(|e| anyhow::anyhow!("call: {e}"))?;
    Ok(serde_json::from_slice(&resp).unwrap_or(serde_json::Value::Null))
}
