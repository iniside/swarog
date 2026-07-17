//! `weles-managed-gateway` — a BLOCKING verify stage that BOOTS WELES on the
//! real fleet and proves `gateway-svc`, in managed mode, actually resolved its
//! peers from the agent and works.
//!
//! # Why this exists at all (it is not a nicer `weles-wire-contract`)
//!
//! Zero-sharing means the agent's contract is written down TWICE, by hand:
//! `weles::agentapi` serves it, `remote::resolve` speaks it, and neither crate
//! may see the other (`core/remote` is in the shipping graph, which may not even
//! dev-depend on weles). So each side is tested against its own fake, and
//! **nothing in either crate can observe the other**.
//!
//! `weles-wire-contract` closes part of that in `--fast`: it drives the REAL
//! derives on both sides, so the serde spellings and field names cannot drift.
//! What it structurally cannot reach:
//!
//! * the HTTP **method** and the **status↔code** pairing — it never makes a
//!   request;
//! * that `cmd/gateway-svc`'s `main` wires `std::env::var` + `remote::resolve_peer`
//!   together AT ALL. `addrs::gateway_addrs` is proven with injected closures, so
//!   its tests pin that env mode asks NO agent — nothing there pins that the real
//!   main passes the real resolver;
//! * the `Http` address class end to end. Since Step 4 made gateway-svc
//!   `Addrs::Asks`, its two passthrough origins were the fleet manifest's ONLY
//!   `AddrKind::Http` declarations — that class has no other live example.
//!
//! Until this stage, **nothing booted weles and asserted on it**:
//! `weles/tests/platform.rs` spawns the binary as a `__test-child` for containment
//! primitives, and split-proof is hard-wired to processctl's fleet.
//!
//! # It may never be `#[ignore]`d, and never quietly not-run
//!
//! The blocking `test` stage runs `cargo test`, which SKIPS ignored tests — an
//! ignored proof is a comment that compiles. That is why this is a stage and not
//! a test. For the same reason it has no platform `cfg` escape: the shape next
//! door (`tools/processctl/tests/downstream`, whose `#[cfg(not(target_os =
//! "linux"))] fn main()` returns SUCCESS while executing nothing) is a green SKIP
//! wearing a PASS, and is deliberately NOT copied.
//!
//! # The lease: this stage BORROWS, it must never acquire
//!
//! verifyctl holds ONE `OwnedLease` for its entire manifest, naming the roles
//! `["splitproof", "weles"]` (`runner.rs`). weles takes the `"weles"` one-shot
//! through `weles::lock::acquire_or_borrow`. A `RolloutLock::acquire` here would
//! deadlock against verifyctl's own lease — the reason the plan's Step 5 taught
//! weles to borrow at all.
//!
//! # What is asserted, and how each part can fail on its own
//!
//! 1. **`/readyz` 200** on the gateway's HTTP port — it booted managed at all.
//! 2. **One op through Remote to a peer**: `GET /leaderboard` with a dev API key
//!    → 200. This crosses TWO resolved EDGE addresses (apikeys-svc verifies the
//!    key, leaderboard-svc answers the op), so it proves a resolved address is
//!    USED, not merely fetched. A gateway that fetched addresses and dialled the
//!    defaults would still answer `/readyz`.
//! 3. **One op through a passthrough**: `GET /admin/login` → 200, the `Http`
//!    class. Its failure mode is precise: an origin that never arrived is a BLANK
//!    origin, `ProxyTable::from_routes` drops a blank-origin route, and the
//!    request 404s. 200-vs-404 IS the discriminator.
//!
//! # Proving the gate can fail (a gate that cannot fail is theatre)
//!
//! Both ways the repo does it, because they answer different questions:
//!
//! * **Run the real thing and observe** — [`decoy_run`] spawns the SAME deployed
//!   `gateway-svc` binary with `ORCHESTRATOR_URL` pointed at a DEAD port and
//!   requires it to die of the managed-resolve path. Same binary, same argv, one
//!   variable changed: the agent. This is what makes the three assertions above
//!   genuinely depend on `resolve`, rather than passing because the fleet happens
//!   to work — and it needs no DB and no fleet, so it runs first and cheaply.
//! * **Drive the decision with staged inputs** — [`findings`] is pure and total,
//!   and its tests hand it each wrong observation in turn. That is what pins the
//!   comparator itself (that each assertion fails for its OWN reason, and that a
//!   decoy which BOOTS is a finding rather than an absence of one).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use processctl::{
    OutputDestination, OwnedChild, ProcessGroupPolicy, ShutdownPolicy, SpawnSpec, WorkspaceLayout,
};

use crate::model::Outcome;
use crate::runner::{self, Context};

/// How long the whole 12-service fleet gets to answer the gateway's `/readyz`.
///
/// Generous on purpose, and NOT a performance assertion: weles gates each
/// service on its own `HEALTH_DEADLINE` (30s) sequentially and mints a CA before
/// the first spawn. This bound exists so a wedged boot fails the stage instead of
/// hanging the run — it is a hang guard with headroom, never a stopwatch.
const BOOT_DEADLINE: Duration = Duration::from_secs(300);

/// Teardown budget for `weles up`: Ctrl-Break/SIGTERM, then force. The graceful
/// half must exceed weles's own worst-case teardown (12 services × its 5s+5s
/// stop budget) so a clean stop is not force-killed by this stage's impatience.
const FLEET_SHUTDOWN: ShutdownPolicy = ShutdownPolicy {
    graceful_timeout: Duration::from_secs(150),
    force_timeout: Duration::from_secs(30),
};

/// How long the doomed decoy gateway gets to die. It fails before any bind, so
/// this is pure headroom (`remote`'s own resolve timeout is 5s).
const DECOY_DEADLINE: Duration = Duration::from_secs(60);

const PROBE_INTERVAL: Duration = Duration::from_millis(250);
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The dev API key seeded by `APIKEYS_DEV_SEED=1` (which weles's manifest sets on
/// apikeys-svc) whose policy covers the player-facing list — `/leaderboard`
/// included, `match.report` not.
const DEV_KEY: &str = "dev-key-client";

/// The phrase `cmd/gateway-svc`'s managed-resolve failure arm prints
/// (`addrs::managed_addr`'s catch-all `bail!`), used as evidence that the decoy
/// died THERE and not somewhere incidental.
///
/// Yes, this reads prose — the one place in this stage that does, and only ever
/// to strengthen a NEGATIVE. Nothing branches on it in production
/// (`managed_addr` branches on `remote::ErrorCode`); if the wording changes, this
/// stage FAILS LOUDLY and is re-pointed, which is the correct outcome for a
/// gate whose whole job is knowing why the decoy died. A bare non-zero exit would
/// be satisfied by the binary failing to start for any reason at all — a missing
/// DLL would "prove" resolve.
const MANAGED_FAILURE_EVIDENCE: &str = "does not fall back to env";

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    // Every path a child needs, resolved BEFORE the lease is lent: the borrowed
    // child holds `ctx` mutably for its whole life.
    let root = ctx.root.clone();
    let weles_exe = weles_executable(ctx);
    if !weles_exe.is_file() {
        ctx.note("the weles executable was not produced by the build stage")?;
        return Ok(Outcome::Fail);
    }
    // weles NEVER builds: it runs only what `weles deploy` staged. The blocking
    // build stage produced the binaries; this stages them, from the same
    // directory the weles binary itself came out of (one derivation, not a second
    // guess at where the build output lives).
    let build_dir = weles_exe
        .parent()
        .context("weles executable has no parent directory")?
        .to_path_buf();
    if ctx.command(
        "deploy",
        weles_exe.clone(),
        vec![OsString::from("deploy"), build_dir.into_os_string()],
    )? != Outcome::Pass
    {
        ctx.note("weles deploy failed — see the stage log; the fleet was never booted")?;
        return Ok(Outcome::Fail);
    }

    // The gateway's OWN identity comes from weles's manifest, never a literal
    // here: the manifest is the authority for where the fleet listens, and this
    // stage is asserting on that fleet.
    let gateway = weles::manifest::split_fleet()
        .into_iter()
        .find(|svc| svc.name == "gateway-svc")
        .context("weles's split fleet no longer contains gateway-svc")?;
    let base = format!("http://127.0.0.1:{}", gateway.http_port);
    // The staged binary, through weles's own pinned-generation authority — the
    // same file `weles up` would spawn, resolved the same way.
    let staged_gateway = weles::prep::Layout::discover(root.clone())
        .context("discover the deployed generation weles just staged")?
        .binary(gateway.pkg);

    let environment = ctx.rollout_environment().clone();
    let decoy = decoy_run(ctx, &staged_gateway, &environment)?;

    let spec = SpawnSpec {
        label: "verify-weles-managed-gateway-up".into(),
        executable: weles_exe,
        args: vec![OsString::from("up"), OsString::from("split")],
        env: runner::os_environment(&environment),
        cwd: root,
        stdout: OutputDestination::File(ctx.stage_log("up", "out")),
        stderr: OutputDestination::File(ctx.stage_log("up", "err")),
        process_group: ProcessGroupPolicy::Owned,
    };
    let up_logs = [ctx.stage_log("up", "out"), ctx.stage_log("up", "err")];

    // From here to `drop(fleet)` the context is mutably borrowed by the lease.
    let mut fleet = ctx.borrow_rollout(spec, weles::lock::BORROWER_ROLE)?;
    let observed = (|| -> Result<Observed> {
        let runtime = tokio::runtime::Runtime::new()?;
        let client = runtime.block_on(async {
            reqwest::Client::builder()
                .timeout(PROBE_TIMEOUT)
                // The passthrough answers 200 for the login FORM; a redirect
                // would let a 303 masquerade as a served route.
                .redirect(reqwest::redirect::Policy::none())
                .build()
        })?;
        let readyz = wait_ready(&mut fleet, &runtime, &client, &base)?;
        // Only ask the rest if the fleet is up: probing a fleet that never
        // booted produces three findings for one fact.
        let (leaderboard, passthrough) = if readyz == Ok(200) {
            (
                get(&runtime, &client, &format!("{base}/leaderboard"), &[("X-Api-Key", DEV_KEY)]),
                get(&runtime, &client, &format!("{base}/admin/login"), &[]),
            )
        } else {
            (Err(NOT_PROBED.to_string()), Err(NOT_PROBED.to_string()))
        };
        Ok(Observed { decoy, readyz, leaderboard, passthrough })
    })();
    let stopped = fleet.shutdown(FLEET_SHUTDOWN);
    drop(fleet);

    let observed = observed?;
    let mut findings = findings(&observed);
    if let Err(error) = stopped {
        // An orphaned 12-service fleet would poison every rollout after this one:
        // it is a stage failure even if all three assertions passed.
        findings.push(format!(
            "weles up could not be stopped ({error}) — the fleet may be orphaned against the \
             shared Postgres"
        ));
    }

    if findings.is_empty() {
        return Ok(Outcome::Pass);
    }
    eprintln!(
        "verifyctl: managed-gateway proof failed ({} finding(s)); fleet logs: {}",
        findings.len(),
        up_logs.map(|path| path.display().to_string()).join(" / ")
    );
    for finding in &findings {
        eprintln!("  {finding}");
        ctx.note(finding)?;
    }
    Ok(Outcome::Fail)
}

/// Marker for a probe that was never made because the fleet never came up.
const NOT_PROBED: &str = "not probed: the gateway never became ready";

/// A probe's outcome: the status it answered, or why there was no answer.
type Probe = std::result::Result<u16, String>;

/// What the doomed decoy did.
#[derive(Clone, Debug)]
pub(crate) struct DecoyRun {
    /// `None` = it was still running when its deadline expired.
    exit_code: Option<i32>,
    /// Its stdout+stderr, so "died for the RIGHT reason" is decidable.
    logs: String,
}

/// Everything the live run observed. Separated from [`findings`] so the verdict
/// is a pure function of it — that is what lets a test stage the wrong world.
#[derive(Clone, Debug)]
pub(crate) struct Observed {
    decoy: DecoyRun,
    readyz: Probe,
    leaderboard: Probe,
    passthrough: Probe,
}

/// The verdict. Pure and total: no I/O, so every branch is reachable from a test
/// with staged inputs.
///
/// Each finding names a DIFFERENT fact, and each is reachable alone:
/// `/readyz` = it booted managed; `/leaderboard` = a resolved EDGE address is
/// really dialled; `/admin/login` = the `Http` class arrived; the decoy = the
/// three above depend on resolve at all.
pub(crate) fn findings(observed: &Observed) -> Vec<String> {
    let mut findings = decoy_findings(&observed.decoy);
    for (probe, expected, what) in [
        (
            &observed.readyz,
            200,
            "gateway-svc /readyz — the front door did not come up in managed mode (it was \
             handed ORCHESTRATOR_URL and no address env at all)",
        ),
        (
            &observed.leaderboard,
            200,
            "GET /leaderboard with a dev api key — the op crosses TWO resolved EDGE addresses \
             (apikeys-svc verifies the key, leaderboard-svc answers). 401/403 means the apikeys \
             edge was not reached, 404/502 means the leaderboard edge was not: an address that \
             was fetched but not USED still answers /readyz, so this is the assertion that \
             catches it",
        ),
        (
            &observed.passthrough,
            200,
            "GET /admin/login — the Http address class (a passthrough ORIGIN, which \
             weles-wire-contract cannot reach and which gateway-svc's two declarations were the \
             fleet's only live example of). 404 is precisely the blank-origin symptom: an origin \
             that never arrived leaves ProxyTable::from_routes dropping the route",
        ),
    ] {
        match probe {
            Ok(status) if *status == expected => {}
            Ok(status) => findings.push(format!("{what}: answered {status}, expected {expected}")),
            Err(error) => findings.push(format!("{what}: {error}")),
        }
    }
    findings
}

/// The falsifiability half: what the decoy must have done for the assertions
/// above to mean anything.
fn decoy_findings(decoy: &DecoyRun) -> Vec<String> {
    match decoy.exit_code {
        // THE finding this whole decoy exists for. A gateway that boots with no
        // agent reachable is a gateway whose addresses did not come from the
        // agent — so a green /readyz + /leaderboard + /admin/login above would be
        // proving the fleet works, not that managed mode does.
        Some(0) => vec![format!(
            "the decoy gateway-svc BOOTED with ORCHESTRATOR_URL pointed at a dead port \
             (exit 0). Managed mode is then not depending on resolve at all — this stage's \
             three assertions would pass for reasons unrelated to the agent, and the gate is \
             theatre. Check cmd/gateway-svc/src/addrs.rs: managed boot must never fall back \
             to env."
        )],
        None => vec![format!(
            "the decoy gateway-svc was still running {DECOY_DEADLINE:?} after being pointed at \
             a dead agent port — a managed boot must FAIL, not hang: an edge peer that cannot \
             be resolved has no benign value to wait for"
        )],
        Some(_) if !decoy.logs.contains(MANAGED_FAILURE_EVIDENCE) => vec![format!(
            "the decoy gateway-svc died as required, but not visibly in the managed-resolve \
             path: its output carries no {MANAGED_FAILURE_EVIDENCE:?}. Either it failed for an \
             unrelated reason (in which case this decoy proves nothing about resolve), or \
             addrs::managed_addr's message changed and this stage must be re-pointed at the new \
             one."
        )],
        Some(_) => Vec::new(),
    }
}

/// Runs the SAME deployed `gateway-svc` with `ORCHESTRATOR_URL` at a dead port,
/// and reports what it did. No DB, no fleet, no ports taken: the process dies in
/// `main` before `app::run` binds anything, which is why this can run before the
/// fleet boots.
fn decoy_run(
    ctx: &mut Context<'_>,
    gateway: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<DecoyRun> {
    let port = dead_port()?;
    let mut environment = environment.clone();
    environment.insert("ORCHESTRATOR_URL".into(), format!("http://127.0.0.1:{port}"));
    let mut child = OwnedChild::spawn(SpawnSpec {
        label: "verify-weles-managed-gateway-decoy".into(),
        executable: gateway.to_path_buf(),
        args: Vec::new(),
        env: runner::os_environment(&environment),
        cwd: ctx.root.clone(),
        stdout: OutputDestination::File(ctx.stage_log("decoy", "out")),
        stderr: OutputDestination::File(ctx.stage_log("decoy", "err")),
        process_group: ProcessGroupPolicy::Owned,
    })?;
    let deadline = Instant::now() + DECOY_DEADLINE;
    let exit_code = loop {
        if let Some(status) = child.try_wait()? {
            break status.code();
        }
        if runner::interrupted() || Instant::now() >= deadline {
            // It must not survive this stage either way: a gateway that DID boot
            // holds :8082, which the fleet is about to want.
            let _ = child.shutdown(ShutdownPolicy {
                graceful_timeout: Duration::from_secs(2),
                force_timeout: Duration::from_secs(5),
            });
            break None;
        }
        std::thread::sleep(PROBE_INTERVAL);
    };
    let logs = format!(
        "{}\n{}",
        std::fs::read_to_string(ctx.stage_log("decoy", "out")).unwrap_or_default(),
        std::fs::read_to_string(ctx.stage_log("decoy", "err")).unwrap_or_default()
    );
    Ok(DecoyRun { exit_code, logs })
}

/// A port nothing is listening on: bind one, learn its number, release it.
///
/// The window between release and the decoy's dial is a race only another
/// process racing for an ephemeral port could lose — and under the one-rollout
/// protocol nothing else is running. If it ever did lose, the decoy would meet a
/// listener that is not an agent and die on `unknown_route`/`Malformed`, i.e.
/// still in the managed path: the failure mode is a still-correct verdict, not a
/// false PASS.
fn dead_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).context("reserve a dead agent port")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// Polls `/readyz` until it answers 200, the fleet dies, or [`BOOT_DEADLINE`].
///
/// A weles that EXITED is reported as its own failure rather than as a timeout:
/// "the supervisor is gone" and "the gateway is slow" are different facts, and
/// only one of them is worth waiting five minutes for.
fn wait_ready(
    fleet: &mut processctl::BorrowedChild<'_>,
    runtime: &tokio::runtime::Runtime,
    client: &reqwest::Client,
    base: &str,
) -> Result<Probe> {
    let deadline = Instant::now() + BOOT_DEADLINE;
    loop {
        if let Some(status) = fleet.try_wait()? {
            return Ok(Err(format!(
                "weles up exited ({status:?}) before the gateway was ready — see the fleet logs"
            )));
        }
        if runner::interrupted() {
            return Ok(Err("interrupted before the gateway was ready".into()));
        }
        if let Ok(200) = get(runtime, client, &format!("{base}/readyz"), &[]) {
            return Ok(Ok(200));
        }
        if Instant::now() >= deadline {
            return Ok(Err(format!(
                "the gateway did not answer /readyz within {BOOT_DEADLINE:?}"
            )));
        }
        std::thread::sleep(PROBE_INTERVAL);
    }
}

fn get(
    runtime: &tokio::runtime::Runtime,
    client: &reqwest::Client,
    url: &str,
    headers: &[(&str, &str)],
) -> Probe {
    runtime
        .block_on(async {
            let mut request = client.get(url);
            for (name, value) in headers {
                request = request.header(*name, *value);
            }
            request.send().await
        })
        .map(|response| response.status().as_u16())
        .map_err(|error| format!("{url} did not answer: {error}"))
}

fn weles_executable(ctx: &Context<'_>) -> PathBuf {
    WorkspaceLayout::from_root(ctx.root.clone(), ctx.environment()).binary("debug", "weles")
}

#[cfg(test)]
#[path = "weles_managed_gateway_tests.rs"]
mod weles_managed_gateway_tests;
