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
    /// A client that does NOT follow redirects, so a 303 (epic callback, admin login)
    /// is observable as a status + Location instead of being transparently chased.
    http_noredirect: reqwest::Client,
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
            .timeout(Duration::from_secs(5))
            .build()?,
        http_noredirect: reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
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

    // Seed the admin logins PRE-BOOT (session auth): adminctl ensures schema `admin`
    // itself and upserts the login (password over stdin, never argv).
    seed_admin(&ctx, "proofadmin", "proofpass")?;
    seed_admin(&ctx, "prooflock", "lockpass")?;

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

/// Seed an admin login via adminctl (password over stdin, never argv). adminctl ensures
/// schema `admin` + admin.users itself, so it runs before admin-svc migrates.
fn seed_admin(ctx: &Ctx, user: &str, pass: &str) -> Result<()> {
    use std::io::Write;
    let bin = ctx.bin_dir.join(format!("adminctl{}", std::env::consts::EXE_SUFFIX));
    let mut child = Command::new(&bin)
        .args(["create-user", user, "--password-stdin"])
        .env("DATABASE_URL", &ctx.db_url)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("spawn adminctl for {user}"))?;
    child
        .stdin
        .take()
        .context("adminctl stdin")?
        .write_all(format!("{pass}\n").as_bytes())?;
    if !child.wait()?.success() {
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
    // [EP1] start -> authorize_url carrying a state param.
    let ep1 = ctx.http.post(format!("{g}/accounts/epic/start")).send().await?;
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
        let ep2 = ctx
            .http_noredirect
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
    let admin_direct = format!("http://127.0.0.1:{P_ADMIN}/admin/login");
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
async fn register_login(ctx: &Ctx, g: &str, email: &str) -> Result<String> {
    ctx.http
        .post(format!("{g}/accounts/register"))
        .header("X-Api-Key", "dev-key-client")
        .json(&serde_json::json!({"email": email, "password": "pw", "displayName": "P"}))
        .send()
        .await?;
    let login = ctx
        .http
        .post(format!("{g}/accounts/login"))
        .header("X-Api-Key", "dev-key-client")
        .json(&serde_json::json!({"email": email, "password": "pw"}))
        .send()
        .await?;
    let body: serde_json::Value = login.json().await.unwrap_or(serde_json::Value::Null);
    body.get("token")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .context("no token from login")
}

/// Create a character through G -> A, returning its id.
async fn create_character(ctx: &Ctx, g: &str, token: &str, name: &str) -> Option<String> {
    let r = ctx
        .http
        .post(format!("{g}/characters"))
        .header("X-Api-Key", "dev-key-client")
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"name": name, "class": "mage"}))
        .send()
        .await
        .ok()?;
    if r.status().as_u16() != 201 {
        return None;
    }
    let body: serde_json::Value = r.json().await.ok()?;
    body.get("id").and_then(|v| v.as_str()).map(str::to_string)
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
    let addr = format!("127.0.0.1:{PLAYER_PORT}").parse().context("player addr")?;
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

/// POST a match report (server key) and return the HTTP status.
async fn report(ctx: &Ctx, g: &str, rid: &str, winner: &str, loser: &str) -> u16 {
    match ctx
        .http
        .post(format!("{g}/match/report"))
        .header("X-Api-Key", "dev-key-server")
        .json(&serde_json::json!({"ReportId": rid, "Winner": winner, "Loser": loser}))
        .send()
        .await
    {
        Ok(r) => r.status().as_u16(),
        Err(_) => 0,
    }
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
    let Ok(addr) = format!("127.0.0.1:{PLAYER_PORT}").parse() else { return false };
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
