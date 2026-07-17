//! Lifecycle tests for the async island. This step ships no verbs, so every
//! test here is about the thing that is actually new and actually risky: a
//! tokio runtime living on its own thread beside synchronous supervisor code.
//!
//! Timing doctrine (`memory/timing-sensitive-tests-doctrine.md`): nothing here
//! sleeps as synchronization or races a real clock. Concurrency is proven with
//! channel handshakes (a happens-before), and every "is it bounded?" claim is a
//! hang-guard with generous headroom — a budget that fails only on a HANG, never
//! on a slow machine.

use super::*;

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Budget for an operation that must be BOUNDED. Deliberately far above any
/// real duration (SHUTDOWN_GRACE is 2s): this can only fire on a true hang, so
/// it never flakes on a loaded box. Not a performance assertion.
const HANG_BUDGET: Duration = Duration::from_secs(30);

/// Serializes tests that read [`RUNTIME_THREADS`], which is process-global: a
/// concurrent test holding a live agent would otherwise make a
/// "no thread leaked" assertion flake. Same `OnceLock<Mutex<()>>` shape as
/// `prep_tests::env_guard` / `supervisor_tests::stop_guard` — copied with
/// provenance, not imported (zero-sharing). Poison-tolerant: a panicking
/// guarded test must not wedge the rest.
fn agent_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn live_runtime_threads() -> usize {
    RUNTIME_THREADS.load(Ordering::SeqCst)
}

/// One bounded raw-TCP HTTP/1.1 request, returning `(status code, body)`. Hand
/// rolled in the same spirit as `health::probe` — the agent's own client is not
/// this step's subject, and a test client that shares the server's stack could
/// hide a wire bug.
fn request(port: u16, method: &str, path: &str) -> (u16, String) {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, HANG_BUDGET).expect("connect to the agent");
    stream.set_read_timeout(Some(HANG_BUDGET)).expect("set read timeout");
    stream.set_write_timeout(Some(HANG_BUDGET)).expect("set write timeout");
    stream
        .write_all(
            format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .expect("write request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("no status code in response: {response:?}"));
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    (status, body)
}

/// Runs `body` on a helper thread and fails if it does not finish within
/// [`HANG_BUDGET`]. This is how a "bounded" claim is proven without a clock
/// race: the assertion is liveness (it finished), not latency (how fast).
fn within_budget<T: Send + 'static>(what: &str, body: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(body());
    });
    rx.recv_timeout(HANG_BUDGET)
        .unwrap_or_else(|_| panic!("{what} did not finish within {HANG_BUDGET:?} — it HANGS"))
}

// ---------------------------------------------------------------------------
// Bind
// ---------------------------------------------------------------------------

#[test]
fn bind_on_a_taken_port_fails_without_leaking_the_runtime_thread() {
    let _guard = agent_guard();
    assert_eq!(live_runtime_threads(), 0, "a previous test leaked a runtime thread");

    // A real listener on the port: the agent's bind must lose against it.
    let blocker = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind the blocking listener");
    let port = blocker.local_addr().expect("blocker port").port();

    // Bounded: a bind failure must never be a silent hang — that is the whole
    // point of copying ControlServer's three-armed ready handshake. A missing
    // `Ok(Err(_))` arm would land here on the BIND_DEADLINE arm instead, so
    // this budget is also what keeps the timeout arm from passing as "fine".
    let error = within_budget("AgentServer::bind on a taken port", move || {
        AgentServer::bind(port).expect_err("bind must fail when the port is taken")
    });
    let message = format!("{error:#}");
    assert!(
        message.contains("bind agent endpoint") && message.contains(&port.to_string()),
        "a bind failure must name the endpoint it could not bind, got: {message}"
    );

    // The failing-branch proof: `bind` joined the runtime thread before
    // returning Err. Without the join (or with a detached thread), the counter
    // would still be 1 — a leaked runtime is invisible any other way.
    assert_eq!(
        live_runtime_threads(),
        0,
        "an Err from bind must leave NO runtime thread behind"
    );
    drop(blocker);
}

#[test]
fn bind_reports_the_address_it_actually_listens_on() {
    let _guard = agent_guard();
    // Port 0 = let the OS pick, so parallel tests never contend on a port.
    let agent = AgentServer::bind(0).expect("bind the agent on an ephemeral port");
    assert_ne!(agent.addr().port(), 0, "addr() must report the RESOLVED port, not the request");
    assert!(agent.addr().ip().is_loopback(), "the agent binds loopback only");
    assert!(!agent.dead(), "a freshly bound agent is not dead");
    assert_eq!(live_runtime_threads(), 1, "a live agent owns exactly one runtime thread");
}

// ---------------------------------------------------------------------------
// Drop: cancellation reaches an accept parked on `.await`
// ---------------------------------------------------------------------------

#[test]
fn drop_stops_and_joins_the_runtime_thread_within_a_bounded_budget() {
    let _guard = agent_guard();
    assert_eq!(live_runtime_threads(), 0, "a previous test leaked a runtime thread");
    let agent = AgentServer::bind(0).expect("bind the agent");
    let port = agent.addr().port();
    assert_eq!(live_runtime_threads(), 1);

    // THE regression this file exists for: the accept loop is parked on
    // `.await`, where an AtomicBool + poll-sleep (ControlServer::drop's shape)
    // never arrives. A verbatim copy of that Drop hangs here forever, and this
    // budget is what catches it.
    within_budget("AgentServer::drop", move || drop(agent));

    assert_eq!(
        live_runtime_threads(),
        0,
        "drop must JOIN the runtime thread, not merely signal it"
    );
    // And the join really means the listener is gone: the port is free again.
    let rebind = std::net::TcpListener::bind(("127.0.0.1", port));
    assert!(rebind.is_ok(), "the agent's port must be released once dropped: {rebind:?}");
}

#[test]
fn dropping_the_agent_does_not_stall_the_rollout_lock_release() {
    let _guard = agent_guard();
    let root = std::env::temp_dir().join(format!("weles-agent-lock-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create test temp dir");

    // The real ordering from `run_up`: the lock is acquired first and dropped
    // LAST, strictly after the agent. If `Runtime::drop` ran on the supervisor
    // thread (rather than the runtime's own), it would block right here — and
    // the lock would still be held.
    let lock = crate::lock::acquire(&root, "agent-lock-test").expect("acquire the rollout lock");
    let agent = AgentServer::bind(0).expect("bind the agent");
    within_budget("drop(agent) then drop(lock)", move || {
        drop(agent);
        drop(lock);
    });

    // Proof the lock was actually RELEASED, not just that drop returned: a
    // fresh acquire succeeds only against a free lock.
    let reacquired = crate::lock::acquire(&root, "after-agent");
    assert!(
        reacquired.is_ok(),
        "the rollout lock must be free once the agent is dropped: {reacquired:?}"
    );
    drop(reacquired);
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// The ordering claim: the agent serves while the supervisor thread is busy
// ---------------------------------------------------------------------------

#[test]
fn healthz_answers_while_the_supervisor_thread_is_blocked_in_sequential_work() {
    let _guard = agent_guard();
    let agent = AgentServer::bind(0).expect("bind the agent");
    let port = agent.addr().port();

    // This is the real content of "bound BEFORE boot": `boot` is a sequential,
    // readyz-gated loop that OWNS the supervisor thread for its whole duration.
    // A service can only reach readyz by talking to the agent DURING that loop,
    // so the agent's I/O must be independent of this thread making progress.
    //
    // The supervisor thread stands in for `boot` by blocking on a channel recv
    // — a real blocking call, not a sleep — and it does not unblock until the
    // client has already been answered. So a 200 here cannot be explained by
    // the "boot" work having finished first; the happens-before is structural.
    let (answered_tx, answered_rx) = std::sync::mpsc::channel();
    let client = std::thread::spawn(move || {
        let result = request(port, "GET", "/healthz");
        answered_tx.send(()).expect("report the answer");
        result
    });

    answered_rx
        .recv_timeout(HANG_BUDGET)
        .expect("/healthz must answer while the supervisor thread is blocked in sequential work");
    let (status, body) = client.join().expect("client thread");
    assert_eq!(status, 200, "/healthz is the one route this step serves");
    assert_eq!(body, "ok\n");
    assert!(!agent.dead(), "serving a request must not kill the endpoint");
}

#[test]
fn there_are_no_verbs_yet_every_other_path_is_a_404() {
    let _guard = agent_guard();
    let agent = AgentServer::bind(0).expect("bind the agent");
    let port = agent.addr().port();

    // Step 2a ships the lifecycle, NOT the contract. `resolve`/`hello` are
    // Step 2b; until then they must 404 rather than answer something invented.
    for path in ["/resolve", "/hello", "/", "/healthz/x"] {
        let (status, _) = request(port, "GET", path);
        assert_eq!(status, 404, "{path} must 404 — this step serves no verbs");
    }
    // Route matching is on (method, path): /healthz is a GET.
    let (status, _) = request(port, "POST", "/healthz");
    assert_eq!(status, 404, "POST /healthz must not match the GET route");
    // The one route still works afterwards — a 404 is not a poison pill.
    assert_eq!(request(port, "GET", "/healthz").0, 200);
}

// ---------------------------------------------------------------------------
// The tokio feature ban — MECHANICAL, because a comment is not a guard
// ---------------------------------------------------------------------------

/// Runs `cargo tree` and returns its stdout, failing loudly on a non-zero exit
/// (a checker that reports green because its own tooling broke is the failure
/// class this repo has a scar from).
fn cargo_tree(args: &[&str]) -> String {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let output = std::process::Command::new(cargo)
        .arg("tree")
        .args(args)
        .arg("--manifest-path")
        .arg(&manifest)
        .output()
        .expect("run cargo tree");
    assert!(
        output.status.success(),
        "cargo tree {args:?} failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("cargo tree output is UTF-8")
}

#[test]
fn weles_tokio_never_gets_the_process_or_signal_feature() {
    // `process` installs a SIGCHLD handler that reaps children out from under
    // platform::OwnedProc::try_wait, destroying Observed::Exited — the sole
    // authority for "the process is gone" — fleet-wide. `signal` would fight
    // the raw libc::signal(SIGINT) handler in supervisor.rs; last writer wins.
    //
    // This is a TEST and not a comment in Cargo.toml because resolver-2 unifies
    // features across the build graph: weles/Cargo.toml is not the authority on
    // what weles's tokio actually resolves to. `--target all` so a Windows run
    // still covers the Unix-only SIGCHLD mine.
    let tree = cargo_tree(&["-e", "features", "-p", "weles", "--target", "all"]);

    // Fail-proof for the test ITSELF: if cargo's rendering ever changes, the
    // bans below would silently match nothing and go green forever. `net` is a
    // feature we KNOW is enabled, so this line proves the pattern shape is live.
    assert!(
        tree.contains(r#"tokio feature "net""#),
        "cargo tree's feature rendering changed — the bans below are no longer \
         checking anything. Output:\n{tree}"
    );
    for banned in ["process", "signal"] {
        assert!(
            !tree.contains(&format!(r#"tokio feature "{banned}""#)),
            "weles's tokio resolved WITH the banned `{banned}` feature — the async island \
             may never own process or signal handling (see agentapi's module doc)"
        );
    }
}

#[test]
fn no_workspace_crate_arms_the_tokio_process_feature() {
    // The scope the previous test cannot reach: `cargo tree -p weles` resolves
    // features for weles's own selection, so it would NOT see a sibling crate
    // enabling `process` — yet `cargo build --workspace` unifies features
    // across selected packages, which is exactly how that mine gets armed
    // inside the weles binary. So the `process` ban is checked graph-wide.
    //
    // KNOWN, DELIBERATE ASYMMETRY: `signal` cannot be banned graph-wide —
    // core/app legitimately owns it (core/app/Cargo.toml, tokio::signal for
    // graceful shutdown). It is banned for weles's own resolve (above) only.
    let tree = cargo_tree(&["-e", "features", "--workspace", "--target", "all", "-i", "tokio"]);

    // Fail-proof + the asymmetry's positive control in ONE assertion: core/app's
    // `signal` MUST be visible here. If it is not, either the rendering changed
    // or the inverted tree stopped covering the workspace — and the `process`
    // ban below would be checking nothing.
    assert!(
        tree.contains(r#"tokio feature "signal""#),
        "expected core/app's tokio `signal` feature in the workspace-wide tree — the \
         `process` ban below is no longer checking anything. Output:\n{tree}"
    );
    assert!(
        !tree.contains(r#"tokio feature "process""#),
        "a workspace crate enabled tokio's `process` feature. Resolver-2 unifies features \
         across the build graph, so this arms the SIGCHLD mine inside the weles binary too: \
         tokio::process reaps children out from under platform::OwnedProc::try_wait, which \
         is the sole authority for Observed::Exited. Remove it or move that crate's \
         subprocess work off tokio::process."
    );
}
