//! B1 Step 1 extension: the ABRUPT-KILL variant of the peer-restart repro. The graceful
//! variant (`src/redial_tests.rs`) tears the peer down with `RunningServer::close()`,
//! which sends a QUIC CONNECTION_CLOSE — the client then observes a clean
//! `edge::Error::Connection` (`ConnectionFatal`) and the `Reconnecting` gate resets.
//! Weles' chaos run killed the svc HARD (TerminateProcess): no close frame is ever
//! sent, so the cached connection dies silently and the client only discovers it via a
//! failed `open_bi`/write/read or the 30s idle timeout — the path that could surface as
//! `edge::Error::Stream` (`StreamLocal`), which the gate NEVER resets (the suspected
//! pinning branch of the decision table in
//! `docs/plans/2026-07-15-1536-b1-stub-redial-after-peer-restart-plan.md`).
//!
//! Topology of the test: the peer is a CHILD PROCESS — this very test binary re-executed
//! in "helper mode" (`helper_peer_main` below, gated on `REDIAL_PEER_PORT`; a no-op pass
//! in normal runs). A child process is required because `Child::kill()` is
//! TerminateProcess on Windows — genuinely no close frame — and because rebinding the
//! same UDP port after a hard kill is exactly the production respawn shape. Cross-process
//! mTLS requires a SHARED anchor: the parent generates a `DevCA` once, writes it with
//! `DevCA::write_pem`, and sets `EDGE_CA_CERT`/`EDGE_CA_KEY` in its own environment
//! (inherited by the children) BEFORE the first `edge::shared_dev_ca()` use — which is
//! why this lives in an integration test (own process; the unit-test binary's other
//! tests memoize a generated anchor first) and why the crate exposes the
//! `#[doc(hidden)] remote::test_only_reconnecting_edge_caller` seam instead of the
//! crate-private `Reconnecting<EdgeDialer>` directly.
//!
//! Timing doctrine: real processes + real sockets ⇒ real clock; the two variants are
//! serialized by a static mutex WITHIN this binary (cargo runs test binaries one at a
//! time, so no cross-binary mutex is needed against `src/redial_tests.rs`); recovery is
//! asserted within a bounded 90s hang-guard (3x the 30s client idle timeout — the
//! detection window for a silently-dead connection), NEVER latency. Per-iteration
//! timing is recorded for diagnosis only. Diagnosis is built into the failure message:
//! a timeout panics with the full observed error/provenance sequence.
//!
//! KNOWN FLAKE CLASS (TOCTOU port steal): between helper A dying and helper B rebinding
//! the same port P, a CONCURRENT workspace test binary could grab P (ephemeral-range
//! UDP bind). The helper's bounded bind-retry loop is the mitigation; a
//! "helper peer never became ready" panic here is a rerun-the-test flake, NOT a product
//! bug.
//!
//! STEADY WALL COST: ~60s for this whole binary — two serialized variants, each waiting
//! out one ~30s idle-timeout detection window on the silently-dead connection (no close
//! frame ⇒ quinn only declares `ConnectionLost` at `CLIENT_IDLE_TIMEOUT_MS`). Paid once
//! per binary run regardless of test-thread parallelism (the variants serialize on the
//! mutex). Budgeted in the blocking `test` verify stage.

use std::io::{BufRead, BufReader, Write as _};
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use opsapi::RetryMode;

/// Serializes the two abrupt-kill variants (they share the parent process's env, the
/// spawned-children port space, and the one shared-CA init).
static SERIAL: Mutex<()> = Mutex::new(());

/// The bounded hang-guard: 3x `edge::client::CLIENT_IDLE_TIMEOUT_MS` (30s). Dead-conn
/// detection without a close frame takes at most the idle window; 3x is deliberate
/// headroom under full workspace parallelism.
const RECOVERY_DEADLINE: Duration = Duration::from_secs(90);

/// The readiness line the helper child prints once its edge server is bound.
const READY_PREFIX: &str = "REDIAL-PEER-READY ";

/// The temp dir holding the persisted shared CA, for best-effort cleanup once BOTH
/// variants have finished (see [`variant_done_cleanup_ca`]).
static CA_DIR: OnceLock<std::path::PathBuf> = OnceLock::new();

/// Count of completed variants; at 2, the CA temp dir is removed (best-effort — a
/// panicking variant skips cleanup, leaving one small dir in the OS temp dir).
static VARIANTS_DONE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Generates the shared CA once per parent process, persists it, and points
/// `EDGE_CA_CERT`/`EDGE_CA_KEY` at the files — so the parent's `shared_dev_ca()`
/// (used inside the crate-private `EdgeDialer`) and every spawned child resolve the
/// SAME anchor. Must run under [`SERIAL`], BEFORE the calling variant builds its tokio
/// runtime (see the variants below).
///
/// `set_var` SAFETY: every env access in this binary (this `set_var` and
/// `helper_peer_main`'s reads) happens under [`SERIAL`], and the variants call this
/// before creating their own runtime, so no unsynchronized getenv can race it here.
/// The residual assumption is Windows-shaped (Win32 env functions are internally
/// thread-safe; POSIX `setenv` after threads exist is the classic getenv data race) —
/// if the pending linux-clippy plan ports this test, keep the SERIAL-covers-all-env-
/// access invariant or move the env setup into a pre-main constructor.
fn ensure_shared_ca_env() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("b1-abrupt-redial-ca-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create CA temp dir");
        let cert = dir.join("edge-ca.pem");
        let key = dir.join("edge-ca.key");
        let ca = edge::DevCA::generate().expect("generate shared dev CA");
        ca.write_pem(cert.to_str().unwrap(), key.to_str().unwrap())
            .expect("persist shared dev CA");
        std::env::set_var("EDGE_CA_CERT", &cert);
        std::env::set_var("EDGE_CA_KEY", &key);
        let _ = CA_DIR.set(dir);
    });
}

/// Marks one variant complete; the second completion removes the CA temp dir
/// (best-effort: the files are only read at parent first-dial and child startup, and
/// no further children are spawned once both variants are done).
fn variant_done_cleanup_ca() {
    if VARIANTS_DONE.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == 2 {
        if let Some(dir) = CA_DIR.get() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// A spawned helper peer, killed HARD (TerminateProcess — no close frame) on drop or
/// via [`HelperPeer::kill_hard`].
struct HelperPeer(Child);

impl HelperPeer {
    fn kill_hard(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for HelperPeer {
    fn drop(&mut self) {
        self.kill_hard();
    }
}

/// Re-executes THIS test binary in helper mode: only `helper_peer_main` runs (libtest
/// `--exact` filter), with `REDIAL_PEER_PORT` set (`0` = ephemeral). Blocks (in
/// `spawn_blocking`) until the child prints its READY line, bounded by `wait`.
async fn spawn_helper(port: u16, wait: Duration) -> (HelperPeer, SocketAddr) {
    let exe = std::env::current_exe().expect("current test binary path");
    let mut child = Command::new(exe)
        .args(["helper_peer_main", "--exact", "--nocapture", "--test-threads=1"])
        .env("REDIAL_PEER_PORT", port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn helper peer process");
    let stdout = child.stdout.take().expect("piped child stdout");
    let mut peer = HelperPeer(child);

    let ready = tokio::time::timeout(
        wait,
        tokio::task::spawn_blocking(move || {
            // Scan for the READY marker ANYWHERE in the line: libtest prints
            // `test helper_peer_main ... ` WITHOUT a newline before the test body
            // runs, so the marker lands mid-line, never at line start.
            for line in BufReader::new(stdout).lines() {
                let line = line.ok()?;
                if let Some(at) = line.find(READY_PREFIX) {
                    let rest = &line[at + READY_PREFIX.len()..];
                    // The addr is the next whitespace-delimited token (libtest may
                    // append more to the same line later, e.g. the test verdict).
                    let token = rest.split_whitespace().next()?;
                    return token.parse::<SocketAddr>().ok();
                }
            }
            None
        }),
    )
    .await;
    match ready {
        Ok(Ok(Some(addr))) => (peer, addr),
        other => {
            peer.kill_hard();
            panic!("helper peer never became ready (port {port}): {other:?}");
        }
    }
}

/// Same inference the graceful variant uses: maps the `opsapi::Error` message back to
/// the provenance partition of `map_edge_call_failure` (the mapped message preserves
/// the `edge::Error` variant prefix). Duplicated from `src/redial_tests.rs` because an
/// integration test is a separate crate from the unit-test module.
fn infer_provenance(status: opsapi::Status, msg: &str) -> &'static str {
    if msg.contains("edge: connection:") {
        "ConnectionFatal(Connection)"
    } else if msg.contains("edge: remote error:") {
        "PeerAnswer(Remote)"
    } else if status == opsapi::Status::NotFound {
        "PeerAnswer(UnknownMethod)"
    } else if msg.contains("edge: stream:") {
        "StreamLocal(Stream)"
    } else if msg.contains("edge: connect:") {
        "StreamLocal(Connect)"
    } else if msg.contains("edge: tls:") {
        "StreamLocal(Tls)"
    } else {
        "StreamLocal(other)"
    }
}

/// The shared abrupt-kill repro. Returns the observed per-iteration error sequence
/// (each entry stamped with elapsed-since-kill); panics with the full sequence if
/// recovery does not happen within [`RECOVERY_DEADLINE`]. Also PINS the diagnosis:
/// under `RetryMode::Never` recovery is only possible through a `ConnectionFatal`
/// reset (the gate's sole reset trigger), so the observed sequence must contain one —
/// a recovery via any other classification would mean the diagnosis (no StreamLocal
/// pinning; detection lands ConnectionFatal) no longer holds. The caller must have run
/// [`ensure_shared_ca_env`] first (under [`SERIAL`], before building its runtime).
async fn run_abrupt_kill_repro(mode: RetryMode) -> Vec<String> {
    // Helper A on an ephemeral port; capture the concrete port P for the respawn.
    let (mut peer_a, addr) = spawn_helper(0, Duration::from_secs(30)).await;
    let port = addr.port();

    // Prime the crate-private Reconnecting<EdgeDialer> cache with a live call —
    // without this the kill leaves an empty cache and the post-respawn call would
    // trivially dial fresh, never exercising the dead-cached-conn branch.
    let caller = remote::test_only_reconnecting_edge_caller(&addr.to_string());
    let primed = caller
        .call("echo", None, b"{}", mode)
        .await
        .expect("priming call to live helper A must succeed");
    assert_eq!(primed, b"{}", "echo must return the payload verbatim");

    // HARD kill: TerminateProcess — no CONNECTION_CLOSE frame reaches the client.
    peer_a.kill_hard();
    let killed_at = Instant::now();

    // Respawn on the SAME port P (the helper retries the bind internally — Windows can
    // transiently refuse the UDP rebind right after the kill).
    let (mut peer_b, addr_b) = spawn_helper(port, Duration::from_secs(30)).await;
    assert_eq!(addr_b.port(), port, "helper B must rebind the same port");

    // Loop until the reconnecting caller heals onto B, recording every failure with
    // its elapsed-since-kill stamp. Bounded deadline; never a latency assertion.
    let mut observations: Vec<String> = Vec::new();
    let deadline = killed_at + RECOVERY_DEADLINE;
    let recovered_after;
    loop {
        match caller.call("echo", None, b"{}", mode).await {
            Ok(v) => {
                assert_eq!(v, b"{}", "recovered call must echo the payload verbatim");
                recovered_after = killed_at.elapsed();
                break;
            }
            Err(e) => {
                observations.push(format!(
                    "iter {} (+{:.1}s after kill): status={:?} inferred_provenance={} msg={:?}",
                    observations.len(),
                    killed_at.elapsed().as_secs_f64(),
                    e.status,
                    infer_provenance(e.status, &e.msg),
                    e.msg,
                ));
                if Instant::now() >= deadline {
                    peer_b.kill_hard();
                    panic!(
                        "reconnecting caller did NOT recover after ABRUPT peer kill within \
                         {:?} (mode {:?}); observed error sequence:\n{}",
                        RECOVERY_DEADLINE,
                        mode,
                        observations.join("\n"),
                    );
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }

    eprintln!(
        "[B1-ABRUPT {mode:?}] recovered {:.1}s after kill, {} failed iteration(s):\n{}",
        recovered_after.as_secs_f64(),
        observations.len(),
        if observations.is_empty() {
            "<recovered on the first post-kill call>".to_string()
        } else {
            observations.join("\n")
        }
    );

    // Pin the diagnosed mechanism, not just the outcome. Under `Never` the gate resets
    // ONLY on ConnectionFatal, so a recovery must have observed one; under
    // `OnceAfterReconnect` the fatal may be absorbed INSIDE the healing call (reset +
    // replay), so an empty sequence is the expected shape — but any observed failures
    // must still include the fatal that unpinned the cache. (No latency asserted.)
    if !(mode == RetryMode::OnceAfterReconnect && observations.is_empty()) {
        assert!(
            observations.iter().any(|o| o.contains("ConnectionFatal")),
            "recovery without an observed ConnectionFatal contradicts the pinned \
             diagnosis (only ConnectionFatal resets the cached conn); observed:\n{}",
            observations.join("\n"),
        );
    }

    peer_b.kill_hard();
    observations
}

/// A fresh current-thread runtime per variant, built AFTER `ensure_shared_ca_env` so
/// the `set_var` happens before this test's runtime (and its blocking threads) exist.
fn variant_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build variant runtime")
}

/// Abrupt-kill repro, `RetryMode::Never` (the per-call mutating path — B1's
/// `Ownership::owner_of` shape, no automatic replay). A plain `#[test]`, not
/// `#[tokio::test]`: the env setup must precede the runtime (see
/// [`ensure_shared_ca_env`]'s safety note).
#[test]
fn abrupt_kill_redials_never() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    ensure_shared_ca_env();
    variant_runtime().block_on(run_abrupt_kill_repro(RetryMode::Never));
    variant_done_cleanup_ca();
}

/// Abrupt-kill repro, `RetryMode::OnceAfterReconnect` (the `#[retry_safe]` read path —
/// one automatic replay after a proven connection-fatal failure).
#[test]
fn abrupt_kill_redials_once_after_reconnect() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    ensure_shared_ca_env();
    variant_runtime().block_on(run_abrupt_kill_repro(RetryMode::OnceAfterReconnect));
    variant_done_cleanup_ca();
}

/// HELPER MODE — not a real test in normal runs (no `REDIAL_PEER_PORT` ⇒ immediate
/// pass). When the parent re-executes this binary with the env set, this boots a real
/// `edge::Server` with one echo method on the given port (0 = ephemeral; a concrete
/// port is bind-retried, since a hard-killed predecessor's UDP socket can linger
/// briefly on Windows), prints the READY line, and blocks until the parent kills it
/// hard. It inherits `EDGE_CA_CERT`/`EDGE_CA_KEY` from the parent, so its
/// `shared_dev_ca()` resolves the SAME anchor the parent's dialer uses.
#[tokio::test]
async fn helper_peer_main() {
    // Read the env under a SHORT-LIVED serial lock (serialized against the variants'
    // `set_var` in a normal run), then RELEASE it: in helper mode this test blocks
    // forever below, and holding the mutex across that would deadlock the binary if
    // `REDIAL_PEER_PORT` ever leaked into a normal run's environment.
    let port = {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        std::env::var("REDIAL_PEER_PORT").ok()
    };
    let Some(port) = port else {
        return; // normal test run — nothing to do
    };
    let port: u16 = port.parse().expect("REDIAL_PEER_PORT must be a u16");
    let ca = edge::shared_dev_ca().expect("helper: shared CA from inherited env");

    let mut running = None;
    for _ in 0..100u32 {
        let mut srv = edge::Server::new();
        srv.handle(
            "echo",
            std::sync::Arc::new(|payload: Vec<u8>| Box::pin(async move { Ok(payload) })),
        );
        match srv.listen(SocketAddr::from(([127, 0, 0, 1], port)), &ca) {
            Ok(r) => {
                running = Some(r);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    let running = running.expect("helper: could not bind edge server after retries");

    println!("{}{}", READY_PREFIX, running.local_addr());
    std::io::stdout().flush().expect("flush READY line");

    // Serve until the parent terminates this process hard.
    std::future::pending::<()>().await;
}
