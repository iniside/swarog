//! The agent's HTTP endpoint: a tokio runtime on its OWN thread, hosting a
//! loopback HTTP server beside weles's otherwise fully synchronous supervisor.
//!
//! There is **no prior art in this repo** for a runtime on a dedicated thread
//! beside sync code (`docs/reference/weles-design.md`, "the async island") —
//! every other crate is whole-process async with `spawn_blocking` escapes, or
//! whole-process sync with one `block_on` in `main`. The lifecycle IS the risk,
//! so it was built and proven alone (Step 2a) before this step laid the contract
//! on top of it.
//!
//! # The contract (M1's whole point is its SHAPE)
//!
//! Two verbs, plus `/healthz`. Both are `POST` + JSON in / JSON out:
//!
//! ```text
//! POST /resolve  {"provider":"characters","kind":"edge"}  → 200 {"addrs":["127.0.0.1:9000"]}
//! POST /hello    {"service":"characters-svc","pid":1234}  → 200 {}
//! GET  /healthz                                           → 200 ok
//! ```
//!
//! * **`resolve` answers a LIST**, with exactly one element in M1. The design
//!   (`weles-design.md`) has `resolve` return *all live instances* because
//!   round-robin LB is client-side; a scalar would bake in a shape LB must
//!   break — in the milestone whose entire purpose is getting the shape right.
//!   LB itself is out of scope.
//! * **404 and `{"addrs":[]}` are different answers, and the line is drawn
//!   HERE** rather than by whichever a client meets first (also recorded in
//!   `weles-design.md`, "M1 scope"):
//!   * **404 `unknown_peer`** — *this `(provider, kind)` is not a thing in this
//!     topology.* A closed-world fact: the map is derived from the manifest, so
//!     an unknown provider or a kind a service does not serve is knowably not
//!     coming. M1 only ever produces this one.
//!   * **200 `{"addrs":[]}`** — *it is a thing; nothing is live right now.* A
//!     liveness fact, which M1 has no source for and therefore never answers.
//!     When M2 adds liveness, zero instances belongs here — inside the list
//!     shape LB already handles — and must NOT be widened into the 404.
//!
//!   A client may treat 404 as fatal-and-final; it may not treat `[]` that way.
//! * **`kind` is a parameter** because the gateway needs eight addresses of TWO
//!   classes: six edge peers plus two HTTP passthrough origins. `accounts` is
//!   both at once (edge 9003, http 8084) and `admin` has `edge_port: None` and
//!   is only ever an origin. A verb keyed on `provider` alone structurally
//!   could not answer. It is [`crate::manifest::AddrKind`] itself on the wire,
//!   never a wire-local twin.
//! * **`hello` is shape, not mechanism.** It logs and returns `{}`. It is here
//!   so the contract is whole; registration only starts to matter for processes
//!   weles did not spawn. See "What this island may NEVER own".
//! * **Every non-2xx carries `{"code":…,"error":…}`** — see [`ErrorCode`]. The
//!   code is the only thing a client may branch on; the prose is for operators.
//! * **`POST` for both**, even though `resolve` is a read: one wire style for
//!   what the design calls a "wire-only JSON contract" (not a REST API), no new
//!   dependency, and a body that `serde` can reject whole with
//!   `deny_unknown_fields`. This is a preference, not a forced move — a
//!   `GET ?provider=…&kind=edge` would deserialize into [`AddrKind`] through
//!   the same derive via `serde_urlencoded`, so it would NOT have cost a second
//!   spelling authority (an earlier draft of this doc claimed it would; it was
//!   wrong).
//!
//! **Deliberate deviation from the design, recorded** (`weles-design.md`:
//! *resolve is scoped per-consumer, never "give me the fleet map"*): M1 serves
//! the map without the caller's identity, because there is no identity mechanism
//! on this hop yet (loopback HTTP; `SO_PEERCRED` is separate work). Narrow and
//! local: one machine, one trust domain. Closes when the contract crosses a
//! trust boundary.
//!
//! # What this island may NEVER own
//!
//! The supervisor's correctness rests on things that stay sync, on the
//! supervisor thread (`weles-design.md`, "Hard rule for the refactor"):
//!
//! * **`platform/*`** — `tokio::process` would install a SIGCHLD handler and
//!   reap children out from under [`crate::platform`]'s `OwnedProc::try_wait`,
//!   destroying `Observed::Exited`, the sole authority for "the process is
//!   gone". Under async, "connection refused" and "the process is gone" look
//!   alike; nothing here may ever manufacture `Observed::Exited`.
//! * **`spawn`** — `SPAWN_LOCK` is a `std::sync::Mutex` held across
//!   `CreateProcessW`.
//! * **[`crate::lock`]** — `RolloutLock` stays an RAII local on the supervisor
//!   thread; "the lock drops last" is an ordering guarantee a task would break.
//! * **[`crate::state`] / [`crate::prep`]** and the pure clock-injected
//!   decision functions.
//! * **The signal handler**, which may touch only a static atomic.
//! * **`Reporter`** — it is `!Sync` (`Cell`/`RefCell`). The server thread never
//!   touches it, and never touches `shared` either: that dodges the state-mutex
//!   poisoning trap by construction rather than by discipline. `shared` is a
//!   `std::sync::Mutex` the supervisor unwraps with `.expect("poisoned")` in
//!   seven places, so a panic in a handler that held it would take the
//!   supervisor's NEXT checkpoint down with it — a dead agent endpoint would
//!   become a dead fleet.
//!
//!   This is why both verbs answer from values only: `resolve` reads an owned
//!   [`crate::manifest::PeerAddrs`] map, computed on the supervisor thread
//!   before the runtime thread exists and `move`d in (the same pattern
//!   `supervisor::run_up` already uses for `ports`), and `hello` writes nothing
//!   at all. **KNOWN, ARMED:** a future `hello` that wants to record a
//!   registration will want `shared`, and that is the step that arms this mine.
//!
//! The island may own network I/O and hand back plain values. That is all.
//!
//! # Why this is not a copy of [`crate::control::ControlServer`]
//!
//! The shape IS copied — the `sync_channel(1)` ready handshake with all three
//! arms, and the stop-authority separation (a private shutdown signal plus
//! `dead`, and NEVER the fleet stop: a control-plane failure must never look
//! like an operator `down`). Two things do not translate:
//!
//! 1. **`AtomicBool` + poll-sleep cancellation cannot reach an accept parked on
//!    `.await`.** `ControlServer`'s accept loop polls a nonblocking listener and
//!    checks the flag each pass; an `.await`ed accept never looks at a flag.
//!    Copying [`crate::control::ControlServer`]'s `Drop` verbatim here would
//!    hang the join forever. Cancellation is a `oneshot` raced against accept.
//! 2. **`Runtime::drop` blocks.** The runtime is therefore built, run, and
//!    dropped entirely on its own thread ([`run_runtime`]); dropping it on the
//!    supervisor thread would stall teardown and, behind it, the `_lock`
//!    release.
//!
//! The accept loop's failure handling, by contrast, IS `ControlServer`'s
//! verbatim — report, sleep [`ACCEPT_RETRY_DELAY`], retry forever — because an
//! accept failure means the same thing on both endpoints.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use hyper::header::{HeaderValue, CONTENT_TYPE};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::manifest::{AddrKind, PeerAddrs};

/// How long [`AgentServer::bind`] waits for the runtime thread to report the
/// endpoint is actually listening before failing loudly. Same value and same
/// intent as `control::BIND_DEADLINE` (copied, not shared — these are two
/// independent endpoints that happen to agree).
const BIND_DEADLINE: Duration = Duration::from_secs(2);

/// Upper bound on `Runtime::shutdown_timeout` once the accept loop has stopped.
///
/// This does NOT give in-flight connection tasks a grace period: a
/// `tokio::spawn`ed async task is dropped at its current await point the moment
/// the runtime shuts down, so every connection here is abandoned at once — this
/// timeout can never elapse on their account. What it actually bounds is the
/// thing `Runtime::drop` really waits for (blocking tasks and worker parking),
/// so a task that somehow refuses to yield cannot wedge the join forever. There
/// are no blocking tasks on this runtime today, which is precisely why this is
/// an escape hatch and not a mechanism anything may rely on.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Delay between accept retries, so a persistently failing `accept()` (EMFILE
/// under fd pressure) cannot spin this thread. Copied verbatim in value and
/// intent from `control::serve`'s accept loop: report, wait, retry — FOREVER.
///
/// Retrying forever is the point. An accept failure is an ambient, transient
/// condition (weles spawns a 12-service fleet with stdio pipes right after this
/// endpoint binds, so fd pressure is plausible here specifically) that clears in
/// milliseconds. Giving up would delete the agent for the rest of the run over a
/// condition that has already passed.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_secs(1);

/// Whole-request bound on reading a client's headers. hyper's own default is
/// 30s but is INERT without a `Timer` installed — so the timer is installed and
/// this is set explicitly, rather than left as a bound that silently does not
/// exist.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Cap on a request body, before parsing. Both verbs' bodies are two small
/// fields; a body cannot be trusted to be small just because every honest
/// caller's is, and `collect()`ing an unbounded body would let one confused
/// client grow this process's memory without limit. Over the cap is a 400 (the
/// request is malformed by contract), never a truncated parse.
///
/// Deliberately TIGHTER than `control::MAX_FRAME` (64 KiB) rather than copied
/// from it, unlike this module's other borrowings from `control.rs`
/// ([`BIND_DEADLINE`], [`ACCEPT_RETRY_DELAY`]): that bound covers a status
/// table listing a whole fleet, while these two bodies are a short name plus a
/// tiny enum. A bound is only worth having at the size of the thing it bounds.
const MAX_BODY_BYTES: usize = 8 * 1024;

/// Live agent runtime threads.
///
/// This exists so "the runtime thread was not leaked" is provable rather than
/// asserted: a leaked tokio runtime thread is otherwise completely invisible
/// from outside (it holds no port, no handle, and no observable state after a
/// failed bind). Incremented when the thread's body starts and decremented by
/// [`RuntimeThreadToken`]'s `Drop` — so it falls back to zero even if the
/// thread panics. Process-global: tests that read it serialize on
/// `agentapi_tests::agent_guard`.
static RUNTIME_THREADS: AtomicUsize = AtomicUsize::new(0);

/// RAII counter for [`RUNTIME_THREADS`], owned by the runtime thread's body.
struct RuntimeThreadToken;

impl RuntimeThreadToken {
    fn new() -> Self {
        RUNTIME_THREADS.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for RuntimeThreadToken {
    fn drop(&mut self) {
        RUNTIME_THREADS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Flags the endpoint dead when the runtime thread ENDS, unless disarmed.
///
/// The authority for "the endpoint is gone" is the thread ending — not one
/// particular way of ending. Storing the flag from inside an `if let Err(...)`
/// arm misses the other way out: a panic unwinds straight past it, so the
/// thread is gone, the port is released, `join()`'s `Err` is swallowed, and
/// `dead()` still answers `false` forever. RAII covers both exits, so only a
/// CLEAN stop (`run_runtime` returned `Ok`, i.e. we asked it to stop) disarms.
///
/// KNOWN GAP, deliberately not fixed here: `control::ControlServer::bind` has
/// this identical hole — its `thread_dead.store(true, …)` also lives only in
/// the `if let Err(…)` arm, so a panicking control serve thread reports
/// `dead() == false` too. Pre-existing and out of this step's scope; recorded
/// rather than silently left as a twin of the bug fixed here.
struct DeathFlag {
    dead: Arc<AtomicBool>,
    armed: bool,
}

impl DeathFlag {
    fn new(dead: Arc<AtomicBool>) -> Self {
        Self { dead, armed: true }
    }

    /// The thread is ending because it was ASKED to. Not a death.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for DeathFlag {
    fn drop(&mut self) {
        if self.armed {
            self.dead.store(true, Ordering::SeqCst);
        }
    }
}

/// Owns the agent's runtime thread; dropping it stops and joins it.
///
/// Stop-authority invariant (copied from [`crate::control::ControlServer`]):
/// this type has no access to the supervisor's fleet-stop atomic at all. Its
/// whole lifecycle — bind timeout, teardown via `Drop`, a dead runtime thread —
/// flows through the private `shutdown` oneshot and the `dead` flag. A dead
/// agent endpoint is a degradation the supervisor reports; it is never a
/// phantom operator `down` that tears a healthy fleet down.
#[derive(Debug)]
pub struct AgentServer {
    /// Private stop signal. Sending (or dropping) it resolves the receiver the
    /// accept loop is `select!`ing on — the ONLY cancellation that reaches an
    /// accept parked on `.await`.
    shutdown: Option<oneshot::Sender<()>>,
    /// Set (only) when the runtime thread died irrecoverably: the endpoint is
    /// gone but the fleet keeps running.
    dead: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    addr: SocketAddr,
}

impl AgentServer {
    /// Binds `127.0.0.1:port` and spawns the runtime thread. Blocks until the
    /// thread reports the endpoint is actually listening (or fails within
    /// [`BIND_DEADLINE`]).
    ///
    /// Loopback only, plaintext only: no TLS feature is enabled anywhere in
    /// this island, which is what keeps `rustls`/`ring`/`aws-lc-rs` out of the
    /// question entirely rather than merely unused.
    ///
    /// On any failure the runtime thread is joined before returning, so an
    /// `Err` from here never leaves a thread (or a runtime) behind — pinned by
    /// `bind_on_a_taken_port_fails_without_leaking_the_runtime_thread`.
    ///
    /// `peers` is what `resolve` answers from: an owned, already-computed map
    /// of the BOOTING topology, moved onto the runtime thread. It is a value
    /// and not a handle on purpose — see the module doc on what this island may
    /// never own.
    pub fn bind(port: u16, peers: PeerAddrs) -> Result<Self> {
        Self::bind_inner(port, peers, ACCEPT_RETRY_DELAY, 0)
    }

    /// [`AgentServer::bind`] with the accept loop's recovery arm drivable:
    /// `accept_faults` accepts fail before the real listener is consulted, and
    /// `retry_delay` replaces [`ACCEPT_RETRY_DELAY`] so a test need not sit
    /// through real seconds. Test-only — production always passes `0` and the
    /// real delay.
    #[cfg(test)]
    fn bind_faulty(
        port: u16,
        peers: PeerAddrs,
        accept_faults: usize,
        retry_delay: Duration,
    ) -> Result<Self> {
        Self::bind_inner(port, peers, retry_delay, accept_faults)
    }

    fn bind_inner(
        port: u16,
        peers: PeerAddrs,
        retry_delay: Duration,
        accept_faults: usize,
    ) -> Result<Self> {
        let dead = Arc::new(AtomicBool::new(false));
        let thread_dead = Arc::clone(&dead);
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("weles-agent".into())
            .spawn(move || {
                let _live = RuntimeThreadToken::new();
                // Armed: any exit from here on — Err OR panic — means the
                // endpoint is gone and `dead()` must say so.
                let mut death = DeathFlag::new(thread_dead);
                match run_runtime(port, peers, retry_delay, accept_faults, ready_tx, shutdown_rx) {
                    // Asked to stop: the endpoint ending is the expected
                    // outcome, not a death.
                    Ok(()) => death.disarm(),
                    // Irrecoverable endpoint death: report it, but do NOT stop
                    // the fleet — a lost agent endpoint is a degradation, never
                    // a phantom operator-down. The flag stays armed.
                    Err(error) => eprintln!(
                        "weles: agent endpoint died: {error:#} — services cannot reach the \
                         agent for this run; the fleet keeps running"
                    ),
                }
            })
            .context("spawn agent endpoint")?;
        match ready_rx.recv_timeout(BIND_DEADLINE) {
            Ok(Ok(addr)) => Ok(Self {
                shutdown: Some(shutdown_tx),
                dead,
                thread: Some(thread),
                addr,
            }),
            Ok(Err(error)) => {
                // Signal before joining, exactly as the timeout arm does. Today
                // every `ready.send(Err(_))` site bails immediately after, so
                // this join would return anyway — but that is an UNSTATED
                // invariant of code elsewhere, and this join has no deadline.
                // A future failure path that reports an error without exiting
                // (Step 2b adds verbs and new failure paths) would hang `bind`
                // forever. Uniform: every arm that abandons the thread signals
                // it first.
                drop(shutdown_tx);
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                // Signal through the oneshot, NOT a flag: if the thread did
                // bind (just late), only this wakes the accept `.await` and
                // lets the join return. Dropping the sender is the signal.
                drop(shutdown_tx);
                let _ = thread.join();
                bail!("agent endpoint did not become ready within {BIND_DEADLINE:?}")
            }
        }
    }

    /// The address the endpoint actually listens on.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Whether the runtime thread died irrecoverably (endpoint gone, fleet
    /// unaffected). The supervisor reports this once, loudly.
    pub fn dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }
}

impl Drop for AgentServer {
    fn drop(&mut self) {
        // NOT `ControlServer::drop`'s `AtomicBool` store: a flag never reaches
        // an accept parked on `.await`, and this join would hang forever.
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            // Bounded by construction: the accept loop breaks on the signal
            // above, and the runtime's own drop is bounded by SHUTDOWN_GRACE
            // — on the runtime's thread, not this one.
            let _ = thread.join();
        }
    }
}

/// The runtime thread's whole body: build the runtime, run the server on it,
/// then shut it down — all three on THIS thread.
///
/// `Runtime::drop` blocks until the runtime has wound down. That blocking wait
/// must never land on the supervisor thread (it would stall teardown and the
/// `_lock` release behind it), and it must be bounded — hence the explicit
/// [`Runtime::shutdown_timeout`](tokio::runtime::Runtime::shutdown_timeout)
/// here, after `block_on` has returned (calling it from inside an async context
/// would panic).
fn run_runtime(
    port: u16,
    peers: PeerAddrs,
    retry_delay: Duration,
    accept_faults: usize,
    ready: std::sync::mpsc::SyncSender<Result<SocketAddr>>,
    shutdown: oneshot::Receiver<()>,
) -> Result<()> {
    // One worker: this endpoint answers a handful of loopback requests. `io`
    // drives the listener; `time` drives the accept-retry delay and hyper's
    // header-read timeout. NEVER the process or signal drivers (see the module
    // doc; the ban is enforced mechanically by verifyctl's `weles-async-island`
    // stage, because a comment cannot survive a feature-unification change
    // elsewhere).
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("weles-agent-rt")
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let message = error.to_string();
            let _ = ready.send(Err(anyhow::Error::new(error).context("build agent runtime")));
            bail!(message);
        }
    };
    let result =
        runtime.block_on(serve(port, peers, retry_delay, accept_faults, ready, shutdown));
    runtime.shutdown_timeout(SHUTDOWN_GRACE);
    result
}

/// Where the accept loop gets its next connection.
///
/// Production always consults the real listener. `pending_faults` lets a test
/// drive the loop's RECOVERY arm — the one that used to give up — through the
/// real production loop, sleep and all, rather than around it. The loop never
/// inspects the error, so a constructed error exercises exactly the same arm an
/// EMFILE would.
struct AcceptSource {
    listener: tokio::net::TcpListener,
    pending_faults: usize,
}

impl AcceptSource {
    /// Cancel-safe: `TcpListener::accept` is, and the fault arm returns without
    /// awaiting at all, so `select!` dropping this future loses no connection.
    async fn accept(&mut self) -> std::io::Result<tokio::net::TcpStream> {
        if self.pending_faults > 0 {
            self.pending_faults -= 1;
            return Err(std::io::Error::other("injected accept failure (EMFILE-shaped)"));
        }
        self.listener.accept().await.map(|(stream, _peer)| stream)
    }
}

/// Binds the listener, reports readiness, then accepts until cancelled.
///
/// The ready handshake is [`crate::control::ControlServer::bind`]'s: the bind
/// error travels to the caller through the channel AND fails this function, so
/// a bind failure can never be a silent hang on the caller's side.
async fn serve(
    port: u16,
    peers: PeerAddrs,
    retry_delay: Duration,
    accept_faults: usize,
    ready: std::sync::mpsc::SyncSender<Result<SocketAddr>>,
    mut shutdown: oneshot::Receiver<()>,
) -> Result<()> {
    // Read-only for the endpoint's whole life: an `Arc`, never a lock. Nothing
    // here can mutate the map, so nothing here can be poisoned or contended.
    let peers = Arc::new(peers);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(error) => {
            let message = error.to_string();
            let _ = ready.send(
                Err(anyhow::Error::new(error))
                    .with_context(|| format!("bind agent endpoint {addr}")),
            );
            bail!(message);
        }
    };
    let local = listener.local_addr().unwrap_or(addr);
    let _ = ready.send(Ok(local));

    let mut source = AcceptSource { listener, pending_faults: accept_faults };
    loop {
        tokio::select! {
            // Resolves on send OR on the sender being dropped: either is a
            // stop. This arm is why cancellation reaches a parked accept.
            _ = &mut shutdown => break,
            accepted = source.accept() => match accepted {
                Ok(stream) => {
                    let peers = Arc::clone(&peers);
                    tokio::spawn(async move {
                        let served = hyper::server::conn::http1::Builder::new()
                            // The timer makes the timeout REAL. Without one,
                            // hyper's own 30s default is inert (it warns and
                            // never fires) — a bound that silently does not
                            // exist. Set explicitly rather than inherited.
                            .timer(hyper_util::rt::TokioTimer::new())
                            .header_read_timeout(Some(HEADER_READ_TIMEOUT))
                            .serve_connection(
                                TokioIo::new(stream),
                                service_fn(move |request| {
                                    route(request, Arc::clone(&peers))
                                }),
                            )
                            .await;
                        if let Err(error) = served {
                            eprintln!("weles: agent connection failed ({error}); continuing");
                        }
                    });
                }
                // Report, wait, retry — FOREVER, exactly as control::serve
                // does. A per-accept failure is an ambient transient (fd
                // pressure while the fleet spawns): never kill the endpoint
                // over it, and never let it spin. Giving up after N would
                // delete the agent for the whole run over a condition that
                // clears in milliseconds — and a count cannot tell "N errors in
                // 10µs" from "N over a minute", which is exactly what the delay
                // does distinguish.
                Err(error) => {
                    eprintln!("weles: agent accept failed ({error}); continuing");
                    tokio::time::sleep(retry_delay).await;
                }
            },
        }
    }
    Ok(())
}

/// Why a request was refused — the ONE thing a client is allowed to branch on.
///
/// Every non-2xx answer carries one of these in `{"code":…,"error":…}`; `error`
/// is prose for an operator and no client may parse it. This exists because the
/// two 404s are otherwise indistinguishable: an unknown PATH (an agent that
/// predates the verb, a typo'd URL) and an unknown PROVIDER (a well-formed
/// question this topology has no answer to) are the same status code, and a
/// client that could not tell them apart would read "this agent does not speak
/// the contract" as "admin has no HTTP origin" — then boot a gateway with a
/// silently empty passthrough instead of failing loudly. Status alone cannot
/// carry that, so the discriminator is a closed enum with its own serde
/// spelling, not a shape a client has to infer.
///
/// `pub` for ONE reason: `verifyctl`'s `weles-wire-contract` stage is the only
/// place in the repo that may see both this type and its hand-copied twin
/// `remote::ErrorCode` (zero-sharing forbids the two crates seeing each other;
/// verification tooling is the narrow sanctioned exception). Nothing outside
/// weles constructs one — the widening buys a drift gate, not a client API.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// No such route: this endpoint does not serve that (method, path). To a
    /// client, this means the agent does not speak the contract it expected —
    /// NEVER a fact about a service.
    UnknownRoute,
    /// The route exists and the question was well-formed; this topology has no
    /// such (provider, kind). A FACT about the fleet.
    UnknownPeer,
    /// The question itself was malformed (unparseable, unknown `kind`,
    /// missing/extra field, over the body cap).
    BadRequest,
    /// The agent failed on its own account. Never the caller's fault.
    Internal,
}

/// The body of every non-2xx answer.
#[derive(Debug, Serialize)]
struct ErrorResponse<'a> {
    code: ErrorCode,
    /// Operator prose. Deliberately unstructured — nothing may branch on it.
    error: &'a str,
}

/// A `resolve` question. `deny_unknown_fields` so a typo'd or renamed field is
/// a loud 400 rather than a silently defaulted one — this is a versionless
/// contract on one machine, so strictness costs nothing and catches drift on the
/// first request instead of the first outage.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveRequest {
    /// The provider's SHORT name (`"characters"`) — `ServiceDef::provider`, the
    /// same spelling `remote::Stub::new("characters", …)` and the fleet
    /// manifest use. Never a `-svc` package name.
    provider: String,
    /// [`AddrKind`] itself, spelled by its own serde derive.
    kind: AddrKind,
}

/// A `resolve` answer. ALWAYS a list — see the module doc.
#[derive(Debug, Serialize)]
struct ResolveResponse {
    addrs: Vec<String>,
}

/// A `hello`. The shape of registration; M1 has no mechanism behind it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HelloRequest {
    /// The process's fleet name (`"characters-svc"`), as spawned.
    service: String,
    pid: u32,
}

/// The whole route table. Matching is on `(method, path)`, and anything that
/// does not match is a 404 — never a redirect, a guess, or a default.
async fn route(
    request: Request<hyper::body::Incoming>,
    peers: Arc<PeerAddrs>,
) -> std::result::Result<Response<Full<Bytes>>, std::convert::Infallible> {
    let response = match (request.method(), request.uri().path()) {
        (&Method::GET, "/healthz") => reply(StatusCode::OK, "ok\n"),
        (&Method::POST, "/resolve") => resolve(request, &peers).await,
        (&Method::POST, "/hello") => hello(request).await,
        // `unknown_route`, NOT `unknown_peer`: this is a statement about the
        // AGENT, not about a service. A client must be able to tell the two
        // apart without parsing prose — see [`ErrorCode`].
        (method, path) => json_error(
            StatusCode::NOT_FOUND,
            ErrorCode::UnknownRoute,
            &format!("no route {method} {path}"),
        ),
    };
    Ok(response)
}

/// `resolve`: which addresses of `kind` does `provider` have, in the topology
/// that is actually booting?
///
/// Two distinct failures, deliberately NOT merged:
///
/// * A body that is not a well-formed question (unparseable, unknown `kind`,
///   missing/extra field) is a **400** — the caller asked wrong.
/// * A well-formed question with no answer — unknown provider, or a provider
///   with no address of that kind (`admin` has `edge_port: None`; every
///   provider under the monolith) — is a **404**. It is never answered with the
///   other kind, a default, or a nearest match: a wrong address is strictly
///   worse than no address, because it fails at dial time in another process,
///   far from here.
async fn resolve(
    request: Request<hyper::body::Incoming>,
    peers: &PeerAddrs,
) -> Response<Full<Bytes>> {
    let question: ResolveRequest = match read_json(request).await {
        Ok(question) => question,
        Err(error) => return json_error(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, &error),
    };
    let addrs = peers.lookup(&question.provider, question.kind);
    if addrs.is_empty() {
        return json_error(
            StatusCode::NOT_FOUND,
            ErrorCode::UnknownPeer,
            &format!(
                "no {:?} address for provider {:?} in the booting topology",
                question.kind, question.provider
            ),
        );
    }
    json_ok(&ResolveResponse { addrs })
}

/// `hello`: the contract's registration shape, with no mechanism behind it yet.
///
/// It logs and returns `{}`. It writes NOTHING — see the module doc: the
/// supervisor's state mutex is the mine that a recording `hello` will arm, and
/// this step does not arm it. The parse is still strict, because the shape is
/// the entire deliverable here: a `hello` that accepted anything would pin
/// nothing.
async fn hello(request: Request<hyper::body::Incoming>) -> Response<Full<Bytes>> {
    let greeting: HelloRequest = match read_json(request).await {
        Ok(greeting) => greeting,
        Err(error) => return json_error(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, &error),
    };
    println!("weles: hello from service={} pid={}", greeting.service, greeting.pid);
    json_response(StatusCode::OK, Bytes::from_static(b"{}"))
}

/// Reads a BOUNDED request body and parses it, mapping every failure to one
/// message the caller gets verbatim in a 400.
async fn read_json<T: DeserializeOwned>(
    request: Request<hyper::body::Incoming>,
) -> std::result::Result<T, String> {
    // Limited, then collect: an over-cap body errors here rather than being
    // buffered whole and rejected afterwards.
    let collected = Limited::new(request.into_body(), MAX_BODY_BYTES)
        .collect()
        .await
        .map_err(|error| format!("read request body (cap {MAX_BODY_BYTES} bytes): {error}"))?;
    serde_json::from_slice(&collected.to_bytes())
        .map_err(|error| format!("parse request body: {error}"))
}

fn json_ok<T: Serialize>(value: &T) -> Response<Full<Bytes>> {
    match serde_json::to_vec(value) {
        Ok(body) => json_response(StatusCode::OK, Bytes::from(body)),
        // Serializing our own owned Strings cannot fail today; if it somehow
        // does, that is ours, not the caller's — a 500, never a 200 with a
        // broken body.
        Err(error) => {
            eprintln!("weles: agent could not serialize a response: {error}");
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
                "response serialization failed",
            )
        }
    }
}

/// The one way to answer a non-2xx: a machine-readable [`ErrorCode`] plus prose
/// for the operator. There is no other refusal path — a bare status with no
/// envelope would put a client back to guessing.
fn json_error(status: StatusCode, code: ErrorCode, message: &str) -> Response<Full<Bytes>> {
    let body = serde_json::to_vec(&ErrorResponse { code, error: message })
        // Serializing a copy enum + a &str cannot fail; if it somehow did, the
        // envelope's CODE is the part that must survive, since it is the part a
        // client is allowed to read.
        .unwrap_or_else(|_| br#"{"code":"internal","error":"unprintable"}"#.to_vec());
    json_response(status, Bytes::from(body))
}

fn json_response(status: StatusCode, body: Bytes) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(body));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

fn reply(status: StatusCode, body: &'static str) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from_static(body.as_bytes())));
    *response.status_mut() = status;
    response
}

// ---------------------------------------------------------------------------
// Drift-gate seams for `verifyctl`'s `weles-wire-contract` stage.
//
// Zero-sharing means this server and its client (`core/remote`'s `resolve`)
// cannot see each other, so NEITHER crate's tests can catch a drift between the
// two hand-copied spellings of this contract — see this module's doc and
// `remote::resolve`'s. `verifyctl` may import both (the narrow verification-
// tooling exception), and these three functions are the smallest surface that
// lets it drive the REAL derives on this side: the wire types themselves stay
// private, so the stage pins what the server actually reads/writes rather than a
// fourth copy of the field names. Nothing in weles calls them.
// ---------------------------------------------------------------------------

/// Parses a `resolve` question exactly as [`resolve`] does — the real
/// [`ResolveRequest`] derive, `deny_unknown_fields` included — and hands back
/// the two values the verb reads. A field the server does not know is an `Err`
/// here for the same reason it is a 400 there.
#[doc(hidden)]
pub fn drift_probe_parse_resolve_request(body: &[u8]) -> std::result::Result<(String, AddrKind), String> {
    serde_json::from_slice::<ResolveRequest>(body)
        .map(|question| (question.provider, question.kind))
        .map_err(|error| error.to_string())
}

/// Renders a `resolve` answer exactly as [`json_ok`] does.
#[doc(hidden)]
pub fn drift_probe_encode_resolve_response(addrs: Vec<String>) -> Vec<u8> {
    serde_json::to_vec(&ResolveResponse { addrs })
        .expect("ResolveResponse is a Vec<String> — serializing it cannot fail")
}

/// Renders a refusal envelope exactly as [`json_error`] does.
#[doc(hidden)]
pub fn drift_probe_encode_error_response(code: ErrorCode, message: &str) -> Vec<u8> {
    serde_json::to_vec(&ErrorResponse { code, error: message })
        .expect("ErrorResponse is a Copy enum plus a &str — serializing it cannot fail")
}

#[cfg(test)]
#[path = "agentapi_tests.rs"]
mod agentapi_tests;
