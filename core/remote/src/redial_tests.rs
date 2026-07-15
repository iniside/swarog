//! B1 repro (Step 1): stub module→module re-dial after a peer restart, exercised at
//! the REAL connection layer through the crate-private `Reconnecting<EdgeDialer>` path
//! (same crate, so the private seam is reachable — no fake transport here; the
//! fake-transport policy tests live in `tests.rs`).
//!
//! The scenario mirrors what Weles' chaos run hit: a consumer process holds ONE
//! `Reconnecting` connection to a provider (`inventory` → `characters`). The provider
//! svc is killed and re-spawned on the SAME port. The question the decision table in
//! `docs/plans/2026-07-15-1536-b1-stub-redial-after-peer-restart-plan.md` asks — and
//! this test answers empirically — is whether the reconnecting caller heals on its own
//! once the cached connection is dead, or pins the corpse forever.
//!
//! Diagnosis is BUILT INTO the test, not printf'd ad-hoc: every failed loop iteration
//! records the mapped `opsapi::Error` (status + message, whose text carries the edge
//! variant prefix) plus the provenance INFERRED from that prefix by the same partition
//! `map_edge_call_failure` uses. On a hang-guard timeout the panic message lists the
//! full observed sequence, so a red run names the decision-table branch by itself.
//!
//! Timing doctrine: real sockets ⇒ real clock. `--test-threads` is per-BINARY, not
//! per-file, so the two variants below (which both bind + rebind a concrete port and
//! drive real QUIC) are serialized by a static mutex. Recovery is asserted within a
//! bounded 90s hang-guard (3× `edge::client::CLIENT_IDLE_TIMEOUT_MS`, deliberate
//! headroom under full workspace parallelism) — never a latency assertion.
//!
//! KNOWN FLAKE CLASS (TOCTOU port steal): between server A releasing port P and server
//! B rebinding it, a CONCURRENT workspace test binary could grab P (ephemeral-range UDP
//! bind). The bounded bind-retry in `boot_echo_server` is the mitigation; a bind panic
//! there is a rerun-the-test flake, NOT a product bug.
//!
//! STEADY WALL COST: sub-second. The graceful `close()` sends a CONNECTION_CLOSE, so
//! the client detects the dead connection on its next call immediately — no 30s
//! idle-timeout window is paid here (that cost lives in the abrupt-kill variant,
//! `tests/abrupt_kill_redial.rs`, ~60s per binary run).

use super::*;
use std::net::SocketAddr;
use std::sync::Mutex as StdSyncMutex;
use std::time::{Duration, Instant};

/// Serializes the two real-edge variants: each binds a concrete loopback port, tears
/// the server down, and rebinds the SAME port, so they must never overlap. `--test-threads`
/// is per-binary, so a static mutex is the file-scoped serialization the doctrine calls for.
/// Poison-tolerant: a red (panicking) variant must not wedge the other.
static REDIAL_SERIAL: StdSyncMutex<()> = StdSyncMutex::new(());

/// The bounded hang-guard: 3× `edge::client::CLIENT_IDLE_TIMEOUT_MS` (30s) = 90s. The
/// dead-connection detection window is at most the idle timeout; 3× is deliberate
/// headroom for a fully-parallel workspace run (2× is too thin per the timing doctrine).
const RECOVERY_DEADLINE: Duration = Duration::from_secs(90);

/// Builds an echo server on `addr`, retrying the bind briefly. Windows can transiently
/// refuse to rebind a UDP port immediately after the previous endpoint released it
/// (the accept task's endpoint clone drops slightly after `close()`), so the rebind of
/// the SAME port is wrapped in a short retry loop. `listen` consumes the `Server`, so a
/// fresh one is built per attempt.
async fn boot_echo_server(ca: &edge::DevCA, addr: SocketAddr) -> edge::RunningServer {
    for attempt in 0..50u32 {
        let mut srv = edge::Server::new();
        // A trivial echo: the response bytes are the request payload verbatim (must be
        // valid JSON — `{}` is — so the server's `ok_response` accepts it).
        srv.handle(
            "echo",
            std::sync::Arc::new(|payload: Vec<u8>| Box::pin(async move { Ok(payload) })),
        );
        match srv.listen(addr, ca) {
            Ok(running) => return running,
            Err(e) => {
                assert!(
                    attempt < 49,
                    "could not bind echo server on {addr} after {} attempts: {e}",
                    attempt + 1
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    unreachable!("bind retry loop returns or asserts")
}

/// Classifies a mapped `opsapi::Error` message back into the `FailureProvenance` the
/// crate-private gate keyed on, by the SAME prefix partition `map_edge_call_failure`
/// uses (`Connection` → ConnectionFatal; `Remote`/NotFound → PeerAnswer; everything
/// else — `Stream`/`Connect`/`Tls`/`Io`/`Codec`/`FrameTooLarge` — → StreamLocal). This
/// is derived from the mapped message (the only thing the `Caller` seam surfaces), so
/// it is labelled "inferred" in the diagnosis — the raw message is the ground truth.
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

/// Runs the shared B1 repro for one retry mode. Returns the observed error sequence
/// (empty iff the very first post-respawn call succeeded) after asserting recovery
/// within the bounded deadline; panics with the full sequence if the deadline elapses.
async fn run_redial_repro(mode: RetryMode) -> Vec<String> {
    let ca = edge::shared_dev_ca().expect("shared dev CA");

    // Server A on an ephemeral loopback port; capture the concrete port P for the rebind.
    let running_a = boot_echo_server(&ca, SocketAddr::from(([127, 0, 0, 1], 0))).await;
    let port = running_a.local_addr().port();
    let peer = running_a.local_addr().to_string();

    // Dial through the REAL crate-private reconnecting caller and prime its cache with a
    // successful call. Without this live call the cache is empty, the kill leaves nothing
    // stale, and the post-respawn call would trivially dial fresh — never exercising the
    // risky "dead cached connection" branch B1 is about.
    let reconnecting = Reconnecting::new(EdgeDialer { peer });
    let primed = reconnecting
        .call("echo", None, b"{}", mode)
        .await
        .expect("priming call to live server A must succeed");
    assert_eq!(primed, b"{}", "echo must return the payload verbatim");

    // Kill server A: close() unblocks the accept loop (its endpoint clone then drops),
    // freeing port P; drop the handle for good measure. The reconnecting caller still
    // holds the now-dead connection to A cached.
    running_a.close();
    drop(running_a);

    // Re-spawn server B on the SAME port P (bind-retry inside).
    let running_b = boot_echo_server(&ca, SocketAddr::from(([127, 0, 0, 1], port))).await;

    // Loop until the reconnecting caller heals onto B, recording every failure. The
    // assertion is "eventually recovers within the bounded deadline", never latency.
    let mut observations: Vec<String> = Vec::new();
    let deadline = Instant::now() + RECOVERY_DEADLINE;
    loop {
        match reconnecting.call("echo", None, b"{}", mode).await {
            Ok(v) => {
                assert_eq!(v, b"{}", "recovered call must echo the payload verbatim");
                break;
            }
            Err(e) => {
                observations.push(format!(
                    "iter {}: status={:?} inferred_provenance={} msg={:?}",
                    observations.len(),
                    e.status,
                    infer_provenance(e.status, &e.msg),
                    e.msg,
                ));
                if Instant::now() >= deadline {
                    running_b.close();
                    panic!(
                        "reconnecting caller did NOT recover after peer restart within {:?} \
                         (mode {:?}); observed error sequence:\n{}",
                        RECOVERY_DEADLINE,
                        mode,
                        observations.join("\n"),
                    );
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }

    running_b.close();
    observations
}

/// A fresh current-thread runtime per variant: the serial guard is a std mutex, so it
/// must never be held across an `.await` (clippy `await_holding_lock`); holding it
/// across a `block_on` has no await points and serializes the variants correctly.
fn variant_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build variant runtime")
}

/// B1 repro with `RetryMode::Never` (the mutating-method path — the one B1's
/// `Ownership::owner_of` consumer effectively hits per call, no automatic replay).
#[test]
fn stub_redials_after_peer_restart_never() {
    let _serial = REDIAL_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let observed = variant_runtime().block_on(run_redial_repro(RetryMode::Never));
    // Surface the observed sequence for the branch report (visible under --nocapture);
    // recovery itself is asserted inside `run_redial_repro`.
    eprintln!(
        "[B1-REPRO Never] recovered after {} failed iteration(s):\n{}",
        observed.len(),
        if observed.is_empty() {
            "<recovered on the first post-respawn call>".to_string()
        } else {
            observed.join("\n")
        }
    );
}

/// B1 repro with `RetryMode::OnceAfterReconnect` (the `#[retry_safe]` read path — one
/// automatic replay after a proven connection-fatal failure).
#[test]
fn stub_redials_after_peer_restart_once_after_reconnect() {
    let _serial = REDIAL_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let observed = variant_runtime().block_on(run_redial_repro(RetryMode::OnceAfterReconnect));
    eprintln!(
        "[B1-REPRO OnceAfterReconnect] recovered after {} failed iteration(s):\n{}",
        observed.len(),
        if observed.is_empty() {
            "<recovered on the first post-respawn call>".to_string()
        } else {
            observed.join("\n")
        }
    );
}
