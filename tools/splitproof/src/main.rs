//! Cross-platform split-proof harness — replacement for the retired shell harnesses.
//!
//! The shell harnesses are structurally fragile on Windows (PowerShell native-arg
//! quote-stripping, MSYS `wait` hangs, winctrl exit-code false-throws). This harness
//! removes the shell entirely: the 15-service fleet is spawned via `processctl`
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
    RolloutLock, ServiceSpec, ShutdownOutcome, ShutdownPolicy, SpawnSpec, WorkspaceLayout,
    PgSessionCapacity, HARNESS_RESERVE, SPLITPROOF_ASSERTION_POOL_MAX,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection as _, PgPool, Row};
use splitproof::{fleet_liveness, Running};

use crate::idp::Idp;
use crate::pushws::{Frame, PushClient};

mod idp;
mod pushws;

#[cfg(test)]
mod tests;


struct Ctx {
    layout: WorkspaceLayout,
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
        let bin = self.layout.binary("debug", svc.executable_package);
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

    /// Polls `/readyz` until success, but checks the just-spawned child's own liveness
    /// FIRST on every iteration (mirrors devctl's `wait_healthy` in
    /// tools/devctl/src/supervisor.rs). A stale listener left by a previous hung run
    /// can answer 200 on `svc.http_port` even though the NEW child already died on
    /// bind (EADDRINUSE) — checking `try_wait` before trusting the HTTP probe turns
    /// that into an immediate, loud failure instead of a proof running against old code.
    /// Liveness alone cannot prove the child OWNS the listener (a bind-failed child
    /// that hangs without exiting is invisible to `try_wait` while a stale listener
    /// answers 200) — `ensure_no_stale_listener`'s pre-spawn probe is the guard for
    /// that class.
    async fn wait_healthy(&self, svc: &ServiceSpec, child: &mut OwnedChild) -> Result<()> {
        let url = format!("http://127.0.0.1:{}/readyz", svc.http_port);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = child.try_wait()? {
                bail!(
                    "{} exited during startup with {status} (a stale listener may still answer on :{})",
                    svc.name,
                    svc.http_port
                );
            }
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

/// Pre-spawn stale-listener probe: before a service is spawned, its readyz port must
/// have NO listener — anything accepting a TCP connect there is a stale process from a
/// previous hung run (or an unrelated port conflict), and the health gate would then
/// probe the OLD listener while the NEW child dies or hangs on bind. Connection
/// refused is the good case. This closes the whole stale-listener class regardless of
/// what the new child later does (exit, hang, or anything in between).
fn ensure_no_stale_listener(svc_name: &str, port: u16) -> Result<()> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok() {
        bail!(
            "port :{port} already has a listener before spawn ({svc_name}) — stale \
             process from a previous run or port conflict; clean up and retry"
        );
    }
    Ok(())
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

/// Decodes the fixed HTML-escape set emitted by Minijinja. `&amp;` stays last so an
/// original literal entity such as `&quot;` round-trips instead of being decoded twice.
fn decode_html_attr(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#x2f;", "/")
        .replace("&amp;", "&")
}

/// Extract `<input name="X" value="Y">` pairs from an admin form page (for the M3b
/// no-op form resubmit — a tiny hand parser avoids a regex dep). Attribute values are
/// decoded as a browser would decode them before submitting the form.
fn extract_form_fields(html: &str) -> Vec<(String, String)> {
    let attr = |tag: &str, key: &str| -> Option<String> {
        let pat = format!("{key}=\"");
        let start = tag.find(&pat)? + pat.len();
        let end = tag[start..].find('"')? + start;
        Some(decode_html_attr(&tag[start..end]))
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
async fn monolith_parity(ctx: &Ctx, pool: &PgPool, idp: &Idp, p: &mut Proof) -> Result<()> {
    println!("\n[splitproof] === MONOLITH PARITY (cmd/server, all Local) ===");
    sqlx::query("DELETE FROM admin.sessions").execute(pool).await.ok();
    sqlx::query("DELETE FROM admin.login_attempts").execute(pool).await.ok();
    let bin = ctx.layout.binary("debug", "server");
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

    // [M3b] LOCAL apikeys RICH-form submit WITH _csrf -> a NEW admin.action{form-submit}
    // event + the created role row. The rich configurator requires an explicit `_action`
    // (a blind resubmit of the rendered fields is a deliberate no-op — a half-filled row is
    // never silently applied), so this creates a role with a unique name. It is the
    // monolith-LOCAL parity for the split's [AD6b] (cross-process create) / [AD6g] (uniform
    // audit) — the same submit path, driven in-process instead of over the edge.
    let m3b_role = format!("m3b-role-{suffix}");
    sqlx::query("DELETE FROM apikeys.roles WHERE name = $1").bind(&m3b_role).execute(pool).await.ok();
    let before: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM asyncevents.events \
         WHERE topic='admin.action' \
           AND payload->>'actor'='proofadmin' \
           AND payload->>'target'='api-keys' \
           AND payload->>'action'='form-submit'",
    )
    .fetch_one(pool)
    .await?;
    let page = jar.get(format!("{m}/admin/api-keys")).send().await?.text().await.unwrap_or_default();
    let csrf = extract_form_fields(&page)
        .into_iter()
        .find(|(k, _)| k == "_csrf")
        .map(|(_, v)| v)
        .unwrap_or_default();
    if !csrf.is_empty() {
        let response = jar
            .post(format!("{m}/admin/api-keys"))
            .form(&[
                ("_csrf", csrf.as_str()),
                ("_action", "create_role"),
                ("role_name", m3b_role.as_str()),
                ("role_policy", "leaderboard.topScores"),
            ])
            .send()
            .await?;
        let post_status = response.status();
        let after: Option<i64> = if post_status.as_u16() == 303 {
            Some(
                sqlx::query_scalar(
                    "SELECT count(*) FROM asyncevents.events \
                     WHERE topic='admin.action' \
                       AND payload->>'actor'='proofadmin' \
                       AND payload->>'target'='api-keys' \
                       AND payload->>'action'='form-submit'",
                )
                .fetch_one(pool)
                .await?,
            )
        } else {
            None
        };
        let role_rows: Option<i64> =
            sqlx::query_scalar("SELECT count(*) FROM apikeys.roles WHERE name = $1")
                .bind(&m3b_role)
                .fetch_optional(pool)
                .await
                .ok()
                .flatten();
        let ok = post_status.as_u16() == 303
            && after.is_some_and(|after| after > before)
            && role_rows == Some(1);
        p.check(
            "[M3b] local rich-form create-role -> 303 + new admin.action event + row",
            ok,
            format!("post={post_status} before={before} after={after:?} role_rows={role_rows:?}"),
        );
    } else {
        p.check("[M3b] local form-submit form present (_csrf)", false, "no _csrf field on apikeys page");
    }

    // --- Wallet parity: the player reads and the durable starter grant, all Local. In the
    // monolith wallet's ops are dispatched in-process and the `player.registered` producer
    // and consumer share one process — the same code, the other topology.
    if let Some(tok) = &mtoken {
        let wl1 = ctx
            .http
            .get(format!("{m}/wallet/currencies"))
            .header("X-Api-Key", "dev-key-client")
            .header("Authorization", format!("Bearer {tok}"))
            .send()
            .await?;
        let (c, body) = (wl1.status().as_u16(), wl1.text().await.unwrap_or_default());
        p.check(
            "[WL1m] monolith GET /wallet/currencies -> 200 + gold/gems",
            c == 200 && body.contains("\"gold\"") && body.contains("\"gems\""),
            format!("code={c}"),
        );
        let wl2 = ctx
            .http
            .get(format!("{m}/wallet/me"))
            .header("X-Api-Key", "dev-key-client")
            .header("Authorization", format!("Bearer {tok}"))
            .send()
            .await?;
        let c = wl2.status().as_u16();
        let body: serde_json::Value = wl2.json().await.unwrap_or(serde_json::Value::Null);
        p.check(
            "[WL2m] monolith GET /wallet/me (Bearer) -> 200 + balance array",
            c == 200 && body.is_array(),
            format!("code={c}"),
        );
    }
    // [WL6m] the LOCAL half of [WL6]. The two topologies reach `render_error` by different
    // routes — LOCAL `Rejection::into_local` -> `SubmitError::Other(msg)`, REMOTE
    // `into_ops` -> `Error::invalid(msg)` whose `Display` is the bare message — so parity is
    // a property of two mappings, not of one. Same grant-then-over-revoke pair, same verdict
    // text, same unchanged balance, driven through the monolith's in-process submit closure.
    let wl6m_player: String = sqlx::query_scalar("SELECT gen_random_uuid()::text")
        .fetch_one(pool)
        .await?;
    let (csrf, idem_grant, _) = wallet_form(&jar, &m).await?;
    let wl6m_grant = jar
        .post(format!("{m}/admin/wallet"))
        .form(&[
            ("_csrf", csrf.as_str()),
            ("_idem_grant", idem_grant.as_str()),
            ("_action", "grant"),
            ("player_id", wl6m_player.as_str()),
            ("currency", "gold"),
            ("amount", "250"),
            ("reason", "splitproof monolith grant"),
        ])
        .send()
        .await?;
    let wl6m_grant_code = wl6m_grant.status().as_u16();
    let (csrf, _, idem_revoke) = wallet_form(&jar, &m).await?;
    let wl6m = jar
        .post(format!("{m}/admin/wallet"))
        .form(&[
            ("_csrf", csrf.as_str()),
            ("_idem_revoke", idem_revoke.as_str()),
            ("_action", "revoke"),
            ("player_id", wl6m_player.as_str()),
            ("currency", "gold"),
            ("amount", "999999"),
            ("reason", "splitproof monolith over-revoke"),
        ])
        .send()
        .await?;
    let wl6m_code = wl6m.status().as_u16();
    let wl6m_body = wl6m.text().await.unwrap_or_default();
    let wl6m_balance: Option<i64> = sqlx::query_scalar(
        "SELECT amount FROM wallet.balances WHERE player_id::text=$1 AND currency='gold'",
    )
    .bind(&wl6m_player)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    p.check(
        "[WL6m] monolith over-revoke -> verdict card 'insufficient funds', balance unchanged at 250",
        wl6m_grant_code == 303
            && wl6m_code == 200
            && wl6m_body
                .contains("save failed: movement rejected: insufficient funds or balance ceiling exceeded")
            && wl6m_balance == Some(250),
        format!("grant={wl6m_grant_code} revoke={wl6m_code} balance={wl6m_balance:?}"),
    );

    match register_capture(ctx, &m, &format!("wallet-mono-{suffix}@test.local")).await {
        Ok((pid, _)) => {
            let credited = poll_count(
                pool,
                "SELECT count(*) FROM wallet.balances \
                  WHERE player_id::text=$1 AND currency='gold' AND amount=100",
                &pid,
                1,
            )
            .await;
            p.check(
                "[WL7m] monolith registration receives the configured starter grant",
                credited,
                format!("pid={pid}"),
            );
        }
        Err(e) => p.check("[WL7m] monolith register for the starter grant", false, format!("{e:#}")),
    }

    // --- Notifications parity: the player read and the wallet fan-in, all Local. The split
    // proves the seams ([NT1]-[NT6]); these two prove the SAME code answers identically when
    // the producer, the consumer and the front door share one process — the fan-in still
    // travels the durable log, it just never leaves the process.
    match create_guest(ctx, &m).await {
        Ok((code, guest)) => {
            let (list_code, items, cursor) = inbox_page(ctx, &m, &guest.token, "", 0).await?;
            p.check(
                "[NT1m] monolith POST /notifications/list (fresh guest) -> 200 + empty page",
                code == 201 && list_code == 200 && items.is_empty() && cursor.is_empty(),
                format!("guest={code} code={list_code} items={}", items.len()),
            );
        }
        Err(e) => p.check("[NT1m] monolith create_guest for the inbox read", false, format!("{e:#}")),
    }

    let nt4m_player: String = sqlx::query_scalar("SELECT gen_random_uuid()::text")
        .fetch_one(pool)
        .await?;
    let (nt4m_csrf, nt4m_idem, _) = wallet_form(&jar, &m).await?;
    let nt4m = jar
        .post(format!("{m}/admin/wallet"))
        .form(&[
            ("_csrf", nt4m_csrf.as_str()),
            ("_idem_grant", nt4m_idem.as_str()),
            ("_action", "grant"),
            ("player_id", nt4m_player.as_str()),
            ("currency", "gold"),
            ("amount", "70"),
            ("reason", "splitproof monolith notification fan-in"),
        ])
        .send()
        .await?;
    let nt4m_code = nt4m.status().as_u16();
    let nt4m_row = poll_count(
        pool,
        "SELECT count(*) FROM notifications.messages \
          WHERE player_id::text = $1 AND kind = 'wallet.credit'",
        &nt4m_player,
        1,
    )
    .await;
    p.check(
        "[NT4m] monolith grant -> wallet.changed -> inbox row (same durable path, one process)",
        nt4m_code == 303 && nt4m_row,
        format!("grant={nt4m_code} inbox_row={nt4m_row} pid={nt4m_player}"),
    );

    // --- Mail parity: [ML1]-[ML4b] again with the producer, the consumer, the outbox and
    // the portal all in ONE process. The event still travels the durable log and the
    // operator faces still run the same `build_content`/`apply_submit`, in-process instead
    // of over the edge — the topology is the only difference.
    mail_assertions(ctx, pool, p, &m, &m, &jar, "m").await?;

    // --- Push parity: `[PH1m]`/`[PH3m]`. The monolith hosts no internal edge and needs
    // none — the producer and the sockets are the same process, so `ctx.push()` resolves
    // through `push_ws::LocalSink` and the backplane never runs. THAT is the point of the
    // pair and the limit of it: it proves the HUB (upgrade, handshake, ack, the nudge from
    // inside a durable delivery transaction, the addressed frame) behaves identically with
    // no transport in between. It proves NOTHING about fan-out; `[PH3]` above is the only
    // assertion that crosses a process, and no monolith run can substitute for it.
    push_assertions(ctx, pool, p, &m, &jar, None, "m").await?;

    federated_assertions(ctx, pool, &m, idp, p, "m").await?;

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

fn workspace_root() -> Result<PathBuf> {
    // Compile-time root derivation (same pattern as devctl's workspace_root):
    // splitproof lives at tools/splitproof, so the workspace root is two levels
    // above the crate's manifest dir. This never depends on where the built
    // binary sits, so an out-of-tree CARGO_TARGET_DIR cannot skew the root —
    // binary LOOKUP comes from WorkspaceLayout, never from exe position.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .context("no workspace root two levels above tools/splitproof")?
        .to_path_buf();
    if !root.join("Cargo.toml").is_file() {
        bail!(
            "derived workspace root {} has no Cargo.toml — expected the GameBackend \
             workspace two levels above the splitproof crate (tools/splitproof); was \
             the repo moved or deleted after the binary was built?",
            root.display()
        );
    }
    Ok(root)
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
    let root = match workspace_root() {
        Ok(root) => root,
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
            ["splitproof"],
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
    let result = runtime.block_on(run(root, run_dir));
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

async fn run(root: PathBuf, run_dir: PathBuf) -> Result<u32> {
    let environment = EnvironmentSnapshot::capture();
    // Locate built binaries via the SAME frozen build env the fleet is built with
    // (honors CARGO_TARGET_DIR); cwd stays `root` so build and lookup agree.
    let layout = WorkspaceLayout::from_root(root.clone(), &environment.build_environment());
    let db_url = environment.value("DATABASE_URL").map(str::to_owned).unwrap_or_else(|| processctl::DEFAULT_DATABASE_URL.to_string());
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
        layout,
        root: root.clone(),
        run_dir,
        db_url,
        fleet,
        environment,
    };

    // Fleet-drift tripwire: the centralized processctl fleet must equal
    // cmd/*-svc on disk.
    preflight_fleet(&root, &ctx).await?;

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

    // Capped, not sqlx's default 10: this pool is the itemized harness term in
    // `processctl`'s Postgres session budget, so the cap and the budget line are one const.
    let pool = PgPoolOptions::new()
        .max_connections(SPLITPROOF_ASSERTION_POOL_MAX)
        .connect(&ctx.db_url)
        .await
        .context("connect DB")?;
    reset_config_baseline(&pool).await?;
    reset_scoreboard_baseline(&pool).await?;
    // [WL7]'s starter-grant knobs, written BEFORE wallet-svc spawns: its `CachedConfig` is
    // boot-fill-or-fail-startup, so a post-boot write would race an invalidation refresh
    // against the registration under test. On a DB that has never booted the fleet the
    // `config` schema does not exist yet, so the write is retried once config-svc has
    // migrated it — still ahead of wallet-svc, which boots later in the canonical order.
    let mut wallet_knobs_seeded = seed_wallet_starter_config(&pool).await.is_ok();

    // The loopback identity provider, up BEFORE any accounts process: its JWKS url is
    // baked into the Proof fleet's `epic` configuration, and the first federated verify
    // fetches it. Dropped with `run`, so the split and the monolith share one key.
    let idp = Idp::start().await.context("start the loopback OIDC fixture")?;

    // Boot the fleet; each guard lives in `fleet` so a `?` below drops them all (kill).
    let mut fleet: Vec<Running> = Vec::new();
    for svc in ctx.fleet.services() {
        println!("[splitproof] starting {} on :{} ...", svc.name, svc.http_port);
        // config-svc must boot AFTER the baseline reset (done above) so its first
        // snapshot is the default; the centralized processctl fleet already
        // places it late.
        ensure_no_stale_listener(svc.name, svc.http_port)?;
        let mut running = ctx.spawn(svc)?;
        ctx.wait_healthy(svc, &mut running.child).await?;
        fleet.push(running);
        println!("[splitproof] {} healthy", svc.name);
        if !wallet_knobs_seeded && svc.name == "config-svc" {
            seed_wallet_starter_config(&pool)
                .await
                .context("seed the wallet starter-grant knobs")?;
            wallet_knobs_seeded = true;
        }
    }
    println!("[splitproof] fleet up: {}/{} processes healthy\n", fleet.len(), ctx.fleet.services().len());

    let mut p = Proof::default();
    // The whole proof runs inside `proof_phase` so its result can be CAPTURED: the wallet
    // starter-grant knobs are a config write this harness made, and they must be undone on
    // the failure path too — a run that dies mid-proof otherwise leaves the box granting
    // every later `devctl up monolith` registration 100 gold.
    let phase = proof_phase(&ctx, &pool, &idp, fleet, &mut p).await;
    clear_wallet_starter_config(&pool).await;

    println!(
        "\n[splitproof] {} passed, {} failed",
        p.pass,
        p.fail.len()
    );
    for f in &p.fail {
        println!("  - FAILED: {f}");
    }
    phase?;
    Ok(p.fail.len() as u32)
}

/// Every assertion phase, split + monolith parity, with the fleet's ownership moved in so it
/// is killed (no orphans) whichever way this returns.
async fn proof_phase(
    ctx: &Ctx,
    pool: &PgPool,
    idp: &Idp,
    mut fleet: Vec<Running>,
    p: &mut Proof,
) -> Result<()> {
    // [LV1] every child that cleared its readyz gate is still alive right after boot —
    // catches a stale listener on a service's port answering for a child that already
    // died (e.g. bind conflict surfacing only after the first successful accept).
    let lv1_dead = fleet_liveness(&mut fleet);
    p.check(
        "[LV1] fleet liveness after boot",
        lv1_dead.is_empty(),
        if lv1_dead.is_empty() { "all processes alive".to_string() } else { lv1_dead.join("; ") },
    );
    assertions(ctx, pool, idp, p).await?;

    // [I-GATE] live security proof: the harness boots the whole fleet with
    // INVENTORY_DEV_GRANT=1 (see the centralized Proof fleet above), so
    // `assertions` structurally cannot see the split bypass Step 1 closed. Restart
    // ONLY inventory-svc without the flag and prove a fully-authed grant call now
    // 404s through the front door.
    i_gate(ctx, &mut fleet, p).await?;

    // [RDY-DEAD] readiness-accuracy proof for the /readyz amplification fix: gateway-svc
    // holds a `remote::Stub` per fronted peer whose `/readyz` check reads a CACHED verdict
    // stamped by a BACKGROUND probe. Kill one peer (characters-svc), assert gateway
    // /readyz flips to 503 naming the dead stub from the probe alone, then respawn and
    // assert recovery — restoring the fleet before [LV2]/parity run.
    rdy_dead(ctx, &mut fleet, p).await?;

    // [REPLICAS] durable-plane replica belt: boot a SECOND leaderboard-svc against the same
    // Postgres (both processes hold `leaderboard.match-finished.v1`), drive a batch of N
    // match.finished events and DB-assert exactly-once (wins == N, never 2N) across two REAL
    // processes; THEN kill instance #1 and drive M more, asserting #2 alone climbs wins to N+M
    // (a deterministic failover witness that #2 is a genuine delivering participant). The
    // deterministic lock/contention proof lives in worker_tests.rs:317; this is the e2e belt.
    // The second instance is scenario-local (never in the fleet); #1 is restored before [LV2].
    replicas_exactly_once(ctx, pool, &mut fleet, p).await?;

    // [LV2] fleet-wide liveness sweep immediately before the split fleet is torn down —
    // a service that died AFTER [LV1]'s post-boot check (e.g. mid-assertions) must not
    // silently drop out of the assertions that ran against it.
    let lv2_dead = fleet_liveness(&mut fleet);
    p.check(
        "[LV2] fleet liveness before teardown",
        lv2_dead.is_empty(),
        if lv2_dead.is_empty() { "all processes alive".to_string() } else { lv2_dead.join("; ") },
    );

    // --- Monolith parity: tear the split down (frees :8080 + :9100), boot cmd/server on
    // the same player front, and re-prove a subset (never-monolith-only-features). ---
    drop(fleet);
    tokio::time::sleep(Duration::from_millis(800)).await;
    if let Err(e) = monolith_parity(ctx, pool, idp, p).await {
        p.check("[M0-M3b] monolith parity phase", false, format!("fatal: {e:#}"));
    }
    Ok(())
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

async fn preflight_fleet(root: &Path, ctx: &Ctx) -> Result<()> {
    ctx.fleet.validate_disk(&root.join("cmd"))?;
    println!(
        "[splitproof] fleet preflight OK: {} svcs == cmd/*-svc on disk",
        ctx.fleet.services().len()
    );
    // The session-budget half of the same preflight: the fleet's reservation is only
    // valid on a cluster that offers that many sessions, so ask before spawning. The
    // query is local (this harness is already async, so it never enters processctl's
    // blocking twin); the SQL, the verdict and its remedy come from processctl, the
    // budget's authority. HARNESS_RESERVE is charged because this harness IS the
    // tooling that reserve itemizes — its assertion pool and its `[REPLICAS]` second
    // leaderboard-svc run alongside the fleet.
    let mut connection = sqlx::PgConnection::connect(&ctx.db_url)
        .await
        .context("connect to DATABASE_URL for the session-capacity preflight")?;
    let (max_connections, superuser_reserved, reserved): (i32, i32, i32) =
        sqlx::query_as(processctl::PG_SESSION_CAPACITY_SQL)
            .fetch_one(&mut connection)
            .await
            .context("read the Postgres session settings")?;
    connection.close().await.ok();
    let capacity = PgSessionCapacity {
        max_connections: max_connections.max(0) as u32,
        reserved: (superuser_reserved.max(0) + reserved.max(0)) as u32,
    };
    let required = ctx.fleet.pg_session_reservation() + HARNESS_RESERVE;
    processctl::check_pg_session_floor(capacity, required)?;
    println!(
        "[splitproof] Postgres preflight OK: {} usable sessions >= {required} reserved",
        capacity.usable()
    );
    Ok(())
}

/// Seed an admin login via adminctl (password in its supported private environment
/// input, never argv). adminctl ensures its schema before admin-svc migrates.
fn seed_admin(ctx: &Ctx, user: &str, pass: &str) -> Result<()> {
    let bin = ctx.layout.binary("debug", "adminctl");
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

/// Every prefix this harness may name a `/match/report` contestant with. ONE authority for
/// two directions: `harness_player` refuses to mint a name outside it, and
/// `reset_scoreboard_baseline` sweeps exactly it — so the delete predicate cannot drift
/// behind a new scenario's names the way it already had ([K4]'s `k4-w`, unswept since the
/// day it was written).
const HARNESS_PLAYER_PREFIXES: [&str; 5] = ["champ-", "chump-", "replicas-", "k3-", "k4-"];

/// A contestant name for a `/match/report`. Panics on anything the reset cannot sweep:
/// `leaderboard.scores`/`rating.ratings` retain a row forever, so an unsweepable name is a
/// permanent addition to a table two assertions read absolute values out of.
#[track_caller]
fn harness_player(name: &str) -> String {
    assert_harness_player(name);
    name.to_string()
}

#[track_caller]
fn assert_harness_player(name: &str) {
    assert!(
        HARNESS_PLAYER_PREFIXES.iter().any(|prefix| name.starts_with(prefix)),
        "match-report contestant {name:?} is outside HARNESS_PLAYER_PREFIXES: its \
         `leaderboard.scores`/`rating.ratings` rows are retained forever and \
         `reset_scoreboard_baseline` would never sweep them",
    );
}

/// The `leaderboard.scores` / `rating.ratings` rows this harness's own match reports leave
/// behind. Prefix-scoped over the WHOLE history, not this run's names: both projections are
/// permanently retained and the harness's player names are keyed by pid, which the OS
/// reuses — so a prior run's `replicas-{pid}` row makes `[REPLICAS-3]`'s `wins == N` start
/// from a non-zero tally, and a prior `champ-{pid}` makes `[MT2]`/`[MT5]` unreachable. Run
/// START, not end of scenario: every scenario returns `?`, so a trailing delete skips
/// exactly the failing run whose rows then break the next one.
///
/// Only `42P01` is tolerated (the schema a first-ever run has not migrated yet). Every other
/// failure ABORTS: this reset is the precondition [MT5] and [REPLICAS-3] read absolute
/// values against, and a swallowed delete would let a prior run's 1030/970 satisfy [MT5]
/// before this run has delivered anything.
async fn reset_scoreboard_baseline(pool: &PgPool) -> Result<()> {
    let predicate = HARNESS_PLAYER_PREFIXES
        .iter()
        .map(|prefix| format!("player LIKE '{prefix}%'"))
        .collect::<Vec<_>>()
        .join(" OR ");
    for table in ["leaderboard.scores", "rating.ratings"] {
        match sqlx::query(&format!("DELETE FROM {table} WHERE {predicate}"))
            .execute(pool)
            .await
        {
            Ok(_) => {}
            Err(e) if is_undefined_table(&e) => {}
            Err(e) => {
                return Err(e).with_context(|| format!("clear the harness-owned {table} rows"))
            }
        }
    }
    Ok(())
}

fn is_undefined_table(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db) if db.code().as_deref() == Some("42P01"))
}

/// The two `wallet` starter-grant knobs [WL7] depends on. Data, not env: the feature is
/// off by compiled default and enabling it is a config write.
async fn seed_wallet_starter_config(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO config.settings (namespace, key, value) VALUES \
             ('wallet','starter_currency','gold'), ('wallet','starter_amount','100') \
         ON CONFLICT (namespace, key) DO UPDATE SET value = EXCLUDED.value",
    )
    .execute(pool)
    .await
    .map(|_| ())
}

/// One render of the `/admin/wallet` form as `(csrf, idem_grant, idem_revoke)`.
/// `extract_form_fields` parses `<input>` only, so it round-trips the render-time
/// `_idem_grant`/`_idem_revoke` hidden keys and the session `_csrf` — but NOT the
/// `_action`/`currency` `<select>`s, which are supplied by hand at POST time exactly as
/// [AD6b] does. Shared by the split's [WL3]-[WL6] (a REMOTE form fetched over the edge) and
/// the monolith's [WL6m] (the in-process render), which is what makes them the same proof of
/// two different submit paths.
async fn wallet_form(client: &reqwest::Client, base: &str) -> Result<(String, String, String)> {
    let page = client
        .get(format!("{base}/admin/wallet"))
        .send()
        .await?
        .text()
        .await
        .unwrap_or_default();
    let fields = extract_form_fields(&page);
    let pick = |name: &str| {
        fields
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    Ok((pick("_csrf"), pick("_idem_grant"), pick("_idem_revoke")))
}

/// One render of the `/admin/inbox` operator-mail form as `(csrf, idem_send)`. The
/// `_idem_send` key is minted per RENDER (`admin::mint_idempotency_key`), so a second
/// message needs a SECOND render — replaying one key is the dedup arm, not a delivery.
/// The URL carries the page's slug, not its item id; see [NT2] for why that is a proof
/// obligation rather than a spelling detail.
async fn inbox_form(client: &reqwest::Client, base: &str) -> Result<(String, String)> {
    let page = client
        .get(format!("{base}/admin/inbox"))
        .send()
        .await?
        .text()
        .await
        .unwrap_or_default();
    let fields = extract_form_fields(&page);
    let pick = |name: &str| {
        fields
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    Ok((pick("_csrf"), pick("_idem_send")))
}

/// `POST /notifications/list` as one player through a front door, answering
/// `(status, items, next_cursor)`. A non-200 yields an empty page so a caller asserts the
/// status instead of unwrapping. Retries past the gateway's always-on 429 exactly as
/// `register_capture` does.
async fn inbox_page(
    ctx: &Ctx,
    base: &str,
    token: &str,
    cursor: &str,
    limit: i64,
) -> Result<(u16, Vec<serde_json::Value>, String)> {
    for _ in 0..15 {
        let r = ctx
            .http
            .post(format!("{base}/notifications/list"))
            .header("X-Api-Key", "dev-key-client")
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({"cursor": cursor, "limit": limit}))
            .send()
            .await?;
        let code = r.status().as_u16();
        if code == 429 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }
        let body: serde_json::Value = r.json().await.unwrap_or(serde_json::Value::Null);
        let items = body
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let next = body
            .get("next_cursor")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        return Ok((code, items, next));
    }
    bail!("notifications.list rate-limited out")
}

/// The status of one bodiless, player-authenticated inbox call (`mark_read`, `delete`).
/// Both ops answer 204 and carry no body, so the code IS the contract — including the
/// second `delete`'s 404.
async fn inbox_status(
    ctx: &Ctx,
    base: &str,
    token: &str,
    method: reqwest::Method,
    path: &str,
) -> Result<u16> {
    for _ in 0..15 {
        let r = ctx
            .http
            .request(method.clone(), format!("{base}{path}"))
            .header("X-Api-Key", "dev-key-client")
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await?;
        let code = r.status().as_u16();
        if code == 429 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }
        return Ok(code);
    }
    bail!("{path} rate-limited out")
}

/// Seeds `n` inbox rows for one player straight into `notifications.messages` and answers
/// their ids, newest first — the order [`inbox_page`] must reproduce. Every third row
/// shares its `created_at` with the two before it, so the walk that reads them back is
/// forced through the keyset's `id DESC` tie-break rather than a `created_at`-only compare.
/// The seed is a FIXTURE for the paging read path; the write paths are proven by [NT2] and
/// [NT4]/[NT6].
async fn seed_inbox_rows(pool: &PgPool, player_id: &str, n: i64) -> Result<Vec<String>> {
    sqlx::query(
        "INSERT INTO notifications.messages (id, player_id, kind, title, body, created_at) \
         SELECT gen_random_uuid(), $1::uuid, 'splitproof.page', 'Page row ' || i, 'seeded', \
                now() - (((i / 3)::int) || ' seconds')::interval \
           FROM generate_series(1, $2::int) AS i",
    )
    .bind(player_id)
    .bind(n)
    .execute(pool)
    .await?;
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT id::text FROM notifications.messages WHERE player_id = $1::uuid \
          ORDER BY created_at DESC, id DESC",
    )
    .bind(player_id)
    .fetch_all(pool)
    .await?;
    Ok(ids)
}

/// Appends one `mail.send_requested` straight onto the durable plane through
/// `asyncevents.append_event` — the plane's single writer, the same SQL entry point
/// `config`'s row trigger uses. That makes the harness a REAL producer: no `mail` code runs
/// in this process, and in the split the consumer is a different OS process, so nothing
/// here can pass by sharing an address space with the module under test. The topic and
/// version are literals for the same reason every other topic in this file is (the harness
/// imports no `api/` crate); a contract-version bump therefore surfaces as [ML1] never
/// seeing a row, not as a silently skipped assertion.
async fn append_send_requested(
    pool: &PgPool,
    key: &str,
    to: &str,
    subject: &str,
    kind: &str,
) -> Result<String> {
    let payload = serde_json::json!({
        "idempotency_key": key,
        "to": to,
        "subject": subject,
        "body": "queued by splitproof",
        "kind": kind,
    });
    let (event_id,): (String,) =
        sqlx::query_as("SELECT asyncevents.append_event($1, $2, $3::jsonb)")
            .bind("mail.send_requested")
            .bind(1i32)
            .bind(payload.to_string())
            .fetch_one(pool)
            .await?;
    Ok(event_id)
}

/// One unlabelled prometheus counter as scraped from the process that OWNS it. `None` means
/// the scrape itself failed; `Some(0.0)` means the counter is absent, which is the honest
/// zero for `mail`'s conflict counter — it is registered on FIRST use, so before the first
/// conflict it exists nowhere.
async fn counter_value(ctx: &Ctx, base: &str, name: &str) -> Option<f64> {
    let body = ctx
        .http
        .get(format!("{base}/metrics"))
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix(name) {
            if let Some(value) = rest.strip_prefix(' ') {
                return value.trim().parse().ok();
            }
        }
    }
    Some(0.0)
}

/// A per-pass random nonce, NOT the pid: pids recycle, and an outbox row left behind by an
/// aborted run under a recycled pid could otherwise satisfy [ML1] with no event consumed at
/// all.
fn mail_nonce(tag: &str) -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let mut hex = String::with_capacity(16);
    for b in bytes {
        hex.push_str(&format!("{b:02x}"));
    }
    format!("{hex}{tag}")
}

/// The `_csrf` + render-minted `_idem_test` pair from one render of the Mail page. The test
/// key is minted PER RENDER (`admin::mint_test_key`), so a second test send needs a second
/// render — replaying one key is the dedup arm, not a delivery.
async fn mail_form(client: &reqwest::Client, base: &str) -> Result<(String, String)> {
    let page = client
        .get(format!("{base}/admin/mail"))
        .send()
        .await?
        .text()
        .await
        .unwrap_or_default();
    let fields = extract_form_fields(&page);
    let pick = |name: &str| {
        fields
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    Ok((pick("_csrf"), pick("_idem_test")))
}

/// `[ML1]`-`[ML4b]`, run once per topology.
///
/// `front` is the process serving `/admin` (gateway-svc's passthrough in the split, the
/// monolith itself in parity), `metrics` is the process that OWNS the outbox counters
/// (mail-svc / the monolith), `jar` is an already-authenticated operator session, and `tag`
/// suffixes every name and every idempotency key so the two topologies neither collide in
/// the outbox nor report indistinguishable verdicts.
async fn mail_assertions(
    ctx: &Ctx,
    pool: &PgPool,
    p: &mut Proof,
    front: &str,
    metrics: &str,
    jar: &reqwest::Client,
    tag: &str,
) -> Result<()> {
    let nonce = mail_nonce(tag);
    // Every `splitproof-` key and every `admin-send-test-` key is this harness's by
    // construction, so clearing ALL of them — not just this pass's — is what makes the
    // outbox's own state a fixture rather than an input: [ML4b]'s whole-table bulk verb
    // cannot inherit a parked row from a run that failed before requeuing it, and the
    // operator test sends do not accumulate across runs. The error PROPAGATES: a swallowed
    // clear is the one way a stale row could be mistaken for this pass's proof.
    sqlx::query(
        "DELETE FROM mail.outbox \
          WHERE idempotency_key LIKE 'splitproof-%' \
             OR idempotency_key LIKE 'admin-send-test-%'",
    )
    .execute(pool)
    .await
    .context("clear the harness-owned mail.outbox rows")?;

    // [ML1] THE cross-process proof of the durable ingress. The event is appended by THIS
    // process through the plane's own writer; mail-svc — which the harness never calls —
    // pulls it on its own subscription cursor, enqueues the outbox row in the delivery
    // transaction, and its drain sends it. Both halves are asserted separately so an
    // ingress failure and a drain failure are distinguishable, and the stored recipient /
    // subject / kind are compared against what was APPENDED so the row cannot be some
    // other message. `body` is deliberately not compared: a delivered row's body is
    // blanked by design.
    let ml1_key = format!("splitproof-ml1-{nonce}");
    let ml1_to = format!("ml1-{nonce}@example.com");
    let ml1_subject = format!("Split-proof durable ingress {nonce}");
    let ml1_kind = "splitproof.delivery";
    let ml1_event =
        append_send_requested(pool, &ml1_key, &ml1_to, &ml1_subject, ml1_kind).await?;
    let ml1_enqueued = poll_count(
        pool,
        "SELECT count(*) FROM mail.outbox WHERE idempotency_key = $1",
        &ml1_key,
        1,
    )
    .await;
    let ml1_sent = ml1_enqueued
        && poll_count(
            pool,
            &format!(
                "SELECT count(*) FROM mail.outbox WHERE idempotency_key = $1 \
                   AND state = 'sent' AND provider = '{}' AND sent_at IS NOT NULL",
                processctl::PROOF_MAIL_PROVIDER
            ),
            &ml1_key,
            1,
        )
        .await;
    let ml1_row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT recipient, subject, kind FROM mail.outbox WHERE idempotency_key = $1",
    )
    .bind(&ml1_key)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let ml1_payload = ml1_row
        .as_ref()
        .is_some_and(|(to, subject, kind)| {
            to == &ml1_to && subject == &ml1_subject && kind.as_str() == ml1_kind
        });
    p.check(
        &format!(
            "[ML1{tag}] harness-appended mail.send_requested -> outbox row -> state=sent, \
             provider={}",
            processctl::PROOF_MAIL_PROVIDER
        ),
        ml1_enqueued && ml1_sent && ml1_payload,
        format!(
            "event={ml1_event} enqueued={ml1_enqueued} sent={ml1_sent} payload={ml1_payload} \
             row={ml1_row:?}"
        ),
    );

    // [ML2] the enqueue authority's idempotency, driven from the durable side. Three events
    // under ONE key: two identical, then one with a different subject. The third is a
    // producer bug the module must refuse WITHOUT dropping the stored message, and its
    // counter is also the happens-after signal for the first two — delivery is ordered per
    // subscription, so a conflict counted means both earlier events were already applied.
    // Without that ordering this assertion would need a sleep and would pass on an
    // enqueue that lands late.
    let ml2_key = format!("splitproof-ml2-{nonce}");
    let ml2_to = format!("ml2-{nonce}@example.com");
    let ml2_subject = format!("Split-proof idempotent {nonce}");
    let ml2_before = counter_value(ctx, metrics, "mail_enqueue_conflicts_total").await;
    for _ in 0..2 {
        append_send_requested(pool, &ml2_key, &ml2_to, &ml2_subject, "splitproof.dedup").await?;
    }
    append_send_requested(
        pool,
        &ml2_key,
        &ml2_to,
        &format!("{ml2_subject} EDITED"),
        "splitproof.dedup",
    )
    .await?;
    // A FOURTH event under a fresh key fences the three above: delivery is ordered per
    // subscription, so its outbox row existing means all three were already applied and the
    // conflict counter is FINAL. That is what licenses the exact `floor + 1` — and the
    // exact count is the assertion, because the realistic dedup regression is the inverse
    // of a silent insert: an identical replay misclassified as a `Conflict` drops the
    // message while blaming the producer, and it leaves `rows`, the kept subject and a
    // merely-`moved` counter all looking correct.
    let ml2_fence_key = format!("splitproof-ml2fence-{nonce}");
    append_send_requested(
        pool,
        &ml2_fence_key,
        &format!("ml2fence-{nonce}@example.com"),
        &format!("Split-proof dedup fence {nonce}"),
        "splitproof.dedup",
    )
    .await?;
    let ml2_fenced = poll_count(
        pool,
        "SELECT count(*) FROM mail.outbox WHERE idempotency_key = $1",
        &ml2_fence_key,
        1,
    )
    .await;
    let ml2_after = if ml2_fenced {
        counter_value(ctx, metrics, "mail_enqueue_conflicts_total").await
    } else {
        None
    };
    let ml2_moved =
        matches!((ml2_before, ml2_after), (Some(b), Some(a)) if (a - b - 1.0).abs() < 0.5);
    let ml2_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mail.outbox WHERE idempotency_key = $1")
            .bind(&ml2_key)
            .fetch_one(pool)
            .await
            .unwrap_or(-1);
    let ml2_subject_kept: Option<String> =
        sqlx::query_scalar("SELECT subject FROM mail.outbox WHERE idempotency_key = $1")
            .bind(&ml2_key)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    p.check(
        &format!(
            "[ML2{tag}] one key, three events -> exactly one outbox row; the edited replay \
             is counted exactly once, the identical ones not at all"
        ),
        ml2_rows == 1 && ml2_subject_kept.as_deref() == Some(ml2_subject.as_str()) && ml2_moved,
        format!(
            "rows={ml2_rows} subject_kept={} fenced={ml2_fenced} \
             conflicts={ml2_before:?}->{ml2_after:?}",
            ml2_subject_kept.as_deref() == Some(ml2_subject.as_str())
        ),
    );

    // [ML3] the operator READ face. In the split this render is `admin.adminData` over the
    // mTLS edge: the portal in admin-svc holds no outbox and no mail code, so every value
    // on the page crossed the wire from mail-svc. The URL is `/admin/mail` — the portal
    // routes on `adminapi::slug(ADMIN_LABEL)`, so a drifted label 404s here and nothing
    // else in the tree would say so — and [ML1]'s recipient is the payload assertion: a
    // page that rendered but showed nothing from the outbox would otherwise pass.
    let ml3 = jar.get(format!("{front}/admin/mail")).send().await?;
    let (ml3_code, ml3_body) = (ml3.status().as_u16(), ml3.text().await.unwrap_or_default());
    let ml3_shows_row = ml3_body.contains(&ml1_to);
    let ml3_is_outbox = ml3_body.contains("Outbox") && ml3_body.contains("PENDING");
    p.check(
        &format!("[ML3{tag}] GET /admin/mail -> 200 outbox page carrying [ML1{tag}]'s row"),
        ml3_code == 200 && ml3_shows_row && ml3_is_outbox,
        format!("code={ml3_code} shows_row={ml3_shows_row} outbox_page={ml3_is_outbox}"),
    );

    // [ML4] the operator WRITE face — in the split, `admin.adminSubmit` over the edge into
    // mail-svc, which owns the only insert. The submit is synchronous behind its 303, so
    // the row is a DIRECT read: polling here would let a write that lands late still pass.
    let ml4_to = format!("ml4-{nonce}@example.com");
    let (ml4_csrf, ml4_idem) = mail_form(jar, front).await?;
    let ml4 = jar
        .post(format!("{front}/admin/mail"))
        .form(&[
            ("_csrf", ml4_csrf.as_str()),
            ("_idem_test", ml4_idem.as_str()),
            ("_action", "send-test"),
            ("test_to", ml4_to.as_str()),
        ])
        .send()
        .await?;
    let ml4_code = ml4.status().as_u16();
    let ml4_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mail.outbox WHERE recipient = $1 AND kind = 'admin.test'",
    )
    .bind(&ml4_to)
    .fetch_one(pool)
    .await
    .unwrap_or(-1);
    p.check(
        &format!("[ML4{tag}] remote send-test submit -> 303 + mail.outbox row (kind=admin.test)"),
        ml4_code == 303 && ml4_rows == 1,
        format!("submit={ml4_code} rows={ml4_rows} to={ml4_to}"),
    );

    // [ML4b] the same remote write driving `requeue-all-parked`, the ONE verb whose result
    // is a `SubmitOutcome::notice` — a channel that exists nowhere else and whose whole
    // path is at risk in the split: the count is computed in mail-svc, crosses the edge in
    // the submit response, is stashed one-shot by admin-svc and is rendered by the
    // FOLLOW-UP GET of the post-redirect-get. The parked row is SEEDED, and that is a
    // stated limit: the drain's whole failure half — backoff, `attempts` growth, the
    // generation CAS, `max_attempts` -> parked, `last_error` truncation — is unproven
    // pending the module's unit tests, which can drive a failing `Sender`. What IS proven
    // here is the operator's recovery verb: the row's own state shows the requeue re-entered
    // delivery instead of only reporting that it had.
    let ml4b_key = format!("splitproof-ml4park-{nonce}");
    sqlx::query(
        "INSERT INTO mail.outbox (idempotency_key, recipient, subject, body, kind, state, \
                                  attempts, last_error) \
         VALUES ($1, $2, $3, 'parked by splitproof', 'splitproof.parked', 'parked', 5, \
                 'seeded parked by splitproof')",
    )
    .bind(&ml4b_key)
    .bind(format!("ml4b-{nonce}@example.com"))
    .bind(format!("Split-proof parked {nonce}"))
    .execute(pool)
    .await?;
    // The bulk verb is whole-table, so the count it must report is READ, never assumed:
    // asserting a literal would make this assertion a hostage to anything else that parked
    // a row. The seed above is what keeps it above zero.
    let ml4b_parked: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mail.outbox WHERE state = 'parked'")
            .fetch_one(pool)
            .await
            .unwrap_or(-1);
    let (ml4b_csrf, _) = mail_form(jar, front).await?;
    let ml4b = jar
        .post(format!("{front}/admin/mail"))
        .form(&[
            ("_csrf", ml4b_csrf.as_str()),
            ("_action", "requeue-all-parked"),
        ])
        .send()
        .await?;
    let ml4b_code = ml4b.status().as_u16();
    let ml4b_location = ml4b
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let ml4b_flash = ml4b_location.contains("/admin/mail?reveal=");
    let ml4b_notice = if ml4b_flash {
        jar.get(format!("{front}{ml4b_location}"))
            .send()
            .await?
            .text()
            .await
            .unwrap_or_default()
    } else {
        String::new()
    };
    // The remaining-count clause is asserted, not just the moved count: it is the half that
    // tells an operator to submit again after `SKIP LOCKED` left rows behind, and a
    // `contains` of the moved count alone is satisfied by either branch of `bulk_report`.
    let ml4b_expected = format!("Requeued {ml4b_parked} parked message(s); none left parked.");
    let ml4b_reported = ml4b_parked >= 1 && ml4b_notice.contains(&ml4b_expected);
    let ml4b_state: Option<String> =
        sqlx::query_scalar("SELECT state FROM mail.outbox WHERE idempotency_key = $1")
            .bind(&ml4b_key)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    let ml4b_unparked = ml4b_state.as_deref().is_some_and(|s| s != "parked");
    p.check(
        &format!(
            "[ML4b{tag}] remote requeue-all-parked -> 303 ?reveal -> notice survives the \
             edge hop and the PRG; the seeded row leaves 'parked'"
        ),
        ml4b_code == 303 && ml4b_flash && ml4b_reported && ml4b_unparked,
        format!(
            "submit={ml4b_code} flash={ml4b_flash} parked_before={ml4b_parked} \
             reported={ml4b_reported} state={ml4b_state:?}"
        ),
    );

    Ok(())
}

/// `[ML6]` — the retention sweep across processes: scheduler-svc appends
/// `scheduler.fired{mail-prune}`, mail-svc pulls it on its OWN subscription and deletes
/// inside the delivery transaction. Same shape as [SP2]'s session prune, driven with the
/// machinery already here (force `last_fired` to the epoch, poll the row out) — no new
/// fixture.
///
/// Split-only, deliberately rather than by omission: the monolith's Proof environment sets
/// no `SCHEDULER_ENABLED` (the fleet sets it on scheduler-svc alone), so there is no tick
/// to drive in the parity pass.
///
/// BOTH halves are asserted. A fresh `sent` row seeded in the same statement must SURVIVE:
/// a sweep with an inverted date predicate, or one that ignored the fired name and ran on
/// every schedule, deletes the stale row exactly as a working one does and would pass on
/// the first half alone.
async fn mail_prune_assertion(pool: &PgPool, p: &mut Proof) -> Result<()> {
    let nonce = mail_nonce("prune");
    let stale_key = format!("splitproof-ml6stale-{nonce}");
    let fresh_key = format!("splitproof-ml6fresh-{nonce}");
    sqlx::query(
        "INSERT INTO mail.outbox (idempotency_key, recipient, subject, body, kind, state, \
                                  provider, sent_at, created_at) \
         VALUES ($1, 'ml6@example.com', 'Split-proof stale', '', 'splitproof.retention', \
                 'sent', 'log', now() - interval '400 days', now() - interval '400 days'), \
                ($2, 'ml6@example.com', 'Split-proof fresh', '', 'splitproof.retention', \
                 'sent', 'log', now(), now())",
    )
    .bind(&stale_key)
    .bind(&fresh_key)
    .execute(pool)
    .await
    .context("seed the mail retention fixtures")?;
    sqlx::query(
        "UPDATE scheduler.schedules SET last_fired = to_timestamp(0) WHERE name = 'mail-prune'",
    )
    .execute(pool)
    .await
    .context("force the mail-prune schedule due")?;
    let swept = poll_count(
        pool,
        "SELECT count(*) FROM mail.outbox WHERE idempotency_key = $1",
        &stale_key,
        0,
    )
    .await;
    let kept: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mail.outbox WHERE idempotency_key = $1")
            .bind(&fresh_key)
            .fetch_one(pool)
            .await
            .unwrap_or(-1);
    p.check(
        "[ML6] scheduler-svc mail-prune -> mail-svc sweeps the stale sent row, keeps the fresh one",
        swept && kept == 1,
        format!("swept={swept} kept={kept}"),
    );
    Ok(())
}

/// Undoes [`seed_wallet_starter_config`], restoring wallet's COMPILED default (the grant is
/// off unless configured) — the `reset_config_baseline` precedent: a proof run must leave the
/// shared local Postgres as it found it, or every later `devctl up` registration silently
/// receives 100 gold and the next "the grant is off by default" check reads as broken.
/// Best-effort and never fatal: this runs on the failure path too, where a missing `config`
/// schema is a possible reason the run failed in the first place.
async fn clear_wallet_starter_config(pool: &PgPool) {
    if let Err(e) = sqlx::query(
        "DELETE FROM config.settings WHERE namespace='wallet' \
           AND key IN ('starter_currency','starter_amount')",
    )
    .execute(pool)
    .await
    {
        println!("[splitproof] WARN: could not clear the wallet starter knobs: {e}");
    }
}

/// The `[PH*]` push-hub proof — the one part of this feature that crosses a process
/// boundary, and therefore the only place its wiring is executed at all.
///
/// Everything else about the hub is pinned by unit tests inside one process: the model
/// and the codec (`core/push`), the backplane queue and its fan-out (`core/remote`), the
/// handshake, the caps, the groups and the teardown (`modules/gateway`). What NO unit
/// test can reach is the composition: `notifications-svc` holding a `remote::PushSender`
/// aimed at `GATEWAY_EDGE_ADDR`, `gateway-svc` serving an internal edge at all, and the
/// batch encoded in one process being decoded in another. `[PH3]` is that proof; a
/// mistake in any one of those three is invisible everywhere else and shows up here as a
/// frame that never arrives.
///
/// `edge` is the front's internal-edge address when there IS one (the split). On the
/// monolith it is `None`: the producer and the sockets share a process, `LocalSink`
/// answers directly, and no backplane exists to prove. The parity pair therefore proves
/// the HUB is topology-independent, NOT that the fan-out works — which is why it is a
/// pair and not a re-run of the whole set.
async fn push_assertions(
    ctx: &Ctx,
    pool: &PgPool,
    p: &mut Proof,
    base: &str,
    admin: &reqwest::Client,
    edge: Option<std::net::SocketAddr>,
    tag: &str,
) -> Result<()> {
    let split = edge.is_some();
    // The upgrade is a plain-HTTP request on the same port; only the scheme differs.
    let ws = format!("ws://{}", base.trim_start_matches("http://"));
    let suffix = format!("{}{tag}", std::process::id());
    let ca_cert = ctx.ca_cert.to_str().context("CA cert path not UTF-8")?.to_string();
    let ca_key = ctx.ca_key.to_str().context("CA key path not UTF-8")?.to_string();
    let deliver = |batch: Vec<push::Envelope>| {
        let (cert, key) = (ca_cert.clone(), ca_key.clone());
        async move {
            let addr = edge.context("the backplane injector needs an internal edge")?;
            pushws::deliver_batch(addr, &cert, &key, &batch).await
        }
    };

    // Every push assertion is driven by a GUEST, for the reason `[NT1]` gives: a guest
    // receives no starter grant, so its inbox is empty BY CONSTRUCTION and the only
    // `notifications.new` frame it can ever see is the one this proof causes. A
    // registered player would be racing its own starter-grant nudge, and `[PH3]` would
    // pass for the wrong reason.
    let (ph1_code, ph1_guest) = create_guest(ctx, base).await?;
    let mut ph1 = PushClient::connect(&ws, &ph1_guest.token, "dev-key-client").await;
    let ph1_ack = match ph1.as_mut() {
        Ok(client) => client.next_frame(Duration::from_secs(10)).await.ok().flatten(),
        Err(_) => None,
    };
    p.check(
        &format!(
            "[PH1{tag}] GET /push with bearer + api key -> upgrade + ack carrying a connection id"
        ),
        ph1_code == 201
            && matches!(&ph1_ack, Some(Frame::Ack { connection_id }) if *connection_id > 0),
        format!(
            "guest={ph1_code} upgrade={} first_frame={ph1_ack:?}",
            ph1.as_ref().map(|_| "ok").unwrap_or("failed")
        ),
    );

    if split {
        // [PH2] the credential refusal, which is NOT an HTTP status: a browser never sees
        // one on a WebSocket dial, so the front upgrades first and then closes with a
        // typed frame. The ack is the ONLY evidence a connection was registered, so a
        // close arriving as the FIRST frame is what "nothing registered" looks like from
        // the outside; `retryable: false` is the half a client acts on — a bad token that
        // reported itself retryable would make every rejected client reconnect forever.
        let mut ph2 = PushClient::connect(&ws, &format!("bogus-{suffix}"), "dev-key-client").await;
        let ph2_first = match ph2.as_mut() {
            Ok(client) => client.next_frame(Duration::from_secs(10)).await.ok().flatten(),
            Err(_) => None,
        };
        let ph2_ended = match ph2.as_mut() {
            Ok(client) => matches!(client.next_frame(Duration::from_secs(10)).await, Ok(None)),
            Err(_) => false,
        };
        p.check(
            &format!("[PH2{tag}] a bad bearer -> typed close (unauthorized, not retryable), no ack"),
            matches!(
                &ph2_first,
                Some(Frame::Close { code, retryable }) if code == "unauthorized" && !*retryable
            ) && ph2_ended,
            format!("first_frame={ph2_first:?} socket_ended={ph2_ended}"),
        );
    }

    // [PH3] THE cross-process proof. An operator grant is applied by wallet-svc, which
    // appends `wallet.changed` to the shared log in its own transaction; notifications-svc
    // — a third process — pulls it, writes the inbox row in the delivery transaction and
    // nudges `ctx.push()`, which in this process is the BACKPLANE sender; the batch
    // crosses the internal edge to gateway-svc, which owns the socket. Nothing here is
    // reachable from a unit test: a wrong `GATEWAY_EDGE_ADDR`, a front that serves no
    // inbound edge, or a batch codec that disagrees across the wire all look identical
    // from inside one process and all fail this assertion.
    //
    // The client connects BEFORE the grant on purpose: push is best-effort with no
    // redelivery, so a frame produced while nobody is listening is gone.
    let (ph3_code, ph3_guest) = create_guest(ctx, base).await?;
    let mut ph3 = PushClient::connect(&ws, &ph3_guest.token, "dev-key-client").await;
    let ph3_ack = match ph3.as_mut() {
        Ok(client) => matches!(
            client.next_frame(Duration::from_secs(10)).await,
            Ok(Some(Frame::Ack { .. }))
        ),
        Err(_) => false,
    };
    let (ph3_csrf, ph3_idem, _) = wallet_form(admin, base).await?;
    let ph3_grant = admin
        .post(format!("{base}/admin/wallet"))
        .form(&[
            ("_csrf", ph3_csrf.as_str()),
            ("_idem_grant", ph3_idem.as_str()),
            ("_action", "grant"),
            ("player_id", ph3_guest.player_id.as_str()),
            ("currency", "gold"),
            ("amount", "55"),
            ("reason", "splitproof push nudge"),
        ])
        .send()
        .await?;
    let ph3_grant_code = ph3_grant.status().as_u16();
    let ph3_frame = match ph3.as_mut() {
        Ok(client) => {
            client
                .await_topic(notificationsapi::PUSH_NEW_TOPIC, Duration::from_secs(30))
                .await
        }
        Err(_) => None,
    };
    // The nudge carries NO row id (constraint 8: it is sent inside the delivery
    // transaction, so an id would name a row a rollback plus redelivery never commits).
    // Asserting the absence here is what keeps a future "helpful" id from being added
    // without the post-commit hook that would make it true.
    let ph3_id_free = match &ph3_frame {
        Some(Frame::Message { payload, .. }) => serde_json::from_slice::<serde_json::Value>(payload)
            .map(|v| v.is_object() && v.get("id").is_none())
            .unwrap_or(false),
        _ => false,
    };
    let ph3_row = poll_count(
        pool,
        "SELECT count(*) FROM notifications.messages \
          WHERE player_id::text = $1 AND kind = 'wallet.credit'",
        &ph3_guest.player_id,
        1,
    )
    .await;
    p.check(
        &format!(
            "[PH3{tag}] durable wallet.changed -> notifications-svc inbox row -> id-free \
             `{}` frame at a client on the front",
            notificationsapi::PUSH_NEW_TOPIC
        ),
        ph3_code == 201
            && ph3_ack
            && ph3_grant_code == 303
            && ph3_frame.is_some()
            && ph3_id_free
            && ph3_row,
        format!(
            "guest={ph3_code} ack={ph3_ack} grant={ph3_grant_code} frame={} id_free={ph3_id_free} \
             inbox_row={ph3_row} pid={}",
            ph3_frame.is_some(),
            ph3_guest.player_id
        ),
    );

    let mut ph5_watcher: Option<PushClient> = None;
    if split {
        // [PH4] groups. NOTHING in the fleet produces a group-addressed message — a group
        // is a client-built audience, so the resolution path would ship with nothing
        // executing it — and the batch is therefore injected straight into the front's
        // inbound `push.deliver` face over the internal mTLS edge, which is exactly what
        // `core/remote`'s sender does. Only the producer is the harness; the codec, the
        // wire method and the handler are production.
        //
        // The non-member's negative is proven BY CONSTRUCTION, not by absence: after it
        // fails to see the group message, the SAME socket is addressed with `Target::All`
        // and must receive that one. A dead or unread socket would fail the second half,
        // so silence on the first half can only mean it was not addressed.
        let group = format!("splitproof-{suffix}");
        let group_topic = format!("splitproof.group.{suffix}");
        let all_topic = format!("splitproof.all.{suffix}");
        let (_, ga) = create_guest(ctx, base).await?;
        let (_, gb) = create_guest(ctx, base).await?;
        let (_, gc) = create_guest(ctx, base).await?;
        let mut a = PushClient::connect(&ws, &ga.token, "dev-key-client").await?;
        let mut b = PushClient::connect(&ws, &gb.token, "dev-key-client").await?;
        let mut c = PushClient::connect(&ws, &gc.token, "dev-key-client").await?;
        let acks = matches!(
            a.next_frame(Duration::from_secs(10)).await,
            Ok(Some(Frame::Ack { .. }))
        ) && matches!(
            b.next_frame(Duration::from_secs(10)).await,
            Ok(Some(Frame::Ack { .. }))
        ) && matches!(
            c.next_frame(Duration::from_secs(10)).await,
            Ok(Some(Frame::Ack { .. }))
        );
        a.join(&group).await?;
        b.join(&group).await?;
        // A join is silent by design (there is no join ack), so the batch is RE-SENT until
        // both members have seen one rather than slept-then-checked: nothing orders the
        // verb against a delivery that started in another process.
        let mut got_a = false;
        let mut got_b = false;
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !(got_a && got_b) {
            deliver(vec![push::Envelope::new(
                push::Target::Group(group.clone()),
                push::Message::new(group_topic.clone(), b"grouped".to_vec()),
            )])
            .await?;
            if !got_a {
                got_a = a.await_topic(&group_topic, Duration::from_millis(750)).await.is_some();
            }
            if !got_b {
                got_b = b.await_topic(&group_topic, Duration::from_millis(750)).await.is_some();
            }
        }
        let c_group = c.await_topic(&group_topic, Duration::from_secs(3)).await;
        deliver(vec![push::Envelope::new(
            push::Target::All,
            push::Message::new(all_topic.clone(), b"broadcast".to_vec()),
        )])
        .await?;
        let c_all = c.await_topic(&all_topic, Duration::from_secs(15)).await;
        p.check(
            &format!(
                "[PH4{tag}] two joiners receive a Target::Group batch; a non-member does not, \
                 yet takes the next Target::All on the same socket"
            ),
            acks && got_a && got_b && c_group.is_none() && c_all.is_some(),
            format!(
                "acks={acks} member_a={got_a} member_b={got_b} nonmember_group={} \
                 nonmember_all={}",
                c_group.is_some(),
                c_all.is_some()
            ),
        );
        a.disconnect().await;
        b.disconnect().await;
        c.disconnect().await;

        // [PH5] presence, which the Proof fleet turns on with `PUSH_PRESENCE=1`
        // (default-off in `gateway::PushLimits` and left off for the Development fleet —
        // see tools/processctl/src/fleet.rs). The watcher must observe BOTH edges: an
        // arrival it did not cause, and the departure of that same connection. A proof of
        // only the first half would pass while `Slot::drop`'s announcement — the one that
        // runs on an aborted or panicked task — never fired at all.
        let (_, gw) = create_guest(ctx, base).await?;
        let (_, gv) = create_guest(ctx, base).await?;
        let mut watcher = PushClient::connect(&ws, &gw.token, "dev-key-client").await?;
        let watcher_ack = matches!(
            watcher.next_frame(Duration::from_secs(10)).await,
            Ok(Some(Frame::Ack { .. }))
        );
        let mut visitor = PushClient::connect(&ws, &gv.token, "dev-key-client").await?;
        let visitor_ack = matches!(
            visitor.next_frame(Duration::from_secs(10)).await,
            Ok(Some(Frame::Ack { .. }))
        );
        let online = watcher
            .await_presence(&gv.player_id, true, Duration::from_secs(15))
            .await;
        visitor.disconnect().await;
        let offline = watcher
            .await_presence(&gv.player_id, false, Duration::from_secs(15))
            .await;
        p.check(
            &format!("[PH5{tag}] PUSH_PRESENCE=1: a second client's arrival AND its departure reach the first"),
            watcher_ack && visitor_ack && online && offline,
            format!(
                "watcher_ack={watcher_ack} visitor_ack={visitor_ack} online={online} \
                 offline={offline} visitor={}",
                gv.player_id
            ),
        );
        ph5_watcher = Some(watcher);
    }

    if split {
        // [PH6] best-effort, proven with NOTHING connected: every socket this proof opened
        // is closed first and the grant names a player nobody ever authenticated as, so
        // the nudge reaches zero connections on the only front there is.
        //
        // The domain outcome must be untouched — and the subscription must still be
        // `active` with no consecutive failures, which is the half that matters: the nudge
        // runs INSIDE the durable delivery transaction, and a handler that returned `Err`
        // over a best-effort concern would back off and pause the subscription for EVERY
        // player until an operator ran `eventctl` (constraint 12).
        if let Ok(client) = ph1 {
            client.disconnect().await;
        }
        if let Ok(client) = ph3 {
            client.disconnect().await;
        }
        if let Some(client) = ph5_watcher {
            client.disconnect().await;
        }
        let ph6_player: String = sqlx::query_scalar("SELECT gen_random_uuid()::text")
            .fetch_one(pool)
            .await?;
        let (ph6_csrf, ph6_idem, _) = wallet_form(admin, base).await?;
        let ph6_grant = admin
            .post(format!("{base}/admin/wallet"))
            .form(&[
                ("_csrf", ph6_csrf.as_str()),
                ("_idem_grant", ph6_idem.as_str()),
                ("_action", "grant"),
                ("player_id", ph6_player.as_str()),
                ("currency", "gold"),
                ("amount", "13"),
                ("reason", "splitproof push best-effort"),
            ])
            .send()
            .await?;
        let ph6_grant_code = ph6_grant.status().as_u16();
        let ph6_row = poll_count(
            pool,
            "SELECT count(*) FROM notifications.messages \
              WHERE player_id::text = $1 AND kind = 'wallet.credit'",
            &ph6_player,
            1,
        )
        .await;
        let ph6_sub: Option<(String, i32)> = sqlx::query_as(
            "SELECT state, consecutive_failures FROM asyncevents.subscriptions \
              WHERE subscription_id = 'notifications.wallet-changed.v1'",
        )
        .fetch_optional(pool)
        .await
        .unwrap_or(None);
        let ph6_healthy = matches!(&ph6_sub, Some((state, failures)) if state == "active" && *failures == 0);
        p.check(
            &format!(
                "[PH6{tag}] with nobody connected the grant still commits, the inbox row lands \
                 and the subscription stays active"
            ),
            ph6_grant_code == 303 && ph6_row && ph6_healthy,
            format!(
                "grant={ph6_grant_code} inbox_row={ph6_row} subscription={ph6_sub:?} pid={ph6_player}"
            ),
        );
    } else {
        if let Ok(client) = ph1 {
            client.disconnect().await;
        }
        if let Ok(client) = ph3 {
            client.disconnect().await;
        }
    }

    Ok(())
}

async fn assertions(ctx: &Ctx, pool: &PgPool, idp: &Idp, p: &mut Proof) -> Result<()> {
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
        .json(&serde_json::json!({"ReportId": format!("k3-{suffix}"), "Winner": harness_player("k3-w"), "Loser": harness_player("k3-l")}))
        .send()
        .await?;
    p.check("[K3] client key on match.report -> 403", k3.status().as_u16() == 403, k3.status());
    // [K4] dev-key-server on match.report -> 202 (full policy).
    let k4 = ctx
        .http
        .post(format!("{g}/match/report"))
        .header("X-Api-Key", "dev-key-server")
        .json(&serde_json::json!({"ReportId": format!("k4-{suffix}"), "Winner": harness_player("k4-w"), "Loser": harness_player("k4-l")}))
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

        // [4c] delete via a NON-canonical (uppercased) id spelling and prove the durable
        // character.deleted event carries the DB-CANONICAL id — the split path (G -> A ->
        // emit) canonicalizes regardless of how the client spelled the URL id.
        let canon_cid = create_character(ctx, &g, &tok, "Canon").await;
        p.check("[4c] Canon character created", canon_cid.is_some(), format!("cid={canon_cid:?}"));
        if let Some(cidc) = canon_cid {
            let upper = cidc.to_uppercase();
            let delc = ctx.http.delete(format!("{g}/characters/{upper}"))
                .header("X-Api-Key", "dev-key-client")
                .header("Authorization", format!("Bearer {tok}"))
                .send().await?;
            p.check("[4c] delete via uppercased id -> 204", delc.status().as_u16() == 204, delc.status());
            // canonical (lowercase) event present ...
            let canon_present = poll_count(
                pool,
                "SELECT count(*) FROM asyncevents.events WHERE topic='character.deleted' AND payload->>'character_id'=$1",
                &cidc, 1,
            ).await;
            p.check("[4c] character.deleted carries canonical id", canon_present, format!("cid={cidc}"));
            // ... and the uppercase spelling was NOT emitted (one-shot after canonical confirmed).
            // Guarded: uppercase is a no-op for an all-digit uuid, so only assert this when the
            // spelling actually differs (else it degenerates into re-checking the canonical id).
            if upper != cidc {
                let upper_count: Option<i64> = sqlx::query_scalar(
                    "SELECT count(*) FROM asyncevents.events WHERE topic='character.deleted' AND payload->>'character_id'=$1",
                ).bind(&upper).fetch_optional(pool).await.ok().flatten();
                p.check("[4c] no non-canonical id emitted", upper_count == Some(0), format!("rows={upper_count:?}"));
            }
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

        // [6] configurable per-player cap enforced through G -> A, cap value resolved via the
        // split's REMOTE CachedConfig (the at-risk path the unit tests can't cover).
        let c6 = sqlx::query(
            "INSERT INTO config.settings (namespace,key,value) VALUES ('characters','max_per_player','2') \
             ON CONFLICT (namespace,key) DO UPDATE SET value=excluded.value",
        ).execute(pool).await;
        p.check("[6] set characters/max_per_player=2", c6.is_ok(), "");
        // The cap propagates to A via CachedConfig invalidation (revision-gated, eventually
        // consistent). Each attempt uses a fresh player (0 chars) and creates 2 (allowed) then
        // probes a 3rd: a 409 can occur ONLY once the cap propagated to <= 2, so a 409 within
        // the bound proves remote-config enforcement.
        let cap_enforced = {
            let mut ok = false;
            for i in 0..30 {
                if let Ok(ptok) = register_login(ctx, &g, &format!("cap-{suffix}-{i}@test.local")).await {
                    let _ = create_character(ctx, &g, &ptok, "Cap1").await;
                    let _ = create_character(ctx, &g, &ptok, "Cap2").await;
                    if create_character_status(ctx, &g, &ptok, "Cap3").await == 409 { ok = true; break; }
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            ok
        };
        p.check("[6] create past remote-resolved cap -> 409", cap_enforced, "");
        // NOTE: the shared `poll_fresh_grant` helper creates one live character per iteration
        // on the passed player, so against the DEFAULT cap (10) a very slow grant propagation
        // could accumulate chars toward the cap — flagged as a known consideration for the live
        // run, not rewritten here (touching the working config-reload helper risks more than it
        // fixes).
        // RESET so cap=2 cannot leak into later split assertions or the fresh monolith re-run.
        sqlx::query("DELETE FROM config.settings WHERE namespace='characters' AND key='max_per_player'")
            .execute(pool).await.ok();
        // Barrier: confirm the cap reverted on characters-svc (remote CachedConfig is eventually
        // consistent) BEFORE any later main-player create ([AD0]) — else a stale cap=2 false-fails it.
        let reverted = {
            let mut ok = false;
            for i in 0..30 {
                if let Ok(rtok) = register_login(ctx, &g, &format!("capreset-{suffix}-{i}@test.local")).await {
                    let _ = create_character(ctx, &g, &rtok, "R1").await;
                    let _ = create_character(ctx, &g, &rtok, "R2").await;
                    if create_character_status(ctx, &g, &rtok, "R3").await == 201 { ok = true; break; }
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            ok
        };
        p.check("[6r] cap reverted to default on characters-svc", reverted, "");
    }

    // --- Match / rating / leaderboard: durable match.finished projection + idempotency. ---
    let winner = harness_player(&format!("champ-{suffix}"));
    let loser = harness_player(&format!("chump-{suffix}"));
    let mt1_rid = format!("mt1-{suffix}");
    let mt4_rid = format!("mt4-{suffix}");
    // [MT1] report -> 202 (AuthNone, capitalized body keys; emits durable match.finished).
    let mt1 = report(ctx, &g, &mt1_rid, &winner, &loser).await;
    p.check("[MT1] match.report -> 202", mt1 == 202, format!("code={mt1}"));
    // [MT2] the durable projection landed: winner's `leaderboard.scores` row is wins=1
    // (I->K durable + upsert), asserted on the row itself so the truncated top-100 page can
    // never mask it. The Remote-routing half is [MT2-ROUTE].
    p.check("[MT2] leaderboard winner wins=1", poll_leaderboard_wins(pool, &winner, 1).await, "");
    // [MT2-ROUTE] G routes the public read Remote to leaderboard-svc: `GET /leaderboard`
    // answers 200 through the front door with a well-formed, non-empty `[{player,wins}]` list
    // ([MT2] just proved at least one row exists, so an empty list is a routing/projection
    // failure, not a vacuous pass).
    let (lb_code, lb_body) = leaderboard_top(ctx, &g).await;
    let lb_wellformed = serde_json::from_str::<Vec<serde_json::Value>>(&lb_body)
        .map(|rows| {
            !rows.is_empty()
                && rows.iter().all(|r| {
                    r.get("player").and_then(|v| v.as_str()).is_some()
                        && r.get("wins").and_then(|v| v.as_i64()).is_some()
                })
        })
        .unwrap_or(false);
    p.check(
        "[MT2-ROUTE] GET /leaderboard -> 200 well-formed list through G (Remote to K)",
        lb_code == 200 && lb_wellformed,
        format!("code={lb_code} body={}", lb_body.chars().take(160).collect::<String>()),
    );
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
        mt4 == 202 && poll_leaderboard_wins(pool, &winner, 2).await,
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

    // --- [D4] Routing-as-data live proof (Phase D payoff) --------------------------------
    // gateway-svc boots in DESCRIBE-ROUTING mode: `cmd/gateway-svc` calls
    // `Gateway::new().with_describe_routing()` and the compile-time `<name>rpc` route imports
    // are REMOVED (the pure-HTTP providers are `Stub::describe_peer`, contributing ONLY their
    // PEER_SLOT address set — zero route factories). So the ONLY source of an op route in this
    // process is each peer's runtime `__describe` manifest, fetched over the mTLS edge in
    // `Stub::start` and rebuilt on a 5s loop. Every HTTP-through-gateway assertion above (K1-K5,
    // MT1-MT6, the leaderboard/characters/inventory/accounts reads) therefore ALREADY routes
    // PURELY via describe by construction; these three name it explicitly and pin the two
    // recorded contract properties.

    // [D4-ROUTE] routing-as-data: an HTTP op reaches the RIGHT svc THROUGH gateway-svc under
    // describe-routing. `POST /match/report` -> match-svc, whose durable side effect is a
    // `match.matches` DB row written by the process that actually handled the request. The route
    // (POST /match/report, 202, AuthNone, the `Winner`/`Loser`/`ReportId` body-name renames) was
    // reconstructed from match-svc's `__describe` manifest — NO compile-time `matchrpc` route
    // import exists in gateway-svc — so this row landing IS the describe-driven route reaching
    // the right peer, not a hand-wired import.
    let d4_rid = format!("d4-route-{suffix}");
    let d4_code = report(ctx, &g, &d4_rid, &winner, &loser).await;
    let d4_row: Option<i64> = sqlx::query_scalar("SELECT count(*) FROM match.matches WHERE report_id=$1")
        .bind(&d4_rid)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    p.check(
        "[D4-ROUTE] describe-routed match.report reaches match-svc (DB row)",
        d4_code == 202 && d4_row == Some(1),
        format!("code={d4_code} rows={d4_row:?}"),
    );

    // [D4-DESCRIBE-404] purely-from-describe (no forward-everything fallback): a well-formed HTTP
    // request to an op path present in NO peer's `__describe` must 404 at the front door — the
    // gateway serves ONLY described routes, it is a describe-driven ROUTER, not a reverse proxy
    // that blindly forwards. This is the LIVE complement to the unit-level fail-closed branch
    // (`modules/gateway/src/tests.rs::peer_down_at_boot_is_routed_after_a_refetch`: a manifest
    // OMITTING an op contributes NO route) and the collision-bail. The stronger decoy form (a
    // REACHABLE peer serving a PARTIAL `__describe`) is DEFERRED: neither live harness makes it
    // cheap — `weles-managed-gateway`'s fake peer is a bare UDP datagram sink (no real edge
    // server, so it cannot answer a hand-crafted `__describe`), and a splitproof decoy would need
    // a net-new mTLS edge-server binary serving a partial DescribeManifest. The
    // omitted-op->not-routed branch is thus pinned by construction (imports removed) + that unit
    // test + this live no-fallback 404.
    let d4_404 = send_status_retrying_429(
        ctx.http
            .post(format!("{g}/match/undescribed-op-{suffix}"))
            .header("X-Api-Key", "dev-key-server")
            .json(&serde_json::json!({ "x": 1 })),
    )
    .await;
    p.check(
        "[D4-DESCRIBE-404] undescribed op path -> 404 (describe router, no proxy fallback)",
        d4_404 == 404,
        format!("code={d4_404}"),
    );

    // [D4-ILLTYPED] caveat (iv) pinned live (core/opsapi/src/databind.rs): a
    // well-formed-but-ILL-TYPED body (`Winner` a JSON number where a String is expected) routes
    // through the describe gateway, which holds NO field types (the `__describe` manifest carries
    // each arg's SOURCE + wire key, never its Rust type), so it can only check the body is a JSON
    // object. The type mismatch is therefore caught SVC-SIDE, at match-svc's generated adapter
    // `from_slice::<ReportRequest>` — but the CALLER-visible answer is now the SAME as the
    // monolith's: that decode site wraps its failure in `edge::InvalidRequestBody`, match-svc's
    // dispatch stamps `ResponseCode::InvalidRequest`, gateway-svc's edge client types it
    // `edge::Error::InvalidRequest` and `From<edge::Error> for opsapi::Error` maps it to
    // `Status::Invalid` => 400 (`Status::http()`). Only WHERE the body is caught is
    // topology-dependent; WHAT the caller sees is not.
    //
    // This assertion was `>= 500 && != 400` — the pre-fix contract, where the ill-typed body
    // came back to the front as `Unavailable`/503 while the monolith answered 400 for the same
    // bytes. It is now the parity assertion: the ONE previously-wrong branch on the real
    // gateway-svc -> match-svc split, which no monolith unit test can reach (the monolith never
    // crosses the edge for this call).
    let d4_ill = send_status_retrying_429(
        ctx.http
            .post(format!("{g}/match/report"))
            .header("X-Api-Key", "dev-key-server")
            .json(&serde_json::json!({
                "ReportId": format!("d4-ill-{suffix}"),
                "Winner": 123,
                "Loser": "bob"
            })),
    )
    .await;
    p.check(
        "[D4-ILLTYPED] ill-typed body -> 400 across the split (monolith parity, caveat iv)",
        d4_ill == 400,
        format!("code={d4_ill} (want 400 = Status::Invalid.http(); pre-fix this was 503)"),
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
        // [P7-ILLTYPED] the player twin of [D4-ILLTYPED]: the SAME 400 class over the
        // player QUIC plane. `handle_player`'s well-formedness gate rejects only MALFORMED
        // json (`from_slice::<&RawValue>`), so a well-formed body whose `name` is a JSON
        // number passes the front, dispatches Remote to characters-svc, and fails at that
        // svc's generated adapter decode — the `edge::InvalidRequestBody` site. The front
        // renders the mapped `opsapi::Error` verbatim, so the envelope status must be
        // `Invalid` (pre-fix: `Unavailable`). Not vacuous: [P1] proves the identical call
        // with a well-typed `name` succeeds over this same connection path.
        let p7 = player_call(ctx, Some(&tok), "characters.create", r#"{"name":123,"class":""}"#).await;
        let p7_status = p7
            .as_ref()
            .ok()
            .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(String::from));
        p.check(
            "[P7-ILLTYPED] QUIC ill-typed body -> Invalid (400 class, not Unavailable)",
            p7_status.as_deref() == Some("Invalid"),
            format!("status={p7_status:?} err={:?}", p7.as_ref().err().map(|e| e.to_string())),
        );
    }

    // --- Admin portal (session auth) + audit ledger, cross-process over QUIC. ---
    let admin = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let aproof = format!("AdminProof-{suffix}");
    // [AD0] a character for the admin table to render (through G -> A). Its uuid is
    // reused by [ADX3]'s character-modal fetch (`?owner=character:<uuid>&partial=modal`).
    let mut ad0_char_id: Option<String> = None;
    if let Some(tok) = token.clone() {
        let acid = create_character(ctx, &g, &tok, &aproof).await;
        p.check("[AD0] admin-proof character created", acid.is_some(), format!("id={acid:?}"));
        ad0_char_id = acid;
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

    // --- [ADX] Cross-process admin EXTENSION POINTS through the REAL split. ---
    // The at-risk topology: extension entries (`ExtensionEntry`) declared by
    // characters-svc / inventory-svc ride `admin.adminData` (ItemData.extensions) over
    // the edge into admin-svc, which merges them into the OWNER page's render (accounts'
    // Players page). The whole cross-process menu/modal seam is monolith-only unless
    // proven here. Params now cross the wire too, so a remote drill-down renders SCOPED.
    //
    // [ADX1] /admin/players (accounts page, slug = slugify("Players")) -> 200 carrying
    // BOTH extension labels — "View Characters" (from characters-svc) and "View
    // Inventory" (from inventory-svc) — proving extensions travel the edge into the
    // owner's row `⋯` menu.
    let adx1 = admin.get(format!("{g}/admin/players")).send().await?;
    let (adx1_code, adx1_body) = (adx1.status().as_u16(), adx1.text().await.unwrap_or_default());
    let adx1_chars = adx1_body.contains("View Characters");
    let adx1_inv = adx1_body.contains("View Inventory");
    p.check(
        "[ADX1] /admin/players -> 200 + cross-process row-menu extensions",
        adx1_code == 200 && adx1_chars && adx1_inv,
        format!("code={adx1_code} view_characters={adx1_chars} view_inventory={adx1_inv}"),
    );

    // [ADX2] scoped remote render: register a SECOND player + character (every character
    // seeded above belongs to the one AD0 test player, so the negative has no fixture
    // without this), then GET the characters page scoped to the AD0 player. The AD0 proof
    // character MUST appear and the second player's character MUST NOT — proving the
    // `owner` param crosses the wire and characters-svc renders per-player.
    let adx_other = format!("AdxOther-{suffix}");
    let adx2 = if let Some(pid) = player_id.as_deref() {
        if let Ok(other_tok) = register_login(ctx, &g, &format!("adxother-{suffix}@test.local")).await {
            let _ = create_character(ctx, &g, &other_tok, &adx_other).await;
        }
        let r = admin.get(format!("{g}/admin/characters?owner=player:{pid}")).send().await?;
        let (code, body) = (r.status().as_u16(), r.text().await.unwrap_or_default());
        (code, body.contains(&aproof), body.contains(&adx_other))
    } else {
        (0, false, false)
    };
    p.check(
        "[ADX2] /admin/characters?owner=player:<pid> -> scoped remote render",
        adx2.0 == 200 && adx2.1 && !adx2.2,
        format!("code={} has_aproof={} has_other={}", adx2.0, adx2.1, adx2.2),
    );

    // [ADX3] modal fragment vs no-JS degradation. WITH `HX-Request: true`, the
    // character-detail URL `?partial=modal` returns a FRAGMENT (modal chrome + the
    // inventory-svc "View Inventory" ModalActions footer, NO page shell). WITHOUT the
    // header the SAME URL degrades to the full page (progressive enhancement).
    let adx3 = if let Some(cid) = ad0_char_id.as_deref() {
        let url = format!("{g}/admin/characters?owner=character:{cid}&partial=modal");
        let frag = admin.get(&url).header("HX-Request", "true").send().await?;
        let (fc, fb) = (frag.status().as_u16(), frag.text().await.unwrap_or_default());
        let frag_ok = fc == 200
            && fb.contains("class=\"modal\"")
            && fb.contains("View Inventory")
            && !fb.contains("class=\"sidebar\"");
        let full = admin.get(&url).send().await?;
        let (pc, pb) = (full.status().as_u16(), full.text().await.unwrap_or_default());
        let full_ok = pc == 200 && pb.contains("class=\"sidebar\"");
        (fc, frag_ok, pc, full_ok)
    } else {
        (0, false, 0, false)
    };
    p.check(
        "[ADX3] modal fragment (HX-Request) -> chrome + inventory action, no shell",
        adx3.1,
        format!("frag_code={} ok={}", adx3.0, adx3.1),
    );
    p.check(
        "[ADX3b] no-HX-Request same URL -> full page (no-JS degradation)",
        adx3.3,
        format!("full_code={} ok={}", adx3.2, adx3.3),
    );

    // [AD3b] /admin/api-keys WITH session -> 200 + the RICH roles/keys configurator (E -> L
    // QUIC, two hops). The page content changed from the old flat "dev-client" checkbox +
    // plaintext-key form to the rich form: a keys TABLE with a Prefix column (never the
    // secret), a role Select, and the create-key action that reveals a one-time secret.
    // Assert the NEW structure the OLD flat form could not have: the "Prefix" table column
    // header and the create-key action label are present, real role/key data ("dev-client")
    // resolved over the edge, AND — crucially — the full dev-server plaintext secret
    // ("dev-key-server", whose 12-char prefix "dev-key-serv" is all the table may show) never
    // appears (the old form rendered raw keys; the rich form hashes + shows only the prefix).
    let ad3b = admin.get(format!("{g}/admin/api-keys")).send().await?;
    let (ad3b_code, ad3b_body) = (ad3b.status().as_u16(), ad3b.text().await.unwrap_or_default());
    let ad3b_ok = ad3b_code == 200
        && ad3b_body.contains("dev-client")
        && ad3b_body.contains("Prefix")
        && ad3b_body.contains("reveals a one-time secret")
        && !ad3b_body.contains("dev-key-server");
    p.check(
        "[AD3b] /admin/api-keys -> rich configurator (Prefix col, create-key action, no plaintext secret)",
        ad3b_ok,
        format!(
            "code={ad3b_code} prefix={} action={} plaintext_leak={}",
            ad3b_body.contains("Prefix"),
            ad3b_body.contains("reveals a one-time secret"),
            ad3b_body.contains("dev-key-server"),
        ),
    );
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

    // --- [AD6] Cross-process rich-form key/role write through the REAL split. ---
    // The at-risk topology: gateway-svc (/admin passthrough) -> admin-svc (session/CSRF,
    // renders the REMOTE form fetched over the edge, submit=None) -> edge `admin.adminSubmit`
    // -> apikeys-svc (runs `apply_submit` server-side, store-local). The whole generic
    // remote-admin-write seam is monolith-only unless proven here. Each sub-check pins:
    //   [AD6a] an authenticated configurator session (login parity with AD3).
    //   [AD6b] a role CREATED across processes — DB row with the exact policy + rev=1.
    //   [AD6c] a key CREATED remotely, its show-once secret revealed via PRG-flash (303 to
    //          ?reveal=<token>, GET consumes the one-shot token, secret in the reveal panel).
    //   [AD6d] the stored `secret_hash` == base64url(sha256(<revealed secret>)) recomputed
    //          identically to modules/apikeys/src/store.rs, the FK `role` points at the new
    //          role, and NO column of the row holds the secret in cleartext (finding #11).
    //   [AD6e] the minted key AUTHENTICATES end-to-end — 200 on an allowed op, 403 on a
    //          disallowed one — proving the keys->roles JOIN resolves the effective policy
    //          across processes (edit the role, every key follows).
    //   [AD6f] finding #2: a create-key on a MISSING role surfaces as a 409 conflict card,
    //          NOT a 405 "read-only" (a domain-missing target must never collapse to the
    //          edge's UnknownMethod->NotFound->405) — and writes no row.
    //   [AD6g] finding #3: a successful REMOTE submit is audited uniformly by admin-svc's
    //          plane (admin.action{form-submit}), not left to the provider process.
    // A long-timeout, cookie-bearing, redirect-none client (like AD2b/AD2c's `slow`, plus a
    // cookie jar) drives it sequentially — never a shell-loop / short-timeout client that
    // could resurrect the AD2b/AD2c deadlock class.
    let role_name = format!("proofrole-{suffix}");
    let key_name = format!("proofkey-{suffix}");
    let orphan_role = format!("ghostrole-{suffix}");
    let orphan_key = format!("orphankey-{suffix}");
    // Pre-clean (keys before roles: FK NO-ACTION order) so a reused pid can't 409 a create.
    for n in [&key_name, &orphan_key] {
        sqlx::query("DELETE FROM apikeys.keys WHERE name = $1").bind(n).execute(pool).await.ok();
    }
    for n in [&role_name, &orphan_role] {
        sqlx::query("DELETE FROM apikeys.roles WHERE name = $1").bind(n).execute(pool).await.ok();
    }
    // Baseline the audit count BEFORE any successful submit (finding #3 delta).
    let audit_before: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM asyncevents.events \
         WHERE topic='admin.action' AND payload->>'actor'='proofadmin' \
           AND payload->>'target'='api-keys' AND payload->>'action'='form-submit'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    let cfg = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let cfg_login = cfg
        .post(format!("{g}/admin/login"))
        .form(&[("username", "proofadmin"), ("password", "proofpass")])
        .send()
        .await?;
    p.check("[AD6a] configurator admin session", cfg_login.status().as_u16() == 303, cfg_login.status());

    // A fresh `_csrf` (per-session, stable) scraped from the rendered form each POST.
    async fn admin_csrf(cfg: &reqwest::Client, g: &str) -> Result<String> {
        let page = cfg
            .get(format!("{g}/admin/api-keys"))
            .send()
            .await?
            .text()
            .await
            .unwrap_or_default();
        Ok(extract_form_fields(&page)
            .into_iter()
            .find(|(k, _)| k == "_csrf")
            .map(|(_, v)| v)
            .unwrap_or_default())
    }

    // [AD6b] create a role whose policy allows exactly `leaderboard.topScores`.
    let csrf = admin_csrf(&cfg, &g).await?;
    let create_role = cfg
        .post(format!("{g}/admin/api-keys"))
        .form(&[
            ("_csrf", csrf.as_str()),
            ("_action", "create_role"),
            ("role_name", role_name.as_str()),
            ("role_policy", "leaderboard.topScores"),
        ])
        .send()
        .await?;
    let cr_status = create_role.status().as_u16();
    let role_row: Option<(String, i64)> =
        sqlx::query_as("SELECT policy, revision FROM apikeys.roles WHERE name = $1")
            .bind(&role_name)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    let role_ok = cr_status == 303
        && role_row
            .as_ref()
            .map(|(policy, rev)| policy == "leaderboard.topScores" && *rev == 1)
            .unwrap_or(false);
    p.check(
        "[AD6b] remote create-role -> 303 + apikeys.roles row (policy, rev=1)",
        role_ok,
        format!("status={cr_status} row={role_row:?}"),
    );

    // [AD6c] create a key referencing that role; capture the show-once secret via PRG-flash.
    let csrf = admin_csrf(&cfg, &g).await?;
    let create_key = cfg
        .post(format!("{g}/admin/api-keys"))
        .form(&[
            ("_csrf", csrf.as_str()),
            ("_action", "create_key"),
            ("key_name", key_name.as_str()),
            ("key_role", role_name.as_str()),
        ])
        .send()
        .await?;
    let ck_status = create_key.status().as_u16();
    let reveal_loc = create_key
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    // Follow the 303 to ?reveal=<token>; the GET consumes the one-shot token and renders
    // the secret once in a readonly `reveal_1` input (scraped like any other form field).
    let reveal_url = if reveal_loc.starts_with("http") {
        reveal_loc.clone()
    } else {
        format!("{g}{reveal_loc}")
    };
    let reveal_page = cfg.get(&reveal_url).send().await?.text().await.unwrap_or_default();
    let secret = extract_form_fields(&reveal_page)
        .into_iter()
        .find(|(k, _)| k == "reveal_1")
        .map(|(_, v)| v)
        .unwrap_or_default();
    p.check(
        "[AD6c] remote create-key -> 303 + show-once secret revealed",
        ck_status == 303 && reveal_loc.contains("reveal=") && secret.starts_with("ak_"),
        format!("status={ck_status} loc={reveal_loc} secret_len={}", secret.len()),
    );

    // [AD6d] the persisted row: secret_hash == base64url(sha256(secret)), role FK correct,
    // and no column holds the plaintext secret.
    use base64::Engine as _;
    use sha2::{Digest as _, Sha256};
    // The two typed reads the specific checks need: the stored hash (for the digest
    // equality) and the FK role.
    let key_meta: Option<(String, String)> =
        sqlx::query_as("SELECT secret_hash::text, role::text FROM apikeys.keys WHERE name = $1")
            .bind(&key_name)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    // The no-cleartext scan is SCHEMA-DRIFT-PROOF: reflect the LIVE column set of
    // apikeys.keys from information_schema (so a future cleartext-bearing column can't
    // silently escape the secret-secrecy invariant) and assert the scraped secret appears
    // in NONE of the row's column values. Each column is cast to text and joined with
    // chr(31) (unit separator, which a base64url secret can't contain, so it can't mask a
    // match straddling a column boundary); concat_ws skips NULLs.
    let key_columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns \
          WHERE table_schema = 'apikeys' AND table_name = 'keys' ORDER BY ordinal_position",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let combined: Option<String> = if key_columns.is_empty() {
        None
    } else {
        let casts = key_columns
            .iter()
            .map(|c| format!("\"{c}\"::text"))
            .collect::<Vec<_>>()
            .join(", ");
        sqlx::query_scalar(&format!(
            "SELECT concat_ws(chr(31), {casts}) FROM apikeys.keys WHERE name = $1"
        ))
        .bind(&key_name)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
    };
    let recomputed =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_bytes()));
    let hash_ok = key_meta.as_ref().map(|(h, _)| h == &recomputed).unwrap_or(false);
    let role_fk_ok = key_meta.as_ref().map(|(_, r)| r == &role_name).unwrap_or(false);
    // Require a reflected, non-empty column set AND a present row: a scan that found no
    // columns (or no row) is a FAILURE, never a vacuous pass.
    let no_cleartext = !secret.is_empty()
        && !key_columns.is_empty()
        && combined.as_ref().map(|c| !c.contains(&secret)).unwrap_or(false);
    p.check(
        "[AD6d] key row: secret_hash==base64url(sha256), role FK, no cleartext (reflected cols)",
        hash_ok && role_fk_ok && no_cleartext,
        format!(
            "present={} cols={} hash_ok={hash_ok} role_fk={role_fk_ok} no_cleartext={no_cleartext}",
            key_meta.is_some(),
            key_columns.len(),
        ),
    );

    // [AD6e] the minted key authenticates across processes: 200 on the allowed op,
    // 403 on a disallowed one (transient gateway 429s retried past, never counted).
    let mut auth_code = 0u16;
    for _ in 0..10 {
        auth_code = ctx
            .http
            .get(format!("{g}/leaderboard"))
            .header("X-Api-Key", secret.as_str())
            .send()
            .await?
            .status()
            .as_u16();
        if auth_code != 429 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let mut deny_code = 0u16;
    for _ in 0..10 {
        deny_code = ctx
            .http
            .post(format!("{g}/match/report"))
            .header("X-Api-Key", secret.as_str())
            .json(&serde_json::json!({"ReportId": format!("ad6-{suffix}"), "Winner": "w", "Loser": "l"}))
            .send()
            .await?
            .status()
            .as_u16();
        if deny_code != 429 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    p.check(
        "[AD6e] minted key authenticates cross-process: leaderboard 200, match.report 403",
        auth_code == 200 && deny_code == 403,
        format!("leaderboard={auth_code} report={deny_code}"),
    );

    // [AD6f] finding #2: create-key on a MISSING role -> 409 conflict card, NOT 405
    // read-only, and NO row written (the FK 23503 -> WriteError::Conflict -> Status::Conflict
    // -> 409, never the edge UnknownMethod->NotFound->405 collapse).
    let csrf = admin_csrf(&cfg, &g).await?;
    let orphan = cfg
        .post(format!("{g}/admin/api-keys"))
        .form(&[
            ("_csrf", csrf.as_str()),
            ("_action", "create_key"),
            ("key_name", orphan_key.as_str()),
            ("key_role", orphan_role.as_str()),
        ])
        .send()
        .await?;
    let orphan_status = orphan.status().as_u16();
    let orphan_rows: Option<i64> =
        sqlx::query_scalar("SELECT count(*) FROM apikeys.keys WHERE name = $1")
            .bind(&orphan_key)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    p.check(
        "[AD6f] create-key on missing role -> 409 (NOT 405), no row",
        orphan_status == 409 && orphan_status != 405 && orphan_rows == Some(0),
        format!("status={orphan_status} rows={orphan_rows:?}"),
    );

    // [AD6g] finding #3: both successful REMOTE submits (role + key) audited by admin-svc's
    // plane; the failed orphan submit (409) emits nothing.
    let audit_after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM asyncevents.events \
         WHERE topic='admin.action' AND payload->>'actor'='proofadmin' \
           AND payload->>'target'='api-keys' AND payload->>'action'='form-submit'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    p.check(
        "[AD6g] remote form-submit audited: >=2 new admin.action{form-submit} rows",
        audit_after >= audit_before + 2,
        format!("before={audit_before} after={audit_after}"),
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

    // --- Wallet ---------------------------------------------------------------------
    // [WL1]/[WL2] are the player-facing reads through gateway-svc (auth = "player", so a
    // real bearer AND the player-facing api key). [WL3]-[WL6] drive the money path on the
    // topology that is actually at risk: gateway-svc (/admin passthrough) -> admin-svc
    // (session + CSRF, renders the REMOTE form fetched over the edge) -> `admin.adminSubmit`
    // -> wallet-svc, which runs `apply_submit` store-local. [WL7] is the durable
    // cross-process grant (accounts-svc emits -> wallet-svc consumes).
    if let Some(tok) = &token {
        // [WL1] the catalog. The dev seed guarantees the CODES `gold`/`gems` exist; their
        // display_name/kind are operator data that permanently drifts once the admin form
        // is used (the seed is insert-if-absent), so nothing here asserts them.
        let wl1 = ctx
            .http
            .get(format!("{g}/wallet/currencies"))
            .header("X-Api-Key", "dev-key-client")
            .header("Authorization", format!("Bearer {tok}"))
            .send()
            .await?;
        let (wl1_code, wl1_body) = (wl1.status().as_u16(), wl1.text().await.unwrap_or_default());
        p.check(
            "[WL1] GET /wallet/currencies -> 200 + seeded gold/gems",
            wl1_code == 200 && wl1_body.contains("\"gold\"") && wl1_body.contains("\"gems\""),
            format!("code={wl1_code}"),
        );

        // [WL2] the caller's OWN balances, keyed by the gateway-verified identity — never a
        // body/query field, so a 200 carrying a JSON array is the whole contract here.
        let wl2 = ctx
            .http
            .get(format!("{g}/wallet/me"))
            .header("X-Api-Key", "dev-key-client")
            .header("Authorization", format!("Bearer {tok}"))
            .send()
            .await?;
        let wl2_code = wl2.status().as_u16();
        let wl2_body: serde_json::Value = wl2.json().await.unwrap_or(serde_json::Value::Null);
        p.check(
            "[WL2] GET /wallet/me (Bearer) -> 200 + balance array",
            wl2_code == 200 && wl2_body.is_array(),
            format!("code={wl2_code} body={wl2_body}"),
        );
    }

    // The admin money assertions use a SYNTHETIC player id (wallet keys balances by uuid and
    // holds no FK to accounts): a registered player would ALSO receive [WL7]'s starter grant,
    // and [WL5]'s event count would then no longer be exactly the grants made here.
    let wl_player: String = sqlx::query_scalar("SELECT gen_random_uuid()::text")
        .fetch_one(pool)
        .await?;

    // [WL3] THE cross-process mutating proof: the grant is applied by wallet-svc, in its own
    // process, from a form rendered and posted by admin-svc.
    let (wl_csrf, wl_idem, _) = wallet_form(&cfg, &g).await?;
    let wl3 = cfg
        .post(format!("{g}/admin/wallet"))
        .form(&[
            ("_csrf", wl_csrf.as_str()),
            ("_idem_grant", wl_idem.as_str()),
            ("_action", "grant"),
            ("player_id", wl_player.as_str()),
            ("currency", "gold"),
            ("amount", "250"),
            ("reason", "splitproof grant"),
        ])
        .send()
        .await?;
    let wl3_code = wl3.status().as_u16();
    let wl3_row = poll_count(
        pool,
        "SELECT count(*) FROM wallet.balances \
          WHERE player_id::text=$1 AND currency='gold' AND amount=250",
        &wl_player,
        1,
    )
    .await;
    p.check(
        "[WL3] remote admin grant -> 303 + wallet.balances 250 (admin-svc -> wallet-svc)",
        wl3_code == 303 && wl3_row,
        format!("status={wl3_code} balance_row={wl3_row} pid={wl_player}"),
    );

    // [WL4] a SECOND grant accumulates — from a SECOND render, because the key is minted per
    // render: replaying the first key with a different amount is the 409 arm, not a movement.
    let (wl_csrf, wl_idem, _) = wallet_form(&cfg, &g).await?;
    let wl4 = cfg
        .post(format!("{g}/admin/wallet"))
        .form(&[
            ("_csrf", wl_csrf.as_str()),
            ("_idem_grant", wl_idem.as_str()),
            ("_action", "grant"),
            ("player_id", wl_player.as_str()),
            ("currency", "gold"),
            ("amount", "150"),
            ("reason", "splitproof top-up"),
        ])
        .send()
        .await?;
    let wl4_code = wl4.status().as_u16();
    let wl4_row = poll_count(
        pool,
        "SELECT count(*) FROM wallet.balances \
          WHERE player_id::text=$1 AND currency='gold' AND amount=400",
        &wl_player,
        1,
    )
    .await;
    p.check(
        "[WL4] second grant with a fresh key -> 303 + balance 400",
        wl4_code == 303 && wl4_row,
        format!("status={wl4_code} balance_row={wl4_row}"),
    );

    // [WL5] the durable side: both movements' `wallet.changed` reach audit's raw sink.
    let wl5 = poll_count(
        pool,
        "SELECT count(*) FROM audit.log WHERE topic='wallet.changed' AND payload->>'player_id'=$1",
        &wl_player,
        2,
    )
    .await;
    p.check("[WL5] wallet.changed reaches audit.log (2 movements)", wl5, "");

    // [WL6] a revoke past the balance. The portal answers 200 WITH THE VERDICT RENDERED —
    // `render_conflict` hard-codes a "reload the page" remedy and drops the domain text, so
    // wallet reserves that arm for an idempotency conflict (where a fresh render IS the
    // remedy) and lets a money verdict ride the error card. The proof is therefore the exact
    // message plus an UNCHANGED balance; a status-code assertion would say nothing here.
    let (wl_csrf, _, wl_idem_revoke) = wallet_form(&cfg, &g).await?;
    let wl6 = cfg
        .post(format!("{g}/admin/wallet"))
        .form(&[
            ("_csrf", wl_csrf.as_str()),
            ("_idem_revoke", wl_idem_revoke.as_str()),
            ("_action", "revoke"),
            ("player_id", wl_player.as_str()),
            ("currency", "gold"),
            ("amount", "999999"),
            ("reason", "splitproof over-revoke"),
        ])
        .send()
        .await?;
    let wl6_code = wl6.status().as_u16();
    let wl6_body = wl6.text().await.unwrap_or_default();
    let wl6_balance: Option<i64> = sqlx::query_scalar(
        "SELECT amount FROM wallet.balances WHERE player_id::text=$1 AND currency='gold'",
    )
    .bind(&wl_player)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    p.check(
        "[WL6] over-revoke -> verdict card 'insufficient funds', balance unchanged at 400",
        wl6_code == 200
            && wl6_body
                .contains("save failed: movement rejected: insufficient funds or balance ceiling exceeded")
            && wl6_balance == Some(400),
        format!("code={wl6_code} balance={wl6_balance:?}"),
    );

    // [WL7] the durable cross-process grant: accounts-svc emits `player.registered`,
    // wallet-svc's subscription consumes it, reads the config knobs seeded pre-spawn and
    // credits inside the DELIVERY transaction. One assertion, three seams.
    let wl7_email = format!("wallet-{suffix}@test.local");
    match register_capture(ctx, &g, &wl7_email).await {
        Ok((wl7_pid, _)) => {
            let credited = poll_count(
                pool,
                "SELECT count(*) FROM wallet.balances \
                  WHERE player_id::text=$1 AND currency='gold' AND amount=100",
                &wl7_pid,
                1,
            )
            .await;
            // The DETERMINISTIC key is what makes a redelivery a no-op, so it is asserted by
            // being the thing looked up.
            let keyed = poll_count(
                pool,
                "SELECT count(*) FROM wallet.ledger WHERE idempotency_key = 'starter:' || $1",
                &wl7_pid,
                1,
            )
            .await;
            p.check(
                "[WL7] a new registration receives the configured starter grant (100 gold)",
                credited && keyed,
                format!("pid={wl7_pid} credited={credited} keyed={keyed}"),
            );
        }
        Err(e) => p.check("[WL7] register a player for the starter grant", false, format!("{e:#}")),
    }

    // --- Notifications inbox ---------------------------------------------------------
    // The module consumes only — nothing it shows is produced in its own process — so the
    // topology at risk is exactly the one this harness boots. [NT1]-[NT3] drive the player
    // ops through gateway-svc and the operator write through the /admin passthrough;
    // [NT4] and [NT6] are the two durable fan-ins, each crossing from a DIFFERENT producer
    // process (wallet-svc, accounts-svc) into notifications-svc.
    //
    // Every inbox assertion is driven by a GUEST: a guest receives no starter grant
    // ([WL8]), so a fresh guest's inbox is empty BY CONSTRUCTION and stays empty. The same
    // assertion on a registered player would merely be racing that player's own grant
    // notification, and would pass for the wrong reason.
    let (nt_guest_code, nt_guest) = create_guest(ctx, &g).await?;

    // [NT1] the read op itself, end to end: gateway-svc verifies the bearer and the
    // player-facing api key, dispatches `notifications.list` Remote over the mTLS edge, and
    // notifications-svc answers its own store. An empty page must be `items: []` WITH an
    // empty `next_cursor` — a non-empty cursor on an empty page is the paging bug that
    // makes a client loop forever.
    let (nt1_code, nt1_items, nt1_cursor) = inbox_page(ctx, &g, &nt_guest.token, "", 0).await?;
    p.check(
        "[NT1] POST /notifications/list (fresh guest) -> 200 + empty page, empty cursor",
        nt_guest_code == 201 && nt1_code == 200 && nt1_items.is_empty() && nt1_cursor.is_empty(),
        format!(
            "guest={nt_guest_code} code={nt1_code} items={} cursor={nt1_cursor:?}",
            nt1_items.len()
        ),
    );

    // [NT2] operator mail over the REMOTE submit path: gateway-svc (/admin passthrough) ->
    // admin-svc (session + CSRF, form rendered from `admin.adminData`) -> `admin.adminSubmit`
    // over QUIC -> notifications-svc, which owns the only insert. The row is asserted in
    // `notifications.messages` because the durable half is the point of the write.
    //
    // The URL is `/admin/inbox`, NOT `/admin/notifications`: the portal routes a page on
    // `slugify(label)`, and `slugify` is private to `modules/admin`, so the module's
    // `ADMIN_SLUG` has NO mechanical pin anywhere in the tree — a live request on the real
    // URL is the only thing that can catch it drifting. The Players row-menu href is
    // checked from the SAME render for the same reason: a self-link built from the item id
    // would 404 and no gate would say so.
    let nt2_title = format!("Operator mail {suffix}");
    let (nt2_csrf, nt2_idem) = inbox_form(&cfg, &g).await?;
    let nt2 = cfg
        .post(format!("{g}/admin/inbox"))
        .form(&[
            ("_csrf", nt2_csrf.as_str()),
            ("_idem_send", nt2_idem.as_str()),
            ("_action", "send-mail"),
            ("player_id", nt_guest.player_id.as_str()),
            ("title", nt2_title.as_str()),
            ("body", "sent by splitproof"),
        ])
        .send()
        .await?;
    let nt2_code = nt2.status().as_u16();
    // The submit is synchronous (the 303 follows the committed write), so this is a direct
    // read, never a poll: polling here would let a write that lands LATE still pass.
    let nt2_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM notifications.messages \
          WHERE player_id = $1::uuid AND kind = 'operator.mail' AND title = $2",
    )
    .bind(&nt_guest.player_id)
    .bind(&nt2_title)
    .fetch_one(pool)
    .await
    .unwrap_or(-1);
    let nt2_players = cfg
        .get(format!("{g}/admin/players"))
        .send()
        .await?
        .text()
        .await
        .unwrap_or_default();
    let nt2_href = nt2_players.contains("View Inbox")
        && nt2_players.contains("href=\"/admin/inbox?player=");
    let nt2_page = cfg
        .get(format!("{g}/admin/inbox?player={}", nt_guest.player_id))
        .send()
        .await?;
    let (nt2_page_code, nt2_page_body) =
        (nt2_page.status().as_u16(), nt2_page.text().await.unwrap_or_default());
    p.check(
        "[NT2] remote operator mail -> 303 + notifications.messages row; /admin/inbox slug resolves",
        nt2_code == 303
            && nt2_rows == 1
            && nt2_href
            && nt2_page_code == 200
            && nt2_page_body.contains(&nt2_title),
        format!(
            "submit={nt2_code} rows={nt2_rows} players_href={nt2_href} \
             drilldown={nt2_page_code} shows_title={}",
            nt2_page_body.contains(&nt2_title)
        ),
    );

    // [NT2b] the REJECTION half of the remote submit, which is different code from the local
    // one: `Rejection::into_local` builds an `adminapi::SubmitError`, `Rejection::into_ops`
    // builds a typed `opsapi::Error` that crosses the edge and is re-classified by admin-svc.
    // Only the split runs the second mapping, and only the status CLASS proves it: the
    // portal turns `Status::NotFound` into 405 "not editable" (its graceful-absent contract
    // for a peer with no write surface), so a rejection that collapsed to `NotFound` would
    // silently degrade this page to read-only while hiding the domain verdict. Resubmitting
    // THIS render's key with an EDITED body is `Sent::KeyReused` -> `Rejection::Stale` ->
    // `Error::conflict`; 409 with the stale-form card is the only answer that is neither the
    // read-only degradation nor a masked success. Run before [NT3] deletes the row: the
    // reused key must find its original message still there to compare against.
    let nt2b = cfg
        .post(format!("{g}/admin/inbox"))
        .form(&[
            ("_csrf", nt2_csrf.as_str()),
            ("_idem_send", nt2_idem.as_str()),
            ("_action", "send-mail"),
            ("player_id", nt_guest.player_id.as_str()),
            ("title", nt2_title.as_str()),
            ("body", "edited after sending"),
        ])
        .send()
        .await?;
    let nt2b_code = nt2b.status().as_u16();
    let nt2b_body = nt2b.text().await.unwrap_or_default();
    let nt2b_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM notifications.messages \
          WHERE player_id = $1::uuid AND kind = 'operator.mail'",
    )
    .bind(&nt_guest.player_id)
    .fetch_one(pool)
    .await
    .unwrap_or(-1);
    p.check(
        "[NT2b] remote resubmit of an edited form -> 409 stale card (not 405 read-only), no second row",
        nt2b_code == 409
            && nt2b_body.contains("This form is stale. Reload the page and try again.")
            && nt2b_rows == 1,
        format!("code={nt2b_code} stale_card={} rows={nt2b_rows}",
            nt2b_body.contains("This form is stale. Reload the page and try again.")),
    );

    // [NT2c] the operator half of the tolerant `::uuid` cast, over the wire. A mistyped
    // player id reaches the insert authority, comes back `22P02` and is mapped to
    // `Rejection::Rejected` -> `Error::invalid`, which admin-svc renders as the page's error
    // card carrying the DOMAIN message — 405 would mean the rejection collapsed to
    // `NotFound`, and a missing message would mean the operator was told nothing actionable.
    // KNOWN GAP: `Status::Invalid` and `Status::Internal` are indistinguishable at this
    // surface — the portal renders both as the same card — so this pins the message and the
    // absence of a row, not the 400-vs-500 split, which nothing observable carries.
    let nt2c_title = format!("Bad player id {suffix}");
    let (nt2c_csrf, nt2c_idem) = inbox_form(&cfg, &g).await?;
    let nt2c = cfg
        .post(format!("{g}/admin/inbox"))
        .form(&[
            ("_csrf", nt2c_csrf.as_str()),
            ("_idem_send", nt2c_idem.as_str()),
            ("_action", "send-mail"),
            ("player_id", "oops"),
            ("title", nt2c_title.as_str()),
            ("body", "should never be stored"),
        ])
        .send()
        .await?;
    let nt2c_code = nt2c.status().as_u16();
    let nt2c_body = nt2c.text().await.unwrap_or_default();
    let nt2c_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM notifications.messages WHERE title = $1")
            .bind(&nt2c_title)
            .fetch_one(pool)
            .await
            .unwrap_or(-1);
    let nt2c_card = nt2c_body.contains("save failed: player_id is not a valid uuid");
    p.check(
        "[NT2c] remote submit with a malformed player_id -> domain error card, no row",
        nt2c_code == 200 && nt2c_card && nt2c_rows == 0,
        format!("code={nt2c_code} card={nt2c_card} rows={nt2c_rows}"),
    );

    // [NT3] the player's own lifecycle on that row: list (it is unread), mark it read, then
    // delete it twice. The SECOND delete is the assertion that matters — `delete` is
    // deliberately NOT `#[retry_safe]`, so a replay after a successful delete must be a 404,
    // and a handler that answered 204 for an absent row would be indistinguishable from a
    // working one without it.
    let (nt3_code, nt3_items, _) = inbox_page(ctx, &g, &nt_guest.token, "", 0).await?;
    let nt3_id = nt3_items
        .first()
        .and_then(|i| i.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let nt3_unread = nt3_items
        .first()
        .and_then(|i| i.get("read_at"))
        .and_then(|v| v.as_str())
        == Some("");
    let (nt3_read, nt3_read_at, nt3_del1, nt3_del2, nt3_gone) = if nt3_id.is_empty() {
        (0, 0, 0, 0, -1)
    } else {
        let read = inbox_status(
            ctx,
            &g,
            &nt_guest.token,
            reqwest::Method::POST,
            &format!("/notifications/{nt3_id}/read"),
        )
        .await?;
        let read_at: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM notifications.messages WHERE id = $1::uuid AND read_at IS NOT NULL",
        )
        .bind(&nt3_id)
        .fetch_one(pool)
        .await
        .unwrap_or(-1);
        let del1 = inbox_status(
            ctx,
            &g,
            &nt_guest.token,
            reqwest::Method::DELETE,
            &format!("/notifications/{nt3_id}"),
        )
        .await?;
        let del2 = inbox_status(
            ctx,
            &g,
            &nt_guest.token,
            reqwest::Method::DELETE,
            &format!("/notifications/{nt3_id}"),
        )
        .await?;
        let gone: i64 =
            sqlx::query_scalar("SELECT count(*) FROM notifications.messages WHERE id = $1::uuid")
                .bind(&nt3_id)
                .fetch_one(pool)
                .await
                .unwrap_or(-1);
        (read, read_at, del1, del2, gone)
    };
    p.check(
        "[NT3] list -> unread row; markRead 204 + read_at set; delete 204 then 404, row gone",
        nt3_code == 200
            && nt3_items.len() == 1
            && nt3_unread
            && nt3_read == 204
            && nt3_read_at == 1
            && nt3_del1 == 204
            && nt3_del2 == 404
            && nt3_gone == 0,
        format!(
            "list={nt3_code} items={} unread={nt3_unread} read={nt3_read} read_at_rows={nt3_read_at} \
             delete={nt3_del1} replay={nt3_del2} remaining={nt3_gone}",
            nt3_items.len()
        ),
    );

    // [NT4] THE cross-process fan-in. An operator grant on the wallet page is applied by
    // wallet-svc, which appends `wallet.changed` to the shared log inside its own
    // transaction; notifications-svc — a different OS process, with its own subscription
    // cursor — pulls it and writes the inbox row in the DELIVERY transaction. Neither
    // process knows the other exists. A synthetic player id keeps this independent of
    // [WL3]/[WL4]'s balances, and the wait is a bounded poll on the ROW, never a sleep.
    let nt4_player: String = sqlx::query_scalar("SELECT gen_random_uuid()::text")
        .fetch_one(pool)
        .await?;
    let (nt4_csrf, nt4_idem, _) = wallet_form(&cfg, &g).await?;
    let nt4 = cfg
        .post(format!("{g}/admin/wallet"))
        .form(&[
            ("_csrf", nt4_csrf.as_str()),
            ("_idem_grant", nt4_idem.as_str()),
            ("_action", "grant"),
            ("player_id", nt4_player.as_str()),
            ("currency", "gold"),
            ("amount", "70"),
            ("reason", "splitproof notification fan-in"),
        ])
        .send()
        .await?;
    let nt4_code = nt4.status().as_u16();
    let nt4_row = poll_count(
        pool,
        "SELECT count(*) FROM notifications.messages \
          WHERE player_id::text = $1 AND kind = 'wallet.credit'",
        &nt4_player,
        1,
    )
    .await;
    p.check(
        "[NT4] wallet-svc grant -> wallet.changed -> notifications-svc inbox row (cross-process)",
        nt4_code == 303 && nt4_row,
        format!("grant={nt4_code} inbox_row={nt4_row} pid={nt4_player}"),
    );

    // [NT5] the keyset walk through the production read path. Rows are SEEDED in SQL (the
    // write paths are [NT2]/[NT4]'s job) at more than DEFAULT_PAGE_LIMIT, in colliding
    // `created_at` groups of three, then walked with a page size that divides neither the
    // total nor the groups. The proof is the exact sequence: equality with the DB's own
    // keyset order catches a duplicate, a dropped row and a re-ordered tie in one compare,
    // and termination is on an EMPTY cursor, with a page cap so a cursor that never
    // exhausts fails loudly instead of hanging.
    const NT5_ROWS: i64 = 30;
    const NT5_LIMIT: i64 = 7;
    const NT5_MAX_PAGES: usize = 12;
    let (nt5_guest_code, nt5_guest) = create_guest(ctx, &g).await?;
    let nt5_seeded = seed_inbox_rows(pool, &nt5_guest.player_id, NT5_ROWS).await?;
    let mut nt5_seen: Vec<String> = Vec::new();
    let mut nt5_cursor = String::new();
    let mut nt5_pages = 0usize;
    let mut nt5_exhausted = false;
    let mut nt5_bad_code: Option<u16> = None;
    while nt5_pages < NT5_MAX_PAGES {
        let (code, items, next) =
            inbox_page(ctx, &g, &nt5_guest.token, &nt5_cursor, NT5_LIMIT).await?;
        nt5_pages += 1;
        if code != 200 {
            nt5_bad_code = Some(code);
            break;
        }
        nt5_seen.extend(
            items
                .iter()
                .filter_map(|i| i.get("id").and_then(|v| v.as_str()).map(str::to_string)),
        );
        if next.is_empty() {
            nt5_exhausted = true;
            break;
        }
        nt5_cursor = next;
    }
    let mut nt5_unique = nt5_seen.clone();
    nt5_unique.sort();
    nt5_unique.dedup();
    p.check(
        "[NT5] cursor walk over 30 rows -> every id exactly once, in keyset order, cursor exhausts",
        nt5_guest_code == 201
            && nt5_bad_code.is_none()
            && nt5_exhausted
            && nt5_seen.len() == NT5_ROWS as usize
            && nt5_unique.len() == nt5_seen.len()
            && nt5_seen == nt5_seeded,
        format!(
            "pages={nt5_pages} exhausted={nt5_exhausted} bad_code={nt5_bad_code:?} \
             seen={} unique={} seeded={} order_matches={}",
            nt5_seen.len(),
            nt5_unique.len(),
            nt5_seeded.len(),
            nt5_seen == nt5_seeded
        ),
    );

    // [NT6] the SECOND fan-in topic, which otherwise ships with no split proof at all. A
    // guest linking its first real identity makes accounts-svc emit durable
    // `player.promoted` inside the link transaction; notifications-svc pulls it on a
    // SEPARATE subscription from [NT4]'s and writes the welcome row. Two topics, two
    // producer processes, one consumer — proving [NT4] alone would leave this half
    // monolith-only by omission.
    let (nt6_guest_code, nt6_guest) = create_guest(ctx, &g).await?;
    let nt6_credential = idp.token(&format!("nt6-epic-{suffix}"))?;
    let (nt6_link, _) =
        link_identity(ctx, &g, &nt6_guest.token, "epic", &nt6_credential).await?;
    let nt6_row = poll_count(
        pool,
        "SELECT count(*) FROM notifications.messages \
          WHERE player_id::text = $1 AND kind = 'account.promoted'",
        &nt6_guest.player_id,
        1,
    )
    .await;
    p.check(
        "[NT6] accounts-svc promotion -> player.promoted -> notifications-svc inbox row",
        nt6_guest_code == 201 && nt6_link == 200 && nt6_row,
        format!("guest={nt6_guest_code} link={nt6_link} inbox_row={nt6_row} pid={}", nt6_guest.player_id),
    );

    // --- Outbound mail ---------------------------------------------------------------
    // The durable ingress is the seam at risk: the producer is anything at all (here, this
    // harness, through the plane's own SQL writer) and the consumer is mail-svc, a process
    // that shares nothing with it. [ML3]/[ML4] add the operator faces, which in the split
    // are `admin.adminData`/`admin.adminSubmit` hops from admin-svc into mail-svc.
    mail_assertions(
        ctx,
        pool,
        p,
        &g,
        &format!("http://127.0.0.1:{}", ctx.http_port("mail-svc")),
        &cfg,
        "",
    )
    .await?;
    mail_prune_assertion(pool, p).await?;

    // --- The push hub. Runs BEFORE the rate-limit burst below: `[PH4]`/`[PH5]` open six
    // sockets through the front door, and `[RL1]`'s deliberate 60-request burst would
    // otherwise still be draining the shared 127.0.0.1 bucket when they upgrade.
    push_assertions(
        ctx,
        pool,
        p,
        &g,
        &cfg,
        Some(
            format!(
                "127.0.0.1:{}",
                ctx.service("gateway-svc").edge_port.context("gateway edge port")?
            )
            .parse()
            .context("gateway edge address")?,
        ),
        "",
    )
    .await?;

    // --- Federated providers, guest promotion and refresh rotation, through gateway-svc
    // (G -> accounts-svc over the mTLS edge; the promotion's durable event crosses to
    // audit-svc and wallet-svc). Re-run verbatim against the monolith below.
    federated_assertions(ctx, pool, &g, idp, p, "").await?;

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
    // Same stale-listener guard as the boot loop: the killed inventory-svc must have
    // actually released its port before the respawn is trusted.
    ensure_no_stale_listener(restarted.name, restarted.http_port)?;
    let mut running = ctx.spawn(&restarted)?;
    ctx.wait_healthy(&restarted, &mut running.child).await?;
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

/// `[RDY-DEAD]` — proves the /readyz amplification fix (commits f5eea2d/1262d51/7f3e631)
/// on the at-risk topology (split, where stubs exist; the monolith holds none). gateway-svc
/// holds a `remote::Stub` per fronted domain provider (see cmd/gateway-svc/src/lib.rs), and
/// each stub's `/readyz` check reads a CACHED reachability verdict stamped by a BACKGROUND
/// probe loop in core/remote — never a request-driven dial. Kill ONE gateway peer
/// (characters-svc — a stub gateway-svc holds, deliberately distinct from the inventory-svc
/// that [I-GATE] restarts) with no respawn, and assert gateway `/readyz` flips to 503
/// NAMING the dead `stub:characters` driven by the probe alone; then respawn and assert
/// /readyz recovers to 200. This scenario proves readiness ACCURACY + RECOVERY end-to-end on
/// the split (gateway-svc's 6 stubs); it makes NO behavioral fixed-vs-unfixed assertion,
/// because a killed peer leaves a CLOSED loopback port whose QUIC dial fast-fails — so even the
/// OLD in-request-dial path would be fast here, and any latency threshold would be unsound
/// (false-pass the unfixed, false-fail the fixed under jitter). The exhaustive by-construction
/// anti-amplification proof is the sequential core/remote `readiness_verdict_*` unit tests.
async fn rdy_dead(ctx: &Ctx, fleet: &mut Vec<Running>, p: &mut Proof) -> Result<()> {
    println!("\n[splitproof] === [RDY-DEAD] kill characters-svc; gateway /readyz must flip via background probe ===");
    let g = format!("http://127.0.0.1:{}", ctx.http_port("gateway-svc"));

    // Baseline: wait for gateway /readyz to be 200 before we kill the peer. This POLLS
    // (not a single shot) on purpose: [I-GATE] just respawned inventory-svc, and the new
    // background probe recovers gateway's `stub:inventory` verdict only ~1-2s after that
    // peer's edge is back (its own PROBE_INTERVAL_UNREADY cadence) — `i_gate`'s wait_healthy
    // gates on inventory's OWN /readyz, not on gateway's stub reflecting it. A single-shot
    // read here could catch a transient stub-recovery 503 and flake. 10s is generous.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut base_ok = false;
    let mut last: Option<u16> = None;
    loop {
        if let Ok(r) = ctx.http.get(format!("{g}/readyz")).send().await {
            last = Some(r.status().as_u16());
            if r.status().is_success() {
                base_ok = true;
                break;
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    p.check(
        "[RDY-DEAD] baseline gateway /readyz 200",
        base_ok,
        format!("last_code={last:?}"),
    );

    let idx = fleet
        .iter()
        .position(|running| running.name == "characters-svc")
        .context("characters-svc missing from fleet (preflight_fleet should have caught this)")?;

    // Guard: characters-svc must be ALIVE before we intentionally kill it. The baseline
    // /readyz==200 poll above already implies this (gateway cannot be 200 with stub:characters
    // down), but assert it explicitly so a spontaneous EARLIER crash can never be silently
    // "healed" by our respawn below and hidden from the later [LV2] liveness sweep.
    let alive_before = matches!(fleet[idx].child.try_wait(), Ok(None));
    p.check(
        "[RDY-DEAD] characters-svc alive before intentional kill",
        alive_before,
        format!("alive={alive_before}"),
    );

    // Kill ONLY characters-svc (Drop kills + waits), NO respawn yet, and let the OS free
    // its HTTP + edge ports so the recovery respawn below rebinds cleanly.
    fleet.remove(idx);
    tokio::time::sleep(Duration::from_millis(800)).await;

    // Poll gateway /readyz until it flips to 503 naming the dead stub. The flip is driven by
    // the stub's BACKGROUND probe loop (core/remote), NOT by these requests — each GET just
    // OBSERVES the cached verdict. Budget must exceed core/remote PROBE_INTERVAL_READY (5s) +
    // probe_peer dial timeout (1s); bump if those grow — this asserts the flip, not the exact
    // interval, so it fails LOUD on drift rather than silently.
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut got503 = false;
    let mut names_stub = false;
    let mut last: Option<u16> = None;
    loop {
        if let Ok(r) = ctx.http.get(format!("{g}/readyz")).send().await {
            last = Some(r.status().as_u16());
            if r.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
                got503 = true;
                // The 503 body maps each failed check_name -> error string (httpmw readiness):
                // assert it is characters' stub that flipped, not some other check.
                let body = r.text().await.unwrap_or_default();
                names_stub = body.contains("stub:characters");
                if names_stub {
                    break;
                }
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    p.check(
        "[RDY-DEAD] dead peer flips gateway /readyz to 503 (background probe)",
        got503 && names_stub,
        format!("last_code={last:?} names_stub={names_stub}"),
    );

    // NOTE: [RDY-DEAD] makes NO behavioral fixed-vs-unfixed assertion on purpose. A killed svc
    // leaves a CLOSED loopback port, and a QUIC dial to it fast-fails (see
    // `probe_unreachable_peer_errs_fast` in core/remote) rather than consuming the 1s timeout —
    // so even the OLD in-request-dial /readyz would return quickly here. A latency threshold
    // would therefore be unsound in BOTH directions (false-pass the unfixed path, false-fail the
    // fixed one under CI jitter). The exhaustive anti-amplification proof is the SEQUENTIAL
    // core/remote `readiness_verdict_*` unit tests (zero-I/O by construction); this split
    // scenario proves readiness ACCURACY + RECOVERY end-to-end across gateway-svc's 6 stubs.

    // Recovery: respawn characters-svc from its canonical spec and wait for it healthy.
    let restarted = ctx.service("characters-svc").clone();
    ensure_no_stale_listener(restarted.name, restarted.http_port)?;
    let mut running = ctx.spawn(&restarted)?;
    ctx.wait_healthy(&restarted, &mut running.child).await?;
    fleet.insert(idx, running);
    println!("[splitproof] characters-svc respawned; waiting for gateway /readyz to recover ...");

    // Poll until gateway /readyz recovers to 200. The stub loop is in its 1s-unready cadence
    // (core/remote PROBE_INTERVAL_UNREADY), so it re-probes and re-stamps ready within ~1-2s
    // of the peer's edge coming back — 10s is generous headroom against CI jitter.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut recovered = false;
    let mut last: Option<u16> = None;
    loop {
        if let Ok(r) = ctx.http.get(format!("{g}/readyz")).send().await {
            last = Some(r.status().as_u16());
            if r.status().is_success() {
                recovered = true;
                break;
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    p.check(
        "[RDY-DEAD] gateway /readyz recovers to 200 after peer returns",
        recovered,
        format!("last_code={last:?}"),
    );
    Ok(())
}

/// `[REPLICAS]` — an END-TO-END BELT for durable-plane replica safety on the at-risk
/// topology (split, TWO real processes of one module). It is NOT the primary
/// contention/lock proof: the deterministic by-construction proof already exists in
/// `core/asyncevents/src/worker_tests.rs:317`
/// (`skip_locked_single_owner_and_failover_from_checkpoint` — worker A holds the
/// subscription row via `pg_sleep`, worker B's `deliver_one` is asserted `== Step::Skipped`;
/// remove the `FOR UPDATE SKIP LOCKED` and B delivers → that unit test fails
/// deterministically). This split scenario BELTS that authority end-to-end, proving two
/// things over two REAL processes sharing `leaderboard.match-finished.v1`:
///
/// - **(a) exactly-once aggregate.** A SECOND leaderboard-svc boots against the SAME Postgres
///   and a batch of N `match.finished` events is driven concurrently (N `POST /match/report`,
///   SAME winner, distinct `ReportId`s). leaderboard's durable effect is a DB-observable
///   `wins+1` upsert per delivered event, so `leaderboard.scores.wins == N` (never overshoots)
///   is a falsifiable aggregate: a plane that double-delivered would reach 2N and FAIL it.
///   (Honest caveat: `/readyz` is seeded green at plane start BEFORE any worker polls —
///   `core/asyncevents/src/lib.rs:332` `mark_pass_ok()` runs before the worker tasks spawn at
///   `:333` — so "both /readyz 200" certifies HTTP + plane-started, NOT that both durable
///   workers are already polling. Contention here is timing-emergent, not forced; the FORCED,
///   deterministic contention proof is the cited unit test, not this belt.)
/// - **(b) `#2` is a genuine failover-capable delivering participant** — a DETERMINISTIC
///   witness (no scheduling-outcome flake). After (a) settles at N, KILL instance `#1` (the
///   base leaderboard-svc), then drive M MORE events with the same winner: only `#2` remains
///   alive, so `wins` MUST climb to N+M. A decoy `#2` that never delivered would leave `wins`
///   stuck at N (the base's own two workers can't run once `#1` is dead) → the assertion FAILS.
///   This pins that the second REAL process actually consumes the shared subscription, which
///   (a) alone cannot show (the base's `WORKERS = 2` could deliver all N by itself).
///
/// Deliberately NOT asserted: "both instances each deliver ≥1" — a correct system may have one
/// process win every race, so asserting a scheduling outcome would be flaky (timing-sensitive-
/// tests doctrine). The failover witness is the deterministic substitute.
///
/// The second `ServiceSpec` is built INLINE from the canonical spec (like `i_gate` builds its
/// respawn spec) and is NEVER added to the centralized processctl fleet — that would trip the
/// fleet-drift preflight; two runtime leaderboard processes sharing one subscription id are
/// replicas (same module, one host) and do NOT trip topiccheck either.
async fn replicas_exactly_once(
    ctx: &Ctx,
    pool: &PgPool,
    fleet: &mut Vec<Running>,
    p: &mut Proof,
) -> Result<()> {
    println!("\n[splitproof] === [REPLICAS] second leaderboard-svc; exactly-once + failover witness ===");
    const N: u32 = 20;
    const M: u32 = 10;
    // Distinct bind ports for the second instance — collide with nothing in the fleet
    // (http 8080-8094, edge 9000-9013, player 9100). Same executable + same DATABASE_URL as
    // the base instance, so both run a durable worker holding the SAME subscription id; only
    // the bind ports differ. Its Postgres sessions are charged to the budget as
    // `processctl`'s `SPLITPROOF_REPLICA_SESSIONS` — the fleet model would otherwise miss
    // this 15th DB-backed process entirely.
    const REPLICA_HTTP: u16 = 8190;
    const REPLICA_EDGE: u16 = 9108;

    let mut replica = ctx.service("leaderboard-svc").clone();
    replica.name = "leaderboard-svc#2";
    replica.http_port = REPLICA_HTTP;
    replica.edge_port = Some(REPLICA_EDGE);
    replica.env.insert("PORT".into(), format!(":{REPLICA_HTTP}"));
    replica.env.insert("EDGE_ADDR".into(), format!(":{REPLICA_EDGE}"));

    // Base leaderboard-svc is already up in `fleet`. Bring the SECOND instance up and wait for
    // its OWN /readyz — both processes then serve (HTTP + plane started; see the caveat above,
    // this does NOT certify both durable workers are polling).
    ensure_no_stale_listener(replica.name, replica.http_port)?;
    let mut replica_running = ctx.spawn(&replica)?;
    ctx.wait_healthy(&replica, &mut replica_running.child).await?;
    println!("[splitproof] second leaderboard-svc healthy on :{REPLICA_HTTP}");

    // [REPLICAS-1] BOTH instances SERVE (both /readyz 200) before the batch.
    let base_url = format!("http://127.0.0.1:{}/readyz", ctx.http_port("leaderboard-svc"));
    let repl_url = format!("http://127.0.0.1:{REPLICA_HTTP}/readyz");
    let base_rdy = ctx.http.get(&base_url).send().await.map(|r| r.status().is_success()).unwrap_or(false);
    let repl_rdy = ctx.http.get(&repl_url).send().await.map(|r| r.status().is_success()).unwrap_or(false);
    p.check(
        "[REPLICAS-1] both leaderboard instances serve /readyz 200 before batch",
        base_rdy && repl_rdy,
        format!("base={base_rdy} replica={repl_rdy}"),
    );

    let winner = harness_player(&format!("replicas-{}", std::process::id()));
    let loser = harness_player(&format!("replicas-loser-{}", std::process::id()));
    let g = format!("http://127.0.0.1:{}", ctx.http_port("gateway-svc"));

    // Phase (a): enqueue the batch of N events (concurrently, so a backlog piles into the
    // shared log) with both processes up, then DB-assert the exactly-once aggregate.
    let accepted = drive_reports(ctx, &g, &winner, &loser, &format!("replicas-{}", std::process::id()), N).await;
    p.check(
        "[REPLICAS-2] all N match reports accepted (batch enqueued, both instances up)",
        accepted == N,
        format!("accepted={accepted}/{N}"),
    );
    let (final_wins, max_seen) = settle_wins(pool, &winner, N as i64).await;
    p.check(
        "[REPLICAS-3] two-process durable delivery is exactly-once: wins == N, never 2N",
        final_wins == N as i64 && max_seen == N as i64,
        format!("final_wins={final_wins} max_seen={max_seen} N={N} (a double-apply would reach {})", 2 * N),
    );

    // Phase (b): FAILOVER witness (deterministic). Kill instance #1 (the base leaderboard-svc
    // in `fleet`), then drive M MORE events. With #1 dead, ONLY #2 can deliver — so wins MUST
    // climb to N+M. A decoy #2 leaves wins stuck at N. Mirrors the rdy_dead kill pattern.
    let base_idx = fleet
        .iter()
        .position(|running| running.name == "leaderboard-svc")
        .context("leaderboard-svc missing from fleet (preflight_fleet should have caught this)")?;
    println!("[splitproof] killing base leaderboard-svc (#1); only #2 (:{REPLICA_HTTP}) can deliver now");
    fleet.remove(base_idx); // Drop kills + waits.
    tokio::time::sleep(Duration::from_millis(800)).await; // let the OS free #1's ports.

    let fo_accepted = drive_reports(ctx, &g, &winner, &loser, &format!("replicas-fo-{}", std::process::id()), M).await;
    p.check(
        "[REPLICAS-4a] M failover reports accepted (base #1 dead; match-svc still routes)",
        fo_accepted == M,
        format!("accepted={fo_accepted}/{M}"),
    );
    let target = (N + M) as i64;
    let (fo_final, fo_max) = settle_wins(pool, &winner, target).await;
    p.check(
        "[REPLICAS-4b] #2 delivers after #1 dies: wins climbs to N+M (decoy #2 => stuck at N)",
        fo_final == target && fo_max == target,
        format!("final_wins={fo_final} max_seen={fo_max} target={target} (a dead/decoy #2 leaves {N})"),
    );

    // Restore the fleet: respawn base leaderboard-svc from its canonical spec and re-insert it
    // so [LV2]'s liveness sweep sees a complete fleet. THEN tear the scenario-local #2 down
    // (kill-on-drop). Order matters only for tidiness — both ports are distinct.
    let restarted = ctx.service("leaderboard-svc").clone();
    ensure_no_stale_listener(restarted.name, restarted.http_port)?;
    let mut running = ctx.spawn(&restarted)?;
    ctx.wait_healthy(&restarted, &mut running.child).await?;
    fleet.insert(base_idx, running);
    println!("[splitproof] base leaderboard-svc restored");
    drop(replica_running);
    Ok(())
}

/// Fire `count` concurrent match reports (SAME winner, distinct `ReportId`s from `id_prefix`),
/// each retrying past a transient gateway 429 (mirrors `report`). Returns the count accepted
/// (202). A concurrent burst piles a real backlog into the shared event log.
async fn drive_reports(ctx: &Ctx, g: &str, winner: &str, loser: &str, id_prefix: &str, count: u32) -> u32 {
    assert_harness_player(winner);
    assert_harness_player(loser);
    let mut handles = Vec::new();
    for i in 0..count {
        let http = ctx.http.clone();
        let g = g.to_string();
        let winner = winner.to_string();
        let loser = loser.to_string();
        let rid = format!("{id_prefix}-{i}");
        handles.push(tokio::spawn(async move {
            for _ in 0..15 {
                let code = match http
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
        }));
    }
    let mut accepted = 0u32;
    for h in handles {
        if let Ok(202) = h.await {
            accepted += 1;
        }
    }
    accepted
}

/// Bounded settle WITHOUT racing the clock: poll `leaderboard.scores.wins` for `player` until
/// it reaches `target`, THEN hold a settle window so a would-be OVERSHOOT (a double-apply) has
/// time to manifest, tracking the max ever observed. Returns `(final_wins, max_seen)`. Callers
/// assert `final_wins == target && max_seen == target` — the equality catches under-delivery,
/// the max catches over-delivery.
async fn settle_wins(pool: &PgPool, player: &str, target: i64) -> (i64, i64) {
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut max_seen: i64 = 0;
    let mut reached_at: Option<Instant> = None;
    loop {
        let wins = leaderboard_wins(pool, player).await;
        max_seen = max_seen.max(wins);
        if wins >= target {
            let since = *reached_at.get_or_insert_with(Instant::now);
            if since.elapsed() >= Duration::from_secs(5) {
                break;
            }
        } else {
            reached_at = None;
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    let final_wins = leaderboard_wins(pool, player).await;
    (final_wins, max_seen.max(final_wins))
}

/// Reads `leaderboard.scores.wins` for one player (0 if absent) — the DB-observable
/// durable effect the `[REPLICAS]` belt asserts an exact aggregate over.
async fn leaderboard_wins(pool: &PgPool, player: &str) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT wins FROM leaderboard.scores WHERE player=$1")
        .bind(player)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or(0)
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

/// Register + login a player, returning `(player_id, bearer)`. [WL7] must DB-assert a
/// grant keyed by the player id, which `register_login` (token only) cannot supply.
/// Retries past the gateway's always-on 429 exactly as `register_login` does.
async fn register_capture(ctx: &Ctx, base: &str, email: &str) -> Result<(String, String)> {
    let mut player_id: Option<String> = None;
    for _ in 0..15 {
        let reg = ctx
            .http
            .post(format!("{base}/accounts/register"))
            .header("X-Api-Key", "dev-key-client")
            .json(&serde_json::json!({"email": email, "password": "pw", "displayName": "W"}))
            .send()
            .await?;
        if reg.status().as_u16() == 429 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }
        let body: serde_json::Value = reg.json().await.unwrap_or(serde_json::Value::Null);
        player_id = body.get("player_id").and_then(|v| v.as_str()).map(str::to_string);
        break;
    }
    let player_id = player_id.context("no player_id from register")?;
    for _ in 0..15 {
        let login = ctx
            .http
            .post(format!("{base}/accounts/login"))
            .header("X-Api-Key", "dev-key-client")
            .json(&serde_json::json!({"email": email, "password": "pw"}))
            .send()
            .await?;
        if login.status().as_u16() == 429 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }
        let body: serde_json::Value = login.json().await.unwrap_or(serde_json::Value::Null);
        let token = body
            .get("token")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .context("no token from login")?;
        return Ok((player_id, token));
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

/// Like `create_character` but returns the final HTTP status (retrying only past the
/// always-on 429), so a caller can assert the cap-rejection 409.
async fn create_character_status(ctx: &Ctx, g: &str, token: &str, name: &str) -> u16 {
    for _ in 0..15 {
        let r = ctx.http.post(format!("{g}/characters"))
            .header("X-Api-Key", "dev-key-client")
            .header("Authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({"name": name, "class": "mage"}))
            .send().await;
        match r {
            Ok(resp) => {
                let code = resp.status().as_u16();
                if code == 429 { tokio::time::sleep(Duration::from_millis(300)).await; continue; }
                return code;
            }
            Err(_) => return 0,
        }
    }
    429
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
/// One `create_guest` answer, kept whole because [A7], [A8], [WL8] and [A9] each need a
/// different field of it.
struct Guest {
    player_id: String,
    token: String,
    refresh_token: String,
    device_secret: String,
}

/// POST json through a front door, retrying only past the always-on 429 (the same bound
/// `register_capture` uses), and answer `(status, body)`. The body is `Null` when the
/// response is not json, so a caller asserts a status without unwrapping.
async fn post_json(
    ctx: &Ctx,
    url: &str,
    body: serde_json::Value,
) -> Result<(u16, serde_json::Value)> {
    for _ in 0..15 {
        let r = ctx
            .http
            .post(url)
            .header("X-Api-Key", "dev-key-client")
            .json(&body)
            .send()
            .await?;
        let code = r.status().as_u16();
        if code == 429 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }
        let parsed: serde_json::Value = r.json().await.unwrap_or(serde_json::Value::Null);
        return Ok((code, parsed));
    }
    bail!("{url} rate-limited out")
}

/// The status `login_federated` answers for one `(provider, credential)` pair — the
/// three-way provider resolution ([A6]) reduced to the number a client sees.
async fn federated_status(ctx: &Ctx, base: &str, provider: &str, credential: &str) -> Result<u16> {
    let (code, _) = post_json(
        ctx,
        &format!("{base}/accounts/login/federated"),
        serde_json::json!({"provider": provider, "credential": credential}),
    )
    .await?;
    Ok(code)
}

async fn create_guest(ctx: &Ctx, base: &str) -> Result<(u16, Guest)> {
    let (code, body) = post_json(ctx, &format!("{base}/accounts/guest"), serde_json::json!({})).await?;
    let pick = |k: &str| body.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    Ok((
        code,
        Guest {
            player_id: pick("player_id"),
            token: pick("token"),
            refresh_token: pick("refresh_token"),
            device_secret: pick("device_secret"),
        },
    ))
}

/// Attach a verified identity to the CALLING player through the Step 9 `link` op — the
/// only production link path this harness can drive (the Epic web flow is a browser
/// redirect nobody here follows). Bearer + player api key, `register_capture`'s 429 retry.
async fn link_identity(
    ctx: &Ctx,
    base: &str,
    bearer: &str,
    provider: &str,
    credential: &str,
) -> Result<(u16, serde_json::Value)> {
    for _ in 0..15 {
        let r = ctx
            .http
            .post(format!("{base}/accounts/link"))
            .header("X-Api-Key", "dev-key-client")
            .header("Authorization", format!("Bearer {bearer}"))
            .json(&serde_json::json!({"provider": provider, "credential": credential}))
            .send()
            .await?;
        let code = r.status().as_u16();
        if code == 429 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }
        let body: serde_json::Value = r.json().await.unwrap_or(serde_json::Value::Null);
        return Ok((code, body));
    }
    bail!("accounts.link rate-limited out")
}

/// Recursively finds the first string-valued `name` field — the player envelope nests the
/// op's answer under a status wrapper (`find_id`'s shape, generalized to any key).
fn find_str(v: &serde_json::Value, name: &str) -> Option<String> {
    match v {
        serde_json::Value::Object(m) => {
            if let Some(found) = m.get(name).and_then(|x| x.as_str()) {
                return Some(found.to_string());
            }
            m.values().find_map(|child| find_str(child, name))
        }
        serde_json::Value::Array(a) => a.iter().find_map(|child| find_str(child, name)),
        _ => None,
    }
}

/// Runs `calls` over ONE player-QUIC connection. [P8] spends five calls, and dialling per
/// call would charge the per-IP CONNECTION bucket (burst 20) that [P1]-[P7] already drew
/// on; one connection charges the request bucket only.
async fn player_session(ctx: &Ctx, calls: &[(&str, String)]) -> Result<Vec<serde_json::Value>> {
    let ca = ctx.ca_cert.to_str().context("CA cert path not UTF-8")?;
    let trust = DevCA::load_cert_only(ca).map_err(|e| anyhow::anyhow!("load CA: {e}"))?;
    let addr = format!("127.0.0.1:{}", ctx.player_port()).parse().context("player addr")?;
    let client = PlayerClient::dial(addr, &trust)
        .await
        .map_err(|e| anyhow::anyhow!("dial: {e}"))?;
    let mut out = Vec::new();
    for (method, payload) in calls {
        let mut answer = serde_json::Value::Null;
        for _ in 0..10 {
            match client.call(method, None, Some("dev-key-client"), payload.as_bytes()).await {
                Ok(resp) => {
                    answer = serde_json::from_slice(&resp).unwrap_or(serde_json::Value::Null);
                    break;
                }
                Err(e) if e.to_string().contains("rate limit") => {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
                Err(e) => bail!("player call {method}: {e}"),
            }
        }
        out.push(answer);
    }
    Ok(out)
}

fn envelope_status(v: &serde_json::Value) -> Option<&str> {
    v.get("status").and_then(|s| s.as_str())
}

/// The federated-provider, guest, promotion and refresh proofs, driven through whatever
/// front `base` names so the split (gateway-svc → accounts-svc over the mTLS edge) and the
/// monolith (all Local) execute the IDENTICAL assertions. `m` is the parity suffix — `""`
/// in the split, `"m"` in the monolith — and is the only difference between the two runs.
async fn federated_assertions(
    ctx: &Ctx,
    pool: &PgPool,
    base: &str,
    idp: &Idp,
    p: &mut Proof,
    m: &str,
) -> Result<()> {
    let suffix = format!("{}{m}", std::process::id());

    // [A6] the three-way provider resolution, as a client sees it. `google` is in
    // KNOWN_PROVIDERS and this fleet deliberately leaves it unconfigured (the Proof
    // overlay clears GOOGLE_* precisely so this arm exists), so it must answer 503, not
    // the 400 an unbuildable name earns.
    let a6_unconfigured = federated_status(ctx, base, "google", "irrelevant").await?;
    let a6_unknown = federated_status(ctx, base, "nope", "irrelevant").await?;
    p.check(
        &format!("[A6{m}] login_federated google -> 503 (known, unconfigured), nope -> 400"),
        a6_unconfigured == 503 && a6_unknown == 400,
        format!("google={a6_unconfigured} nope={a6_unknown}"),
    );

    // [A7] the guest device: minted server-side, its ticket revealed once and replayable
    // as a credential under the `guest` provider.
    let (guest_code, guest) = create_guest(ctx, base).await?;
    let a7_login = federated_status(ctx, base, "guest", &guest.device_secret).await?;
    p.check(
        &format!("[A7{m}] create_guest -> 201 + device secret; login_federated guest -> 200"),
        guest_code == 201
            && !guest.player_id.is_empty()
            && !guest.device_secret.is_empty()
            && !guest.refresh_token.is_empty()
            && a7_login == 200,
        format!("create={guest_code} pid={} login={a7_login}", guest.player_id),
    );

    // [WL8] first half. The guest's `player.registered` must reach wallet's subscription
    // and credit NOTHING — a negative only worth asserting once the event has actually
    // been consumed, so the wait is on that subscription's cursor passing the event, never
    // on a sleep.
    let registered_consumed = poll_count(
        pool,
        "SELECT count(*) FROM asyncevents.subscriptions s, asyncevents.events e \
          WHERE s.subscription_id='wallet.player-registered.v1' \
            AND e.topic='player.registered' AND e.payload->>'player_id'=$1 \
            AND (s.cursor_generation, s.cursor_xid, s.cursor_tie) \
                >= (e.generation, e.producer_xid, e.tie_breaker)",
        &guest.player_id,
        1,
    )
    .await;
    let ledger_before: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM wallet.ledger WHERE idempotency_key = 'starter:' || $1",
    )
    .bind(&guest.player_id)
    .fetch_one(pool)
    .await?;

    // [A8] the promotion. A guest gaining its FIRST non-guest identity emits durable
    // `player.promoted` inside the link transaction; audit's raw sink records it from the
    // shared log — in the split, written by accounts-svc and read by audit-svc.
    let a8_credential = idp.token(&format!("epic-{suffix}"))?;
    let (link_code, _) = link_identity(ctx, base, &guest.token, "epic", &a8_credential).await?;
    let promoted = poll_count(
        pool,
        "SELECT count(*) FROM audit.log WHERE topic='player.promoted' \
           AND payload->>'player_id'=$1",
        &guest.player_id,
        1,
    )
    .await;
    p.check(
        &format!("[A8{m}] guest links a real identity -> player.promoted reaches audit.log"),
        link_code == 200 && promoted,
        format!("link={link_code} pid={} promoted={promoted}", guest.player_id),
    );

    // [WL8] second half: the promotion — not the registration — is what grants, and the
    // deterministic `starter:<player_id>` key means the count is exactly one even though
    // BOTH subscriptions have now seen this player.
    let granted = poll_count(
        pool,
        "SELECT count(*) FROM wallet.ledger WHERE idempotency_key = 'starter:' || $1",
        &guest.player_id,
        1,
    )
    .await;
    p.check(
        &format!("[WL8{m}] guest registration credits nothing; promotion credits exactly one starter row"),
        registered_consumed && ledger_before == 0 && granted,
        format!(
            "consumed={registered_consumed} before={ledger_before} after_promotion_is_one={granted}"
        ),
    );

    // [A9] refresh rotation, the grace window and the family kill, on a player of its own
    // so no other login shares its family. The out-of-grace replay is produced by AGEING
    // the consumed row in SQL — the window is 30s and a harness that slept through it
    // would be asserting the clock, not the branch.
    let (_, a9_reg) = post_json(
        ctx,
        &format!("{base}/accounts/register"),
        serde_json::json!({
            "email": format!("refresh-{suffix}@test.local"),
            "password": "pw",
            "displayName": "R"
        }),
    )
    .await?;
    let a9_pid = a9_reg.get("player_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let r0 = a9_reg.get("refresh_token").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let refresh_url = format!("{base}/accounts/refresh");

    let (c1, s1) = post_json(ctx, &refresh_url, serde_json::json!({"refresh_token": r0})).await?;
    let r1 = s1.get("refresh_token").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let t1 = s1.get("token").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let rotated = c1 == 200 && !r1.is_empty() && r1 != r0;

    // The positive control that isolates the ONE variable [A9] changes: the same consumed
    // token, replayed IN the window, is the lost-response case — 200 with the recorded
    // successor handed back and nothing revoked.
    let (c2, s2) = post_json(ctx, &refresh_url, serde_json::json!({"refresh_token": r0})).await?;
    let grace = c2 == 200 && s2.get("refresh_token").and_then(|v| v.as_str()) == Some(r1.as_str());

    sqlx::query("UPDATE accounts.refresh_tokens SET used_at = now() - interval '10 minutes' WHERE token = $1")
        .bind(&r0)
        .execute(pool)
        .await?;

    let (c3, _) = post_json(ctx, &refresh_url, serde_json::json!({"refresh_token": r0})).await?;
    // The family kill commits on the 401 path, so it is observable from OUTSIDE: the
    // successor no longer refreshes, the access token minted at rotation no longer
    // authenticates, and the family's rows are gone.
    let (c4, _) = post_json(ctx, &refresh_url, serde_json::json!({"refresh_token": r1})).await?;
    let me_code = ctx
        .http
        .get(format!("{base}/accounts/me"))
        .header("X-Api-Key", "dev-key-client")
        .header("Authorization", format!("Bearer {t1}"))
        .send()
        .await?
        .status()
        .as_u16();
    let surviving: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM accounts.refresh_tokens WHERE player_id::text = $1",
    )
    .bind(&a9_pid)
    .fetch_one(pool)
    .await?;
    p.check(
        &format!("[A9{m}] refresh rotates; in-grace replay 200; late replay -> 401 + family dead"),
        rotated && grace && c3 == 401 && c4 == 401 && me_code == 401 && surviving == 0,
        format!(
            "rotate={c1} grace={c2}/{grace} late={c3} successor={c4} me={me_code} \
             surviving_refresh_rows={surviving}"
        ),
    );

    // [P8] the same auth ops on the player-QUIC plane. All four are `auth = "none"`, so
    // they are reachable WITHOUT a bearer — the plane's api-key gate still applies — and
    // the provider resolution renders as the envelope statuses the HTTP front maps to
    // 503/400.
    let p8_guest_call = ("accounts.createGuest", "{}".to_string());
    let p8 = player_session(ctx, &[p8_guest_call]).await?;
    let p8_secret = p8.first().and_then(|v| find_str(v, "device_secret")).unwrap_or_default();
    let p8_refresh = p8.first().and_then(|v| find_str(v, "refresh_token")).unwrap_or_default();
    let p8_rest = player_session(
        ctx,
        &[
            (
                "accounts.loginFederated",
                serde_json::json!({"provider": "guest", "credential": p8_secret}).to_string(),
            ),
            (
                "accounts.loginFederated",
                serde_json::json!({"provider": "google", "credential": "irrelevant"}).to_string(),
            ),
            (
                "accounts.loginFederated",
                serde_json::json!({"provider": "nope", "credential": "irrelevant"}).to_string(),
            ),
            (
                "accounts.refresh",
                serde_json::json!({"refresh_token": p8_refresh}).to_string(),
            ),
        ],
    )
    .await?;
    let statuses: Vec<Option<&str>> = p8
        .iter()
        .chain(p8_rest.iter())
        .map(envelope_status)
        .collect();
    p.check(
        &format!("[P8{m}] QUIC createGuest/loginFederated/refresh -> Ok, Ok, Unavailable, Invalid, Ok"),
        !p8_secret.is_empty()
            && statuses
                == vec![
                    Some("Ok"),
                    Some("Ok"),
                    Some("Unavailable"),
                    Some("Invalid"),
                    Some("Ok"),
                ],
        format!("secret={} statuses={statuses:?}", !p8_secret.is_empty()),
    );

    Ok(())
}

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
    assert_harness_player(winner);
    assert_harness_player(loser);
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

/// Send one built request, retrying only the gateway's always-on rate-limit 429 (20rps/burst
/// 40) so a single-shot status assertion (D4-404/D4-ILLTYPED) reads the ROUTING outcome, not an
/// incidental rate-limit collision. `RequestBuilder` is not `Clone`-cheap across a body, so this
/// takes the builder once and re-clones it (the bodies here are tiny JSON, cheaply `try_clone`d);
/// a non-clonable body would just send once. Returns `0` on a transport error.
async fn send_status_retrying_429(req: reqwest::RequestBuilder) -> u16 {
    for _ in 0..15 {
        let attempt = match req.try_clone() {
            Some(cloned) => cloned,
            None => return req.send().await.map(|r| r.status().as_u16()).unwrap_or(0),
        };
        let code = attempt.send().await.map(|r| r.status().as_u16()).unwrap_or(0);
        if code == 429 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            continue;
        }
        return code;
    }
    429
}

/// Poll `leaderboard.scores` until `winner`'s row holds exactly `wins`. Reads the ROW, not
/// the `GET /leaderboard` top-100 projection: that list is truncated (`LIMIT 100`) and holds
/// rows this harness does not own, so a freshly reported champion at wins=1 can be a real, correct row
/// that is simply outside the served page. Bounded and fail-closed — a value that never lands
/// returns `false` rather than waiting.
async fn poll_leaderboard_wins(pool: &PgPool, winner: &str, wins: i64) -> bool {
    poll_count(pool, "SELECT wins FROM leaderboard.scores WHERE player=$1", winner, wins).await
}

/// `GET /leaderboard` through G, retrying only the always-on rate-limit 429. Returns the
/// status and body.
async fn leaderboard_top(ctx: &Ctx, g: &str) -> (u16, String) {
    for _ in 0..15 {
        if let Ok(r) = ctx
            .http
            .get(format!("{g}/leaderboard"))
            .header("X-Api-Key", "dev-key-client")
            .send()
            .await
        {
            let code = r.status().as_u16();
            if code == 429 {
                tokio::time::sleep(Duration::from_millis(300)).await;
                continue;
            }
            return (code, r.text().await.unwrap_or_default());
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    (429, String::new())
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

/// [P6] one persistent player connection depletes the per-connection bucket (burst 20)
/// by CONCURRENCY, not call speed: it fans out 60 simultaneous calls on the one
/// connection, so the limiter is observable under any realistic load (the old
/// sequential 22-call version only ever saw a denial if avg call latency stayed
/// <5ms — green on an idle box, red under load; a 60-wide burst would need the
/// server to spread admission over >4s of refill to dodge every denial). At least
/// one call must be rate-limited. Then, after a refill pause, a single sequential
/// call must succeed again — proving the limiter is per-connection and transient,
/// not sticky. Same fan-out idiom as `burst_429` / [AD2b]/[AD2c].
async fn player_burst(ctx: &Ctx) -> bool {
    let Some(ca) = ctx.ca_cert.to_str() else { return false };
    let Ok(trust) = DevCA::load_cert_only(ca) else { return false };
    let Ok(addr) = format!("127.0.0.1:{}", ctx.player_port()).parse() else { return false };
    let Ok(client) = PlayerClient::dial(addr, &trust).await else { return false };
    let client = std::sync::Arc::new(client);

    // Fan out 60 concurrent calls on the ONE connection (burst 20, refill 10 rps).
    // For ALL of them to pass, the server would have to spread admission over >4s
    // of refill — margin against load, not a guarantee (a wall-clock-free proof
    // would need a clock seam in the limiter); 60 vs the old 25 turns "the server
    // stalls >500ms and the denial vanishes" into "the server stalls >4s".
    let mut hs = Vec::new();
    for _ in 0..60 {
        let client = client.clone();
        hs.push(tokio::spawn(async move {
            client.call("leaderboard.topScores", None, Some("dev-key-client"), b"{}").await
        }));
    }
    let mut limited = false;
    for h in hs {
        if let Ok(Err(e)) = h.await {
            if e.to_string().contains("rate limit") {
                limited = true;
            }
        }
    }

    // Refill pause, then ONE sequential call must succeed again — the bucket refilled,
    // so the earlier denials were transient, not a sticky per-connection ban.
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let refilled = match client
        .call("leaderboard.topScores", None, Some("dev-key-client"), b"{}")
        .await
    {
        Ok(resp) => serde_json::from_slice::<serde_json::Value>(&resp)
            .ok()
            .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(|s| s == "Ok"))
            .unwrap_or(false),
        Err(_) => false,
    };

    limited && refilled
}
