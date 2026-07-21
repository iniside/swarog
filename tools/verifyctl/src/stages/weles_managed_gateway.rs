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
//! # The trap this stage was REBUILT around: resolved and defaulted are the same bytes
//!
//! The first version of this stage claimed `GET /leaderboard` → 200 proved a
//! resolved EDGE address was *used*. **It proved no such thing**, and the reason
//! generalizes to every edge peer: weles's `service_addr` formats
//! `127.0.0.1:{edge_port}` (leaderboard 9008, apikeys 9009), and
//! `cmd/gateway-svc`'s `ADDR_SPECS` carry exactly those bytes as their standalone
//! defaults. A gateway that resolved all eight addresses and then dialled
//! `spec.env_default` answers `/leaderboard` 200 — so the assertion was blind to
//! the one regression that matters here: someone "softening" `managed_addr` into
//! `answer.unwrap_or(spec.env_default)` after a flaky-boot incident, which is the
//! precise hack `addrs.rs` forbids in prose and nothing forbade in code.
//!
//! The `Http` class was accidentally sound (`ADMIN_HTTP_ADDR`'s default is `""`,
//! and a blank origin is a DROPPED route → 404) — but only by accident: nothing
//! pinned that blank, so one plausible dev default would have made all three
//! assertions insensitive at once. [`the_passthrough_defaults_must_stay_blank`]
//! now pins it, and it is no longer what the proof rests on.
//!
//! **[`swap_probe`] is what makes used-vs-fetched observable**, and it does so
//! WITHOUT diverging any shipped manifest: the stage stands up its own fake agent
//! and answers with addresses IT owns, which are not any default by construction.
//! (The alternative — moving a peer's port in `weles::manifest` so it differs
//! from the default — was rejected: it would diverge weles's fleet from
//! processctl's for no operational reason. The divergence belongs inside this
//! stage, not in the fleet everything else runs.)
//!
//! # What is asserted, and how each part can fail on its own
//!
//! On the REAL fleet (weles up, booting the deployed fleet.toml), the front door
//! works end to end:
//!
//! 1. **`/readyz` 200** — it booted managed at all, handed only `ORCHESTRATOR_URL`.
//! 2. **`GET /leaderboard` + dev key → 200** — a POSITIVE CONTROL, and it is
//!    labelled as one: the managed front door really dispatches an op Remote over
//!    the mTLS edge to a live peer, and really verifies the key against another
//!    (401 without a key). It does NOT discriminate resolved from defaulted —
//!    same bytes. Do not re-read it as more.
//! 3. **`GET /admin/login` → 200** — the same, for the `Http` class: a real
//!    passthrough to a real origin.
//!
//! Then, against the stage's OWN fake agent ([`swap_probe`], while the fleet is
//! up so the real peers and the CA exist), the two facts the fleet cannot show:
//!
//! 4. **the resolved `Http` origin is USED** — the agent answers `admin`'s origin
//!    as a port the STAGE serves, and the marker in the body can only come from
//!    there. Neither the blank default nor a hypothetical `:8085` default could
//!    produce it.
//! 5. **the resolved `Edge` address is USED** — the agent answers `leaderboard`'s
//!    edge as a port the STAGE owns (no QUIC server on it). Two independent
//!    observations: `/leaderboard` must NOT answer 200 (a 200 means it dialled
//!    9008, the default, i.e. fetched-and-discarded), and a UDP datagram must
//!    ARRIVE at that port (a positive: bytes went to the address the agent
//!    chose — proof by construction, not by absence of errors).
//!
//! # Proving the gate can fail (a gate that cannot fail is theatre)
//!
//! Both ways the repo does it, because they answer different questions:
//!
//! * **Run the real thing and observe** — [`decoy_run`] spawns the SAME deployed
//!   `gateway-svc` binary with `ORCHESTRATOR_URL` pointed at a DEAD port and
//!   requires it to die of the managed-resolve path. Same binary, same argv, one
//!   variable changed: the agent. It needs no DB and no fleet, so it runs first
//!   and cheaply.
//! * **Drive the decision with staged inputs** — [`findings`] is pure and total,
//!   and its tests hand it each wrong observation in turn: each assertion must
//!   fail for its OWN reason, and the world where the fleet is green but the
//!   decoy BOOTED must be refused.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use processctl::{
    OutputDestination, OwnedChild, ProcessGroupPolicy, ShutdownPolicy, SpawnSpec, WorkspaceLayout,
};

use crate::model::Outcome;
use crate::runner::{self, Context};
use crate::stages::fake_http;

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

/// The swap probe's gateway boots alone (its peers are already up), so it needs
/// far less than the fleet — but still a hang guard, not a stopwatch.
const SWAP_BOOT_DEADLINE: Duration = Duration::from_secs(90);

/// Longer than a probe of a healthy route: the swapped `/leaderboard` is
/// EXPECTED to fail at the edge's dial deadline (5s) plus the front door's
/// admission budget, and this must not cut it short and report a client timeout
/// where the answer is the point.
const SWAP_REQUEST_TIMEOUT: Duration = Duration::from_secs(45);

/// How long to wait for a datagram at the address the fake agent advertised.
/// Zero race: the dial happens during the `/leaderboard` request above, and a
/// datagram already sitting in the socket buffer is read immediately — this
/// bounds only the case where NOTHING was ever sent.
const EDGE_DIAL_WAIT: Duration = Duration::from_secs(5);

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

    // The operator fleet weles deploys AND boots — the file that replaced the
    // deleted `manifest::split_fleet()` table. The stage reads the SAME file to
    // learn where the fleet listens (ports/peers), so its assertions and the
    // booted fleet share one authority. `deploy` stamps it into the generation
    // (`--fleet`); `up` reads it back and boots it. Loaded once here and threaded
    // to the two probes below, never re-parsed.
    let fleet_fixture = root.join("weles/fleet.split.toml");
    let fleet_def = weles::fleet_toml::load(&fleet_fixture)
        .context("load weles/fleet.split.toml — the fleet this stage boots and asserts on")?;

    if ctx.command(
        "deploy",
        weles_exe.clone(),
        vec![
            OsString::from("deploy"),
            build_dir.into_os_string(),
            OsString::from("--fleet"),
            fleet_fixture.into_os_string(),
        ],
    )? != Outcome::Pass
    {
        ctx.note("weles deploy failed — see the stage log; the fleet was never booted")?;
        return Ok(Outcome::Fail);
    }

    // The gateway's OWN identity comes from the deployed fleet, never a literal
    // here: the fleet file is the authority for where the fleet listens, and this
    // stage is asserting on that fleet.
    let gateway = fleet_def
        .services
        .iter()
        .find(|svc| svc.name == "gateway-svc")
        .context("weles's split fleet no longer contains gateway-svc")?;
    let base = format!("http://127.0.0.1:{}", gateway.http_port);
    // The staged binary, through weles's own pinned-generation authority — the
    // same file `weles up` would spawn, resolved the same way.
    let staged_gateway = weles::prep::Layout::discover(root.clone())
        .context("discover the deployed generation weles just staged")?
        .binary(&gateway.pkg);

    let environment = ctx.rollout_environment().clone();
    let decoy = decoy_run(ctx, &staged_gateway, &environment, &base)?;

    // The swap probe's world, resolved now: once the lease is lent, `ctx` is
    // borrowed. The CA is the one weles mints for THIS fleet — its `edgeca`
    // `[[prepare]]` hook writes run/weles/edge-ca.{crt,key} before any service
    // spawns — so the probe's gateway dials the real peers with the same
    // material they trust. The stage runs NO separate CA pre-step; `weles up`
    // provisions it.
    let swap_input = SwapInput {
        gateway: staged_gateway,
        services: fleet_def.services.clone(),
        root: ctx.root.clone(),
        environment: environment.clone(),
        ca_cert: ctx.root.join("run/weles/edge-ca.crt"),
        ca_key: ctx.root.join("run/weles/edge-ca.key"),
        stdout: ctx.stage_log("swap", "out"),
        stderr: ctx.stage_log("swap", "err"),
    };

    let spec = SpawnSpec {
        label: "verify-weles-managed-gateway-up".into(),
        executable: weles_exe,
        args: vec![OsString::from("up")],
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
        let (leaderboard, passthrough, swap) = if readyz == Ok(200) {
            // The gateway's /readyz is a PLAIN 200 (`without_db`) and reflects
            // nothing about the two passthrough origins (admin-svc, accounts-svc):
            // they are HTTP proxy TARGETS, not `remote::Stub` peers, so no
            // readiness probe stands for them — and weles boots admin-svc LAST, AFTER
            // the gateway. Gating the `/admin/login` positive control on the
            // gateway's readyz alone therefore RACES admin-svc's boot and answers
            // 502 (proven live: pause admin-svc → gateway /readyz stays 200 while
            // /admin/login 502s). Wait for the whole fleet to actually serve before
            // probing, so the positive control tests the passthrough — not the boot
            // clock. Order-independent (every service, not just the last one) so a
            // future manifest reorder or a new passthrough cannot reopen this race.
            wait_fleet_serving(&mut fleet, &runtime, &client, &fleet_def.services)?;
            (
                get(&runtime, &client, &format!("{base}/leaderboard"), &[("X-Api-Key", DEV_KEY)]),
                get(&runtime, &client, &format!("{base}/admin/login"), &[]),
                // The used-vs-fetched half, against the stage's own agent — it
                // needs the real peers and the real CA, so it runs HERE, inside
                // the live fleet's window.
                swap_probe(&swap_input).map_err(|error| format!("{error:#}")),
            )
        } else {
            (
                Err(NOT_PROBED.to_string()),
                Err(NOT_PROBED.to_string()),
                Err(NOT_PROBED.to_string()),
            )
        };
        Ok(Observed { decoy, readyz, leaderboard, passthrough, swap })
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

/// How the doomed decoy ended — the FACTS that can actually occur, so a verdict
/// never has to guess which one it is looking at.
///
/// The first version had `exit_code: Option<i32>`, which could not express the
/// regression it existed to catch: a gateway that fell back to env does not
/// *exit 0*, it SERVES until signalled (`core/app`'s run loop). That world landed
/// in the `None` arm and was reported as "still running — a managed boot must
/// FAIL, not hang", sending the reader after a hang while the real defect was
/// "it booted and is holding :8082". [`Survived`](Self::Survived) is that fact,
/// named — and it is separated from a true [`Hung`](Self::Hung) by PROBING the
/// port before the kill, not by assuming.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DecoyEnd {
    /// It exited with this code. The only end that can carry the evidence.
    Exited(i32),
    /// It exited without a code: killed by a signal (`ExitStatus::code()` is
    /// `None` on unix). Says nothing about resolve, and must not be reported as
    /// if it did.
    Signalled,
    /// Still running at the deadline AND its HTTP port answered: it BOOTED
    /// without an agent. THE regression this decoy exists for.
    Survived,
    /// Still running at the deadline and its port answered nothing: a genuine
    /// hang, somewhere before serving.
    Hung,
    /// This stage was interrupted while waiting. An operator fact, not a verdict
    /// about the gateway — never an accusation against `addrs.rs`.
    Interrupted,
}

/// What the doomed decoy did.
#[derive(Clone, Debug)]
pub(crate) struct DecoyRun {
    end: DecoyEnd,
    /// Its stdout+stderr, so "died for the RIGHT reason" is decidable.
    logs: String,
}

/// What the swap probe saw: the fake-agent run, whose answers are addresses the
/// STAGE owns and no default could be.
#[derive(Clone, Debug)]
pub(crate) struct SwapProbe {
    /// Did the swapped gateway's HTTP port ANSWER — any status?
    ///
    /// Deliberately not "`/readyz` 200", and the reason is load-bearing: every
    /// `remote::Stub` contributes a per-peer readiness probe
    /// (`core/remote`'s `readiness_verdict`), and this probe POINTS ONE PEER AT A
    /// BLACK HOLE on purpose. So this gateway is designed to be UNREADY, and
    /// waiting for 200 waits for a state the experiment removed — as the first
    /// live run of this probe demonstrated, sitting out its whole 90s budget
    /// against a gateway that had been serving since second three.
    ///
    /// "The port answers" is what "it booted" means here. Readiness is the
    /// FLEET probe's question, where nothing is sabotaged.
    serving: Probe,
    /// `GET /admin/login` through the passthrough, whose origin the fake agent
    /// pointed at a port the stage serves. `Ok(true)` = the stage's marker came
    /// back, so the RESOLVED origin was dialled.
    origin_marker: std::result::Result<bool, String>,
    /// `GET /leaderboard` when the fake agent pointed leaderboard's EDGE at a
    /// port with no QUIC server. A 200 means the gateway reached the REAL
    /// leaderboard-svc on the default 9008 — fetched and discarded.
    leaderboard: Probe,
    /// Did a datagram arrive at the port the fake agent named as leaderboard's
    /// edge? The positive half: bytes went where the agent said.
    edge_dialled: bool,
}

/// Everything the live run observed. Separated from [`findings`] so the verdict
/// is a pure function of it — that is what lets a test stage the wrong world.
#[derive(Clone, Debug)]
pub(crate) struct Observed {
    decoy: DecoyRun,
    readyz: Probe,
    leaderboard: Probe,
    passthrough: Probe,
    /// `Err` when the probe could not be RUN (a fixture failed to bind). Never
    /// silently absent: a proof that did not execute is a finding.
    swap: std::result::Result<SwapProbe, String>,
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
            "GET /leaderboard with a dev api key (POSITIVE CONTROL: the managed front door \
             really dispatches an op Remote over the mTLS edge to a live peer, and verifies the \
             key against another). NOTE: this does NOT discriminate a resolved address from a \
             defaulted one — they are the same bytes; swap_probe is what does",
        ),
        (
            &observed.passthrough,
            200,
            "GET /admin/login (POSITIVE CONTROL for the Http class: a real passthrough to a \
             real origin). Whether the RESOLVED origin was used is swap_probe's question",
        ),
    ] {
        match probe {
            Ok(status) if *status == expected => {}
            Ok(status) => findings.push(format!("{what}: answered {status}, expected {expected}")),
            Err(error) => findings.push(format!("{what}: {error}")),
        }
    }
    findings.extend(swap_findings(&observed.swap));
    findings
}

/// The used-vs-fetched verdict — the part of this stage that the real fleet
/// structurally cannot deliver.
fn swap_findings(swap: &std::result::Result<SwapProbe, String>) -> Vec<String> {
    let swap = match swap {
        Ok(swap) => swap,
        // A proof that did not RUN is a finding. Reporting nothing here would be
        // the vacuous green this whole stage exists to refuse.
        Err(error) => {
            return vec![format!(
                "the swap probe could not be run ({error}) — so nothing in this stage \
                 distinguishes a resolved address from the standalone default, which are the \
                 same bytes on the real fleet"
            )]
        }
    };
    let mut findings = Vec::new();
    let serving = swap.serving.is_ok();
    if let Err(error) = &swap.serving {
        findings.push(format!(
            "the swap probe's gateway never served: {error} — so the used-vs-fetched questions \
             below were never really asked. (Note it is EXPECTED to be unready: one of its peers \
             is a black hole by design, and every Stub contributes a readiness probe. Serving is \
             the signal; readiness is not.)"
        ));
    }
    if serving {
        match &swap.origin_marker {
            Ok(true) => {}
            Ok(false) => findings.push(
                "the resolved Http ORIGIN was not used: the fake agent answered admin's origin \
                 with a port THIS STAGE serves, and /admin/login did not come back with its \
                 marker. The address was fetched and something else was dialled (or the route \
                 was dropped)"
                    .to_string(),
            ),
            Err(error) => findings.push(format!(
                "the resolved Http ORIGIN could not be checked: /admin/login {error}"
            )),
        }
        // THE assertion the first version of this stage was missing. 200 here is
        // only reachable by dialling 9008 — the standalone default — because the
        // agent answered a port with no QUIC server on it.
        if swap.leaderboard == Ok(200) {
            findings.push(
                "the resolved EDGE address was FETCHED AND DISCARDED: the fake agent answered \
                 leaderboard's edge with a port that serves no QUIC, yet /leaderboard came back \
                 200 — which is only possible by dialling the standalone default 127.0.0.1:9008. \
                 Managed boot is falling back to env (check managed_addr for an \
                 `unwrap_or(spec.env_default)`-shaped 'softening')"
                    .to_string(),
            );
        }
        if !swap.edge_dialled {
            findings.push(
                "nothing ever dialled the resolved EDGE address: the fake agent named a port \
                 this stage owns and no datagram arrived at it. Absence of a 200 is not proof \
                 the resolved address was used — this is the positive half, and it is missing"
                    .to_string(),
            );
        }
    }
    findings
}

/// The falsifiability half: what the decoy must have done for the assertions
/// above to mean anything.
fn decoy_findings(decoy: &DecoyRun) -> Vec<String> {
    match decoy.end {
        // THE finding this whole decoy exists for. A gateway that SERVES with no
        // agent reachable is a gateway whose addresses did not come from the
        // agent — so a green /readyz + /leaderboard + /admin/login above would be
        // proving the fleet works, not that managed mode does.
        DecoyEnd::Survived => vec![
            "the decoy gateway-svc BOOTED and is SERVING with ORCHESTRATOR_URL pointed at a \
             dead port — its HTTP port answered. Managed mode is then not depending on resolve \
             at all: this stage's assertions would pass for reasons unrelated to the agent, and \
             the gate is theatre. Check cmd/gateway-svc/src/addrs.rs — managed boot must never \
             fall back to env."
                .to_string(),
        ],
        // Distinguished from Survived by an actual probe, not by assumption: this
        // one is not serving, so it is stuck somewhere before that.
        DecoyEnd::Hung => vec![format!(
            "the decoy gateway-svc was still running {DECOY_DEADLINE:?} after being pointed at \
             a dead agent port, and its HTTP port answered nothing — a managed boot must FAIL, \
             not hang: an edge peer that cannot be resolved has no benign value to wait for"
        )],
        // An operator interrupted this stage. Not a verdict about the gateway,
        // and emphatically not an accusation against addrs.rs.
        DecoyEnd::Interrupted => vec![
            "the decoy gateway-svc run was interrupted before it reached a verdict — this \
             stage's falsifiability was not established on this run"
                .to_string(),
        ],
        // Killed by a signal (unix: `ExitStatus::code()` is None). It died, but
        // not of its own accord, so it carries no evidence about resolve.
        DecoyEnd::Signalled => vec![
            "the decoy gateway-svc was killed by a signal rather than exiting on its own — it \
             says nothing about whether managed boot depends on resolve"
                .to_string(),
        ],
        DecoyEnd::Exited(0) => vec![
            "the decoy gateway-svc exited 0 with ORCHESTRATOR_URL pointed at a dead port: a \
             managed boot that cannot resolve its edge peers must FAIL, and a clean exit is not \
             a failure"
                .to_string(),
        ],
        DecoyEnd::Exited(_) if !decoy.logs.contains(MANAGED_FAILURE_EVIDENCE) => vec![format!(
            "the decoy gateway-svc died as required, but not visibly in the managed-resolve \
             path: its output carries no {MANAGED_FAILURE_EVIDENCE:?}. Either it failed for an \
             unrelated reason (in which case this decoy proves nothing about resolve), or \
             addrs::managed_addr's message changed and this stage must be re-pointed at the new \
             one."
        )],
        DecoyEnd::Exited(_) => Vec::new(),
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
    base: &str,
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
    let end = loop {
        if let Some(status) = child.try_wait()? {
            // No code = a signal took it (unix). That is a different fact, and
            // conflating it with an exit would let an operator's Ctrl-C read as
            // an accusation against addrs.rs.
            break match status.code() {
                Some(code) => DecoyEnd::Exited(code),
                None => DecoyEnd::Signalled,
            };
        }
        let interrupted = runner::interrupted();
        if interrupted || Instant::now() >= deadline {
            // Still running. WHICH still-running fact is it? Ask its port before
            // killing it: a gateway that fell back to env is SERVING (core/app's
            // run loop holds until signalled), and that is the regression this
            // decoy exists for — while a true hang answers nothing. Guessing here
            // is what sent the last reader hunting a hang.
            let serving = probe_once(base);
            // It must not survive this stage either way: a gateway that DID boot
            // holds the port the fleet is about to want.
            let _ = child.shutdown(ShutdownPolicy {
                graceful_timeout: Duration::from_secs(2),
                force_timeout: Duration::from_secs(5),
            });
            break if interrupted {
                DecoyEnd::Interrupted
            } else if serving {
                DecoyEnd::Survived
            } else {
                DecoyEnd::Hung
            };
        }
        std::thread::sleep(PROBE_INTERVAL);
    };
    let logs = format!(
        "{}\n{}",
        std::fs::read_to_string(ctx.stage_log("decoy", "out")).unwrap_or_default(),
        std::fs::read_to_string(ctx.stage_log("decoy", "err")).unwrap_or_default()
    );
    Ok(DecoyRun { end, logs })
}

/// One `/readyz` question, answered `true` only by a real 2xx. Its own tiny
/// runtime: it runs before the stage's main one exists, and it must never be the
/// reason a verdict changes — an error here means "not serving", which is what a
/// dead gateway is.
fn probe_once(base: &str) -> bool {
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        return false;
    };
    runtime.block_on(async {
        let Ok(client) = reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() else {
            return false;
        };
        client
            .get(format!("{base}/readyz"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
    })
}

/// The body the stage's fake admin origin answers with. Its presence in a
/// response through the gateway is the proof: nothing else on this machine
/// serves it.
const ORIGIN_MARKER: &str = "weles-managed-gateway-swap-origin";

/// The one question the real fleet cannot answer: **was the resolved address
/// USED, or merely fetched?**
///
/// On the real fleet it cannot be asked — the agent's answer and the standalone
/// default are the same bytes. Here the stage IS the agent, so it answers two of
/// the eight with addresses it owns and nothing could have guessed:
///
/// * `admin`'s HTTP origin → a port this stage SERVES. The marker can only come
///   from there.
/// * `leaderboard`'s EDGE → a port this stage OWNS but serves no QUIC on. So
///   `/leaderboard` must fail (a 200 means the real 9008 was dialled, i.e. the
///   default) AND a datagram must arrive (the resolved address really was
///   dialled).
///
/// The other six answers are the REAL fleet's addresses, straight from weles's
/// manifest — the gateway must boot and its key check must reach the real
/// apikeys-svc, or `/leaderboard` would never get far enough to dial anything.
///
/// Runs while the fleet is up: the real peers and the CA weles minted both have
/// to exist. Its gateway gets its OWN HTTP and player ports, so it never
/// contends with the fleet's front door.
fn swap_probe(input: &SwapInput) -> Result<SwapProbe> {
    // The fake agent answers from weles's OWN parser and encoders (the
    // `drift_probe_*` seams). A hand-rolled JSON body here would be a fourth copy
    // of the contract, free to drift from the server this stage is standing in
    // for — and the drift would show up as this stage failing to prove anything,
    // which is the worst possible place for it.
    let real = weles::manifest::PeerAddrs::from_fleet(&input.services);
    let origin = fake_http::FakeHttp::start(|_route, _body| {
        (200, ORIGIN_MARKER.as_bytes().to_vec())
    })
    .context("start the fake admin origin")?;
    // A socket the stage OWNS and never serves QUIC on. Held for the whole probe:
    // dropping it would free the port the agent is about to advertise.
    let edge = UdpSocket::bind(("127.0.0.1", 0)).context("bind the fake edge port")?;
    edge.set_read_timeout(Some(EDGE_DIAL_WAIT))
        .context("bound the wait for a datagram")?;
    let edge_addr = format!("127.0.0.1:{}", edge.local_addr()?.port());

    let swaps = vec![
        ("admin".to_string(), weles::manifest::AddrKind::Http, origin.addr()),
        (
            "leaderboard".to_string(),
            weles::manifest::AddrKind::Edge,
            edge_addr.clone(),
        ),
    ];
    let agent = fake_http::FakeHttp::start(move |route, body| {
        agent_answer(route, body, &swaps, &real)
    })
    .context("start the fake agent")?;

    let http_port = dead_port()?;
    let player_port = dead_udp_port()?;
    let base = format!("http://127.0.0.1:{http_port}");
    let mut environment = input.environment.clone();
    environment.insert(
        "ORCHESTRATOR_URL".into(),
        format!("http://127.0.0.1:{}", agent.port()),
    );
    // Its OWN ports: the fleet's gateway holds 8082 and the player plane's 9100.
    environment.insert("PORT".into(), format!(":{http_port}"));
    environment.insert("PLAYER_EDGE_ADDR".into(), format!(":{player_port}"));
    environment.insert("TLS_MODE".into(), "off".into());
    // The mTLS material weles minted for this fleet — without it the stubs cannot
    // dial the real peers, and the key check would never reach apikeys-svc.
    environment.insert(
        "EDGE_CA_CERT".into(),
        input.ca_cert.to_string_lossy().into_owned(),
    );
    environment.insert(
        "EDGE_CA_KEY".into(),
        input.ca_key.to_string_lossy().into_owned(),
    );

    let mut child = OwnedChild::spawn(SpawnSpec {
        label: "verify-weles-managed-gateway-swap".into(),
        executable: input.gateway.clone(),
        args: Vec::new(),
        env: runner::os_environment(&environment),
        cwd: input.root.clone(),
        stdout: OutputDestination::File(input.stdout.clone()),
        stderr: OutputDestination::File(input.stderr.clone()),
        process_group: ProcessGroupPolicy::Owned,
    })?;

    let probe = (|| -> Result<SwapProbe> {
        let runtime = tokio::runtime::Runtime::new()?;
        let client = runtime.block_on(async {
            reqwest::Client::builder()
                .timeout(SWAP_REQUEST_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .build()
        })?;
        let serving = wait_serving(&mut child, &runtime, &client, &base, SWAP_BOOT_DEADLINE)?;
        if serving.is_err() {
            return Ok(SwapProbe {
                serving,
                origin_marker: Err(NOT_PROBED.into()),
                leaderboard: Err(NOT_PROBED.into()),
                edge_dialled: false,
            });
        }
        let origin_marker = body_of(&runtime, &client, &format!("{base}/admin/login"), &[])
            .map(|body| body.contains(ORIGIN_MARKER));
        // Fire the op, THEN look for the datagram: the dial happens while this
        // request is in flight (it fails at the edge's dial deadline), and UDP
        // datagrams sit in the socket's buffer until read — so this is a
        // happens-before, not a race with a clock.
        let leaderboard = get(
            &runtime,
            &client,
            &format!("{base}/leaderboard"),
            &[("X-Api-Key", DEV_KEY)],
        );
        let edge_dialled = edge.recv_from(&mut [0u8; 2048]).is_ok();
        Ok(SwapProbe { serving, origin_marker, leaderboard, edge_dialled })
    })();

    let _ = child.shutdown(ShutdownPolicy {
        graceful_timeout: Duration::from_secs(15),
        force_timeout: Duration::from_secs(10),
    });
    drop(child);
    drop(agent);
    drop(origin);
    probe
}

/// The stage's fake agent, as a pure-ish function of the question.
///
/// It answers through weles's OWN `drift_probe_*` seams — the real
/// `ResolveRequest` parser (`deny_unknown_fields` included) and the real response
/// encoders. A hand-rolled body here would be a FOURTH copy of the contract,
/// free to drift from the server it stands in for; the drift would surface as
/// this stage failing to prove anything, which is the worst place for it.
///
/// `swaps` are the answers the stage OWNS (an address no default could be);
/// everything else is the real fleet's address, so the gateway boots and its key
/// check reaches the real apikeys-svc.
fn agent_answer(
    route: &str,
    body: &[u8],
    swaps: &[(String, weles::manifest::AddrKind, String)],
    real: &weles::manifest::PeerAddrs,
) -> fake_http::Answer {
    // Method AND path, exactly as weles matches them: a client that drifted to
    // GET, or to another path, gets what weles would give it — which is what
    // makes this fixture a fair stand-in rather than a permissive one.
    if route != format!("POST {}", weles::agentapi::RESOLVE_PATH) {
        return (
            404,
            weles::agentapi::drift_probe_encode_error_response(
                weles::agentapi::ErrorCode::UnknownRoute,
                &format!("no route {route}"),
            ),
        );
    }
    let (provider, kind) = match weles::agentapi::drift_probe_parse_resolve_request(body) {
        Ok(question) => question,
        Err(error) => {
            return (
                400,
                weles::agentapi::drift_probe_encode_error_response(
                    weles::agentapi::ErrorCode::BadRequest,
                    &error,
                ),
            )
        }
    };
    let addrs = match swaps
        .iter()
        .find(|(name, swapped, _)| *name == provider && *swapped == kind)
    {
        Some((_, _, addr)) => vec![addr.clone()],
        None => real.lookup(&provider, kind),
    };
    if addrs.is_empty() {
        return (
            404,
            weles::agentapi::drift_probe_encode_error_response(
                weles::agentapi::ErrorCode::UnknownPeer,
                &format!("no {kind:?} address for {provider:?}"),
            ),
        );
    }
    (200, weles::agentapi::drift_probe_encode_resolve_response(addrs))
}

/// Everything [`swap_probe`] needs, resolved before the lease is lent.
struct SwapInput {
    gateway: PathBuf,
    /// The deployed fleet's services — the authority for the REAL peer addresses
    /// the fake agent answers with (`PeerAddrs::from_fleet`). Cloned from the
    /// once-loaded fleet so the probe never re-parses the fixture.
    services: Vec<weles::manifest::ServiceDef>,
    root: PathBuf,
    environment: BTreeMap<String, String>,
    ca_cert: PathBuf,
    ca_key: PathBuf,
    stdout: PathBuf,
    stderr: PathBuf,
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

/// The same, for a UDP port (the swap gateway's own player plane must not land on
/// the fleet's `:9100`).
fn dead_udp_port() -> Result<u16> {
    let socket = UdpSocket::bind(("127.0.0.1", 0)).context("reserve a player-plane port")?;
    let port = socket.local_addr()?.port();
    drop(socket);
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

/// Polls EVERY fleet service's own `/readyz` until all answer 200, the fleet
/// dies, or [`BOOT_DEADLINE`].
///
/// [`wait_ready`] gates only on the gateway's `/readyz`, which is a plain 200
/// (`without_db`) that says nothing about the two passthrough origins the
/// positive controls dial: admin-svc and accounts-svc are HTTP proxy TARGETS,
/// not `remote::Stub` peers, so no readiness probe on the gateway represents
/// them, and weles boots admin-svc LAST — after the gateway. Without this second
/// gate the `/admin/login` positive control fires before admin-svc has bound
/// its port and answers 502 (a race, not a resolve fault — the managed origin is
/// correct at steady state).
///
/// It asks each service's OWN port straight from weles's manifest (the same
/// authority the fleet listens on), so it is order-independent: it needs no
/// knowledge of which services are passthrough origins or of the boot order, and
/// a future manifest reorder cannot reopen the race. weles already health-gates
/// each service on its `/readyz` before booting the next, so once the fleet is up
/// every port answers 200 and this adds no real wait — only correctness.
///
/// Same rule as [`wait_ready`]: a weles that EXITED is its own fact, not a timeout.
fn wait_fleet_serving(
    fleet: &mut processctl::BorrowedChild<'_>,
    runtime: &tokio::runtime::Runtime,
    client: &reqwest::Client,
    services: &[weles::manifest::ServiceDef],
) -> Result<()> {
    let bases: Vec<String> = services
        .iter()
        .map(|svc| format!("http://127.0.0.1:{}", svc.http_port))
        .collect();
    let deadline = Instant::now() + BOOT_DEADLINE;
    loop {
        if fleet.try_wait()?.is_some() || runner::interrupted() {
            // The fleet is gone or the run was interrupted — stop waiting and let
            // the probes below report the real finding rather than sitting out the
            // whole budget.
            return Ok(());
        }
        let all_ready = bases
            .iter()
            .all(|base| matches!(get(runtime, client, &format!("{base}/readyz"), &[]), Ok(200)));
        if all_ready || Instant::now() >= deadline {
            return Ok(());
        }
        std::thread::sleep(PROBE_INTERVAL);
    }
}

/// [`wait_ready`]'s twin for the swap probe's own gateway: waits until its port
/// ANSWERS, whatever it answers.
///
/// NOT readiness — see [`SwapProbe::serving`]. This gateway has a sabotaged peer
/// by construction, so it is `/readyz` 503 forever by design; a 200 gate here
/// waits out its whole budget against a process that is up and serving.
///
/// Same rule as its twin: a process that EXITED is its own fact, never a timeout.
fn wait_serving(
    child: &mut OwnedChild,
    runtime: &tokio::runtime::Runtime,
    client: &reqwest::Client,
    base: &str,
    budget: Duration,
) -> Result<Probe> {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Err(format!(
                "the swap probe's gateway exited ({status:?}) before serving — see its logs"
            )));
        }
        if runner::interrupted() {
            return Ok(Err("interrupted before the swap gateway served".into()));
        }
        // Any answer at all: the question is "is this process serving HTTP",
        // and 503 is an answer.
        if let Ok(status) = get(runtime, client, &format!("{base}/readyz"), &[]) {
            return Ok(Ok(status));
        }
        if Instant::now() >= deadline {
            return Ok(Err(format!(
                "the swap probe's gateway did not answer on {base} within {budget:?}"
            )));
        }
        std::thread::sleep(PROBE_INTERVAL);
    }
}

/// A GET whose BODY is the answer — the marker probe. A non-2xx is an `Err`: the
/// marker must arrive in a served response, never in an error page.
fn body_of(
    runtime: &tokio::runtime::Runtime,
    client: &reqwest::Client,
    url: &str,
    headers: &[(&str, &str)],
) -> std::result::Result<String, String> {
    runtime
        .block_on(async {
            let mut request = client.get(url);
            for (name, value) in headers {
                request = request.header(*name, *value);
            }
            let response = request.send().await.map_err(|error| format!("{error}"))?;
            let status = response.status();
            let body = response.text().await.map_err(|error| format!("{error}"))?;
            if status.is_success() {
                Ok(body)
            } else {
                Err(format!("answered {status}"))
            }
        })
        .map_err(|error: String| format!("{url}: {error}"))
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
