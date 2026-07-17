//! Tests for the async island: its LIFECYCLE (a tokio runtime living on its own
//! thread beside synchronous supervisor code) and its two VERBS.
//!
//! The verb tests are all about the same question — does `resolve` answer from
//! the fleet manifest's one address authority, and does it refuse to answer when
//! it has no answer? Every one of them drives the real server over real TCP with
//! a hand-rolled client, because a client sharing the server's stack could hide a
//! wire bug, and the wire SHAPE is this milestone's entire deliverable.
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

use crate::manifest::{compose_env_with_fleet, monolith, split_fleet, RuntimeInputs};

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

/// An agent serving the real SPLIT fleet's addresses — the topology that
/// actually has peers to resolve.
fn split_agent() -> AgentServer {
    AgentServer::bind(0, PeerAddrs::from_fleet(&split_fleet())).expect("bind the agent")
}

/// One bounded raw-TCP HTTP/1.1 request, returning `(status code, body)`. Hand
/// rolled in the same spirit as `health::probe` — the agent's own client is not
/// this step's subject, and a test client that shares the server's stack could
/// hide a wire bug.
fn request(port: u16, method: &str, path: &str) -> (u16, String) {
    request_with_body(port, method, path, "")
}

/// [`request`] with a body — the shape both verbs take.
fn request_with_body(port: u16, method: &str, path: &str, body: &str) -> (u16, String) {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, HANG_BUDGET).expect("connect to the agent");
    stream.set_read_timeout(Some(HANG_BUDGET)).expect("set read timeout");
    stream.set_write_timeout(Some(HANG_BUDGET)).expect("set write timeout");
    stream
        .write_all(
            format!(
                "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
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
        AgentServer::bind(port, PeerAddrs::from_fleet(&split_fleet()))
            .expect_err("bind must fail when the port is taken")
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
    let agent = split_agent();
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
    let agent = split_agent();
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
    let agent = split_agent();
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
    let agent = split_agent();
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
fn routing_is_on_method_and_path_and_anything_unmatched_is_a_404() {
    let _guard = agent_guard();
    let agent = split_agent();
    let port = agent.addr().port();

    // The route table is exactly three entries. An unknown path is a 404 — the
    // agent never redirects, never prefix-matches, never guesses.
    for path in ["/", "/healthz/x", "/resolv", "/resolve/characters"] {
        let (status, _) = request(port, "GET", path);
        assert_eq!(status, 404, "{path} must 404 — it is not a route");
    }
    // Matching is on the PAIR: the verbs are POSTs and /healthz is a GET, so
    // each is a 404 under the other's method. A path-only match would answer
    // here — and would answer `resolve` to a GET that carries no question.
    assert_eq!(request(port, "POST", "/healthz").0, 404, "POST /healthz is not the GET route");
    for path in ["/resolve", "/hello"] {
        assert_eq!(request(port, "GET", path).0, 404, "GET {path} is not the POST verb");
    }
    // The routes still work afterwards — a 404 is not a poison pill.
    assert_eq!(request(port, "GET", "/healthz").0, 200);
}

// ---------------------------------------------------------------------------
// The accept loop's recovery arm
// ---------------------------------------------------------------------------

/// Injected accept failures. Deliberately far ABOVE the 64-consecutive-error
/// count this loop used to give up on: the point is that a burst which would
/// have deleted the endpoint for the rest of the run is now just noise.
const FAULT_BURST: usize = 200;

#[test]
fn the_accept_loop_recovers_from_a_burst_of_accept_failures() {
    let _guard = agent_guard();
    // A burst of accept() failures is an ambient transient — fd pressure while
    // weles spawns a 12-service fleet with stdio pipes, which is exactly what
    // happens right after this endpoint binds. It clears in milliseconds, so
    // the endpoint must RECOVER, never give up: an agent deleted for the rest
    // of the run is a far worse outcome than a few retried accepts.
    //
    // The delay is injected (1ms, not the real 1s) so this test does not sit
    // through 200 real seconds; the loop under test is otherwise the production
    // one, sleep and all. The delay's VALUE is not the invariant — recovery is.
    let agent = AgentServer::bind_faulty(
        0,
        PeerAddrs::from_fleet(&split_fleet()),
        FAULT_BURST,
        Duration::from_millis(1),
    )
    .expect("bind the agent");
    let port = agent.addr().port();

    // The endpoint still serves AFTER the burst. This is the assertion that a
    // count-based give-up rule cannot pass: it would have bailed at 64, killed
    // the runtime thread, and left this connect refused.
    let (status, body) = request(port, "GET", "/healthz");
    assert_eq!(status, 200, "the endpoint must survive {FAULT_BURST} accept failures");
    assert_eq!(body, "ok\n");
    assert!(!agent.dead(), "a burst of transient accept failures must not kill the endpoint");
}

#[test]
fn a_clean_stop_is_not_reported_as_a_death() {
    let _guard = agent_guard();
    let agent = split_agent();
    assert!(!agent.dead(), "a live endpoint is not dead");
    // The runtime thread ENDING is what arms the death flag, so the clean-stop
    // path must disarm it — otherwise every ordinary teardown would report a
    // dead endpoint. (The panic path, which cannot be provoked without a fault
    // injector inside the runtime, is covered by the same RAII guard: it is
    // armed by default and only `Ok(())` disarms it.)
    within_budget("AgentServer::drop", move || drop(agent));
}

#[test]
fn the_death_flag_arms_on_any_thread_exit_and_only_a_clean_stop_disarms() {
    // The authority for "the endpoint is gone" is the thread ENDING, not one
    // particular way of ending. Armed by default:
    let errored = Arc::new(AtomicBool::new(false));
    drop(DeathFlag::new(Arc::clone(&errored)));
    assert!(errored.load(Ordering::SeqCst), "an Err exit must report the endpoint dead");

    // Only an explicit clean stop disarms:
    let stopped = Arc::new(AtomicBool::new(false));
    let mut flag = DeathFlag::new(Arc::clone(&stopped));
    flag.disarm();
    drop(flag);
    assert!(!stopped.load(Ordering::SeqCst), "a clean stop is not a death");

    // THE branch that a `thread_dead.store(true)` inside an `if let Err(...)`
    // arm misses: a panic unwinds straight past that arm, so the thread is
    // gone, the port is released, join()'s Err is swallowed — and dead() would
    // answer `false` forever. RAII catches the unwind; a store in one arm does
    // not. (control::ControlServer::bind still has this exact hole — recorded
    // as a known gap on DeathFlag, deliberately not fixed here.)
    let panicked = Arc::new(AtomicBool::new(false));
    let flagged = Arc::clone(&panicked);
    let joined = std::thread::spawn(move || {
        let _flag = DeathFlag::new(flagged);
        panic!("the runtime thread panicked");
    })
    .join();
    assert!(joined.is_err(), "the helper thread must really have panicked");
    assert!(
        panicked.load(Ordering::SeqCst),
        "a PANICKING runtime thread must report the endpoint dead"
    );
}

// ---------------------------------------------------------------------------
// The verbs
// ---------------------------------------------------------------------------

/// Asks the real server a `resolve` question, building the body through
/// [`AddrKind`]'s own serde derive rather than a spelling typed out here — the
/// wire spelling has ONE authority, and `resolve_kind_is_addrkinds_own_spelling`
/// is the single place that pins what it is.
fn post_resolve(port: u16, provider: &str, kind: AddrKind) -> (u16, serde_json::Value) {
    let body = serde_json::json!({ "provider": provider, "kind": kind }).to_string();
    let (status, raw) = request_with_body(port, "POST", "/resolve", &body);
    let parsed = serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("resolve must answer JSON, got {raw:?}: {error}"));
    (status, parsed)
}

/// The addresses `resolve` gives for `(provider, kind)`, asserting a 200.
fn resolve_addrs(port: u16, provider: &str, kind: AddrKind) -> Vec<String> {
    let (status, body) = post_resolve(port, provider, kind);
    assert_eq!(status, 200, "resolve {provider} {kind:?} must answer: {body}");
    body["addrs"]
        .as_array()
        .unwrap_or_else(|| panic!("resolve must answer a LIST under `addrs`, got {body}"))
        .iter()
        .map(|addr| addr.as_str().expect("an address is a string").to_string())
        .collect()
}

fn fake_inputs() -> RuntimeInputs {
    RuntimeInputs {
        database_url: "postgres://fake/db".to_string(),
        ca_cert: std::path::PathBuf::from("/fake/ca-cert.pem"),
        ca_key: std::path::PathBuf::from("/fake/ca-key.pem"),
    }
}

/// THE authority test: for EVERY peer edge in the real split fleet, the address
/// `resolve` hands out is the address `compose_env` composes for that same
/// `(provider, kind)`.
///
/// This is the fix-the-authority proof, not a smoke test. `resolve` and the
/// composed env are the two ways a service can learn where a peer lives; the
/// only thing that makes them one fact rather than two is that `PeerAddrs` and
/// `compose_env_with_fleet` are fed the same `ServiceDef` slice and format
/// through the same `service_addr`. A second `format!("127.0.0.1:{port}")` on
/// either side passes every other test in this file and fails this one — but
/// only because this compares them VALUE BY VALUE over all 19 real peer edges
/// (match 1, characters 1, inventory 2, gateway 8, admin 7),
/// rather than asserting a spot-checked literal that both sides could drift away
/// from together.
#[test]
fn resolve_answers_exactly_what_compose_env_composes_for_the_same_pair() {
    let _guard = agent_guard();
    let agent = split_agent();
    let port = agent.addr().port();

    let fleet = split_fleet();
    let inputs = fake_inputs();
    let mut compared = 0;
    for consumer in &fleet {
        let env = compose_env_with_fleet(consumer, &inputs, &fleet);
        for (key, provider, kind) in consumer.peers {
            let composed = env
                .get(std::ffi::OsStr::new(*key))
                .unwrap_or_else(|| panic!("{} composes no {key}", consumer.name))
                .to_string_lossy()
                .into_owned();
            assert_eq!(
                resolve_addrs(port, provider, *kind),
                vec![composed.clone()],
                "{}: resolve({provider:?}, {kind:?}) must be the SAME fact as its {key}={composed} \
                 — two authorities for where a peer lives is the drift this seam exists to kill",
                consumer.name
            );
            compared += 1;
        }
    }
    // A guard against the loop silently comparing nothing (an empty `peers`
    // field would make the assertions above vacuous and this test green).
    assert_eq!(compared, 19, "the real split fleet's peer edges must all be compared");
}

/// The wire spelling of `kind`, pinned ONCE, against a literal — everything else
/// in this file routes through the serde derive, so without this test the two
/// sides could agree on a renamed spelling and no test would notice. This is
/// what Step 3's client (which cannot test against this server: zero-sharing)
/// must match; only Step 6 pins that the two sides really agree.
#[test]
fn resolve_kind_is_addrkinds_own_spelling() {
    let _guard = agent_guard();
    let agent = split_agent();
    let port = agent.addr().port();

    let (status, body) =
        request_with_body(port, "POST", "/resolve", r#"{"provider":"characters","kind":"edge"}"#);
    assert_eq!(status, 200, "`edge` is the wire spelling of AddrKind::Edge: {body}");
    assert_eq!(body, r#"{"addrs":["127.0.0.1:9000"]}"#, "the answer is a LIST of addresses");

    let (status, body) =
        request_with_body(port, "POST", "/resolve", r#"{"provider":"accounts","kind":"http"}"#);
    assert_eq!(status, 200, "`http` is the wire spelling of AddrKind::Http: {body}");
    assert_eq!(body, r#"{"addrs":["127.0.0.1:8084"]}"#);
}

/// `admin` has `edge_port: None` — it is a passthrough ORIGIN, never a peer.
/// The failing branch this pins is the tempting one: answering the HTTP port
/// because it is the only port that exists. A gateway handed `127.0.0.1:8085`
/// as an mTLS edge would dial an HTTP listener and fail far from here.
#[test]
fn resolve_404s_for_a_provider_that_serves_no_edge() {
    let _guard = agent_guard();
    let agent = split_agent();
    let port = agent.addr().port();

    let (status, body) = post_resolve(port, "admin", AddrKind::Edge);
    assert_eq!(status, 404, "admin has no edge — that is a 404, not a fallback: {body}");

    // And the fallback really was available to take: the HTTP kind answers.
    assert_eq!(resolve_addrs(port, "admin", AddrKind::Http), vec!["127.0.0.1:8085".to_string()]);
}

/// `accounts` is dialed as BOTH kinds at once — the case that makes `kind` a
/// parameter rather than a property of the provider. The two kinds must read
/// two different port fields; a verb keyed on `provider` alone could not answer
/// this at all.
#[test]
fn resolve_reads_a_different_port_field_per_kind_for_one_provider() {
    let _guard = agent_guard();
    let agent = split_agent();
    let port = agent.addr().port();

    let edge = resolve_addrs(port, "accounts", AddrKind::Edge);
    let http = resolve_addrs(port, "accounts", AddrKind::Http);
    assert_ne!(edge, http, "accounts' edge and HTTP addresses are different facts");
    assert_eq!(edge, vec!["127.0.0.1:9003".to_string()], "accounts' edge_port");
    assert_eq!(http, vec!["127.0.0.1:8084".to_string()], "accounts' http_port");
}

/// A provider nobody in this fleet provides. Never a guess, never a nearest
/// match — an address invented here fails at dial time in another process.
#[test]
fn resolve_404s_for_an_unknown_provider() {
    let _guard = agent_guard();
    let agent = split_agent();
    let port = agent.addr().port();

    for unknown in [
        "ghost",
        // NOT a synonym for `characters`: the map keys on ServiceDef::provider,
        // the short name the wire and Stub::new already use. A `-svc` suffix
        // rule anywhere here would be a third naming authority.
        "characters-svc",
        "Characters",
        "",
    ] {
        let (status, body) = post_resolve(port, unknown, AddrKind::Edge);
        assert_eq!(status, 404, "{unknown:?} is not a provider: {body}");
    }
}

/// The discriminator Step 3's client will branch on: both 404s carry a machine
/// readable `code`, so "this agent does not speak the contract" (`unknown_route`
/// — an agent predating the verb, a typo'd path) can never be read as "admin has
/// no HTTP origin" (`unknown_peer` — a fact about the fleet).
///
/// The failing branch: with status alone, Step 4's per-class policy would take
/// an unknown ROUTE for a legitimately absent passthrough origin and boot a
/// gateway whose routes silently 404, instead of dying on an agent that cannot
/// answer it. Prose is not a discriminator — nothing may parse `error`.
#[test]
fn both_404s_are_told_apart_by_code_not_by_prose() {
    let _guard = agent_guard();
    let agent = split_agent();
    let port = agent.addr().port();

    // A fact about the FLEET.
    let (status, body) = post_resolve(port, "ghost", AddrKind::Edge);
    assert_eq!(status, 404);
    assert_eq!(body["code"], "unknown_peer", "an unknown provider is a fact about the fleet");

    // A fact about the AGENT — same status, different meaning.
    for (method, path) in [("POST", "/resolv"), ("GET", "/resolve"), ("POST", "/nope")] {
        let (status, raw) = request_with_body(port, method, path, "{}");
        assert_eq!(status, 404, "{method} {path}");
        let parsed: serde_json::Value = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("every non-2xx carries an envelope, got {raw:?}: {e}"));
        assert_eq!(
            parsed["code"], "unknown_route",
            "{method} {path} is the AGENT not speaking the contract — never a fact about a \
             service: {raw}"
        );
    }

    // A malformed question is neither.
    let (status, raw) = request_with_body(port, "POST", "/resolve", "{}");
    assert_eq!(status, 400);
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("an envelope");
    assert_eq!(parsed["code"], "bad_request");
}

/// THE topology branch. Under the monolith, `resolve` answers NOTHING — and it
/// does so because `monolith()`'s `provider` is `None` (one process hosting all
/// 12 domains is nameable as none of them), so the map is empty as a property of
/// the DATA. There is no `if topology` anywhere on this path.
///
/// The branch that used to be wrong, and that this pins: a map built from
/// `split_fleet()` regardless of the booting topology would hand out addresses
/// for twelve processes that do not exist — and every one of those answers would
/// look perfectly well-formed.
#[test]
fn under_the_monolith_every_resolve_404s() {
    let _guard = agent_guard();
    let agent = AgentServer::bind(0, PeerAddrs::from_fleet(&[monolith()]))
        .expect("bind the agent on the monolith topology");
    let port = agent.addr().port();

    // Derived from the split fleet, not hand-listed: a service added there is
    // automatically asked for here.
    let providers: Vec<&str> = split_fleet().iter().filter_map(|svc| svc.provider).collect();
    assert_eq!(providers.len(), 12, "the split fleet's providers");
    for provider in providers {
        for kind in [AddrKind::Edge, AddrKind::Http] {
            let (status, body) = post_resolve(port, provider, kind);
            assert_eq!(
                status, 404,
                "the monolith hosts {provider} IN-PROCESS — it has no address to give: {body}"
            );
        }
    }
    // The monolith's own name is not resolvable either — it has none.
    assert_eq!(post_resolve(port, "server", AddrKind::Http).0, 404);
    assert_eq!(post_resolve(port, "monolith", AddrKind::Http).0, 404);
    // The endpoint itself is perfectly alive: 404 is an ANSWER here, not a
    // degradation. A monolith has no peers to resolve.
    assert_eq!(request(port, "GET", "/healthz").0, 200);
    assert!(!agent.dead());
}

/// A malformed question is the caller's 400, kept distinct from the 404 that
/// means "well-formed, no answer" — the two are different problems for whoever
/// reads the log, and merging them would make an unknown `kind` look like a
/// missing service.
#[test]
fn resolve_rejects_a_malformed_question_as_400_not_404() {
    let _guard = agent_guard();
    let agent = split_agent();
    let port = agent.addr().port();

    for body in [
        // Not JSON at all.
        "",
        "not json",
        // A kind outside the enum. `deny_unknown_fields` + the derive make the
        // set closed: there is no "default kind" to fall into.
        r#"{"provider":"characters","kind":"quic"}"#,
        r#"{"provider":"characters","kind":"Edge"}"#,
        // Missing / extra / mistyped fields.
        r#"{"provider":"characters"}"#,
        r#"{"kind":"edge"}"#,
        r#"{"provider":"characters","kind":"edge","replicas":3}"#,
        r#"{"provider":7,"kind":"edge"}"#,
    ] {
        let (status, raw) = request_with_body(port, "POST", "/resolve", body);
        assert_eq!(status, 400, "malformed resolve {body:?} must be a 400, got {raw}");
    }
    // Still serving: a bad request is not a poison pill.
    assert_eq!(resolve_addrs(port, "characters", AddrKind::Edge), vec!["127.0.0.1:9000"]);
}

/// `hello` is the contract's SHAPE — it logs, returns `{}`, and (deliberately)
/// changes nothing. The parse is still strict, because the shape is the whole
/// deliverable: a `hello` that accepted anything would pin nothing at all.
#[test]
fn hello_accepts_a_well_formed_greeting_and_rejects_a_malformed_one() {
    let _guard = agent_guard();
    let agent = split_agent();
    let port = agent.addr().port();

    let (status, body) =
        request_with_body(port, "POST", "/hello", r#"{"service":"characters-svc","pid":1234}"#);
    assert_eq!(status, 200, "a well-formed hello is accepted: {body}");
    assert_eq!(body, "{}", "hello has no mechanism behind it — it answers an empty object");

    for body in [
        "",
        "{",
        // Missing / extra / mistyped fields.
        r#"{"service":"characters-svc"}"#,
        r#"{"pid":1234}"#,
        r#"{"service":"characters-svc","pid":"1234"}"#,
        r#"{"service":"characters-svc","pid":-1}"#,
        r#"{"service":"characters-svc","pid":1234,"instance":"a"}"#,
    ] {
        let (status, raw) = request_with_body(port, "POST", "/hello", body);
        assert_eq!(status, 400, "malformed hello {body:?} must be a 400, got {raw}");
    }
    assert!(!agent.dead(), "a malformed greeting must not kill the endpoint");
}

/// An oversized body is a bounded 400, not an unbounded buffer: `Limited` caps
/// the read before the parse. Without the cap, one confused client could grow
/// this process without limit — and this process is supervising a fleet.
#[test]
fn an_oversized_body_is_a_bounded_400() {
    let _guard = agent_guard();
    let agent = split_agent();
    let port = agent.addr().port();

    // Just over the cap, deliberately not megabytes: the server answers 400 and
    // closes while the client is still mid-body, so a body larger than the
    // socket buffer would make the CLIENT's write fail (a reset) instead of
    // reading the answer — which would test the test, not the cap.
    let huge = format!(
        r#"{{"provider":"{}","kind":"edge"}}"#,
        "x".repeat(MAX_BODY_BYTES + 1024)
    );
    let (status, _) = within_budget("an oversized resolve body", move || {
        request_with_body(port, "POST", "/resolve", &huge)
    });
    assert_eq!(status, 400, "a body over the cap is rejected, not buffered");
    assert_eq!(resolve_addrs(port, "characters", AddrKind::Edge), vec!["127.0.0.1:9000"]);
}
