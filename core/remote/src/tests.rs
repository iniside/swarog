use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

// ---- Fake transport for the redial-once logic --------------------------

#[test]
fn edge_failures_map_to_provenance_before_status_erasure() {
    let cases = [
        (
            edge::Error::Connection("lost".into()),
            FailureProvenance::ConnectionFatal,
            opsapi::Status::Unavailable,
        ),
        (
            edge::Error::Remote("handler failed".into()),
            FailureProvenance::PeerAnswer,
            opsapi::Status::Unavailable,
        ),
        (
            edge::Error::UnknownMethod("edge: unknown method fake".into()),
            FailureProvenance::PeerAnswer,
            opsapi::Status::NotFound,
        ),
        (
            edge::Error::Stream("stopped".into()),
            FailureProvenance::StreamLocal,
            opsapi::Status::Unavailable,
        ),
        (
            edge::Error::FrameTooLarge { size: 2, max: 1 },
            FailureProvenance::StreamLocal,
            opsapi::Status::Unavailable,
        ),
        (
            edge::Error::Connect("unprovenanced at call seam".into()),
            FailureProvenance::StreamLocal,
            opsapi::Status::Unavailable,
        ),
    ];

    for (failure, provenance, status) in cases {
        let mapped = map_edge_call_failure(failure);
        assert_eq!(mapped.provenance, provenance);
        assert_eq!(mapped.mapped.status, status);
    }
}

#[derive(Clone, Copy)]
struct FakeFailure {
    status: opsapi::Status,
    provenance: FailureProvenance,
}

/// A fake connection: `ok` decides whether its call succeeds; a failing call carries
/// independently selected mapped status and provenance. Shared counters record calls
/// and closes so tests can prove retry/reset policy without inferring from status.
struct FakeConn {
    ok: bool,
    failure: FakeFailure,
    closes: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Conn for FakeConn {
    async fn call(
        &self,
        _method: &str,
        _identity: Option<&str>,
        _payload: &[u8],
    ) -> Result<Vec<u8>, CallFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.ok {
            Ok(b"ok".to_vec())
        } else {
            Err(CallFailure {
                mapped: Error::new(self.failure.status, "fake: call failed"),
                provenance: self.failure.provenance,
            })
        }
    }
    fn close(&self) {
        self.closes.fetch_add(1, Ordering::SeqCst);
    }
}

/// A fake dialer: the Nth dial (0-based) yields a conn whose `call` succeeds iff
/// `N + 1 >= heal_after`; a failing conn returns `failure`. `dials` counts how many
/// times it was asked to dial.
struct FakeDialer {
    dials: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    heal_after: usize,
    failure: FakeFailure,
}

#[async_trait]
impl Dialer for FakeDialer {
    async fn dial(&self) -> Result<Arc<dyn Conn>, Error> {
        let n = self.dials.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(FakeConn {
            ok: n + 1 >= self.heal_after,
            failure: self.failure,
            closes: self.closes.clone(),
            calls: self.calls.clone(),
        }))
    }
}

fn reconnecting(
    heal_after: usize,
) -> (Reconnecting<FakeDialer>, Arc<AtomicUsize>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    reconnecting_failing_with(
        heal_after,
        FakeFailure {
            status: opsapi::Status::Unavailable,
            provenance: FailureProvenance::ConnectionFatal,
        },
    )
}

/// Like [`reconnecting`], but failing conns return an explicit mapped status and
/// provenance.
fn reconnecting_failing_with(
    heal_after: usize,
    failure: FakeFailure,
) -> (Reconnecting<FakeDialer>, Arc<AtomicUsize>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let dials = Arc::new(AtomicUsize::new(0));
    let closes = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let r = Reconnecting::new(FakeDialer {
        dials: dials.clone(),
        closes: closes.clone(),
        calls: calls.clone(),
        heal_after,
        failure,
    });
    (r, dials, closes, calls)
}

/// A healthy first connection: one dial, one call, no redial.
#[tokio::test]
async fn healthy_call_does_not_redial() {
    let (r, dials, closes, _) = reconnecting(1); // dial #0 → ok
    let out = r
        .call("characters.ownerOf", None, b"{}", RetryMode::OnceAfterReconnect)
        .await
        .unwrap();
    assert_eq!(out, b"ok");
    assert_eq!(dials.load(Ordering::SeqCst), 1, "must not redial a healthy conn");
    assert_eq!(closes.load(Ordering::SeqCst), 0);
}

/// A dead first connection heals on the SINGLE retry: the first call fails, the
/// conn is reset (closed) and re-dialed, and the retry succeeds — exactly two dials.
#[tokio::test]
async fn redials_once_and_succeeds() {
    let (r, dials, closes, _) = reconnecting(2); // dial #0 → dead, dial #1 → ok
    let out = r
        .call("characters.ownerOf", None, b"{}", RetryMode::OnceAfterReconnect)
        .await
        .unwrap();
    assert_eq!(out, b"ok");
    assert_eq!(dials.load(Ordering::SeqCst), 2, "exactly one reconnect");
    assert_eq!(closes.load(Ordering::SeqCst), 1, "the dead conn was closed on reset");
}

/// A persistently dead peer: the first call fails, one reconnect is attempted, the
/// retry also fails — the error propagates and there is NO third dial. BOTH dead
/// conns are reset (closes == 2): a dead c2 must not stay cached for the next request.
#[tokio::test]
async fn gives_up_after_one_retry() {
    let (r, dials, closes, _) = reconnecting(usize::MAX); // every conn dead
    let err = r
        .call("characters.ownerOf", None, b"{}", RetryMode::OnceAfterReconnect)
        .await
        .unwrap_err();
    assert_eq!(err.status, opsapi::Status::Unavailable);
    assert_eq!(dials.load(Ordering::SeqCst), 2, "one initial dial + one retry, no more");
    assert_eq!(closes.load(Ordering::SeqCst), 2, "BOTH dead conns were closed (c2 too)");
}

/// Peer-answer provenance preserves the shared connection regardless of mapped status:
/// `Remote` maps to Unavailable while `UnknownMethod` maps to NotFound, but neither may
/// reset or replay in either retry mode.
#[tokio::test]
async fn peer_answers_do_not_reset_or_replay() {
    for status in [opsapi::Status::Unavailable, opsapi::Status::NotFound] {
        for mode in [RetryMode::Never, RetryMode::OnceAfterReconnect] {
            let (r, dials, closes, calls) = reconnecting_failing_with(
                usize::MAX,
                FakeFailure {
                    status,
                    provenance: FailureProvenance::PeerAnswer,
                },
            );
            let err = r
                .call("characters.ownerOf", None, b"{}", mode)
                .await
                .unwrap_err();
            assert_eq!(err.status, status, "mapped peer answer returned ({mode:?})");
            assert_eq!(dials.load(Ordering::SeqCst), 1, "no peer-answer redial ({mode:?})");
            assert_eq!(closes.load(Ordering::SeqCst), 0, "peer answer keeps conn ({mode:?})");
            assert_eq!(calls.load(Ordering::SeqCst), 1, "no peer-answer replay ({mode:?})");
        }
    }
}

/// Stream-local provenance also preserves the connection and never replays. Use
/// `Internal` deliberately: status is independent from provenance in both directions.
#[tokio::test]
async fn stream_local_failures_do_not_reset_or_replay() {
    for mode in [RetryMode::Never, RetryMode::OnceAfterReconnect] {
        let (r, dials, closes, calls) = reconnecting_failing_with(
            usize::MAX,
            FakeFailure {
                status: opsapi::Status::Internal,
                provenance: FailureProvenance::StreamLocal,
            },
        );
        let err = r
            .call("characters.create", None, b"{}", mode)
            .await
            .unwrap_err();
        assert_eq!(err.status, opsapi::Status::Internal);
        assert_eq!(dials.load(Ordering::SeqCst), 1, "no stream-local redial ({mode:?})");
        assert_eq!(closes.load(Ordering::SeqCst), 0, "stream-local keeps conn ({mode:?})");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no stream-local replay ({mode:?})");
    }
}

/// `close` drops and closes the cached connection.
#[tokio::test]
async fn close_closes_cached_conn() {
    let (r, _dials, closes, _) = reconnecting(1);
    r.call("characters.ownerOf", None, b"{}", RetryMode::Never)
        .await
        .unwrap(); // caches a conn
    r.close().await;
    assert_eq!(closes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unsafe_failure_resets_without_replaying_and_next_request_redials() {
    let (r, dials, closes, calls) = reconnecting(2);
    let err = r
        .call("characters.create", None, b"{}", RetryMode::Never)
        .await
        .unwrap_err();
    assert_eq!(err.status, opsapi::Status::Unavailable);
    assert_eq!(dials.load(Ordering::SeqCst), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "unsafe call must not replay");
    assert_eq!(closes.load(Ordering::SeqCst), 1, "failed connection is still reset");

    let out = r
        .call("characters.create", None, b"{}", RetryMode::Never)
        .await
        .unwrap();
    assert_eq!(out, b"ok");
    assert_eq!(dials.load(Ordering::SeqCst), 2, "next independent request redials");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// A fake dialer whose dial #0 connection fails one way and every later connection
/// fails another, so replay behavior can be tested independently of mapped status.
struct TwoFailureDialer {
    dials: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    first: FakeFailure,
    second: FakeFailure,
}

#[async_trait]
impl Dialer for TwoFailureDialer {
    async fn dial(&self) -> Result<Arc<dyn Conn>, Error> {
        let n = self.dials.fetch_add(1, Ordering::SeqCst);
        let failure = if n == 0 { self.first } else { self.second };
        Ok(Arc::new(FakeConn {
            ok: false,
            failure,
            closes: self.closes.clone(),
            calls: self.calls.clone(),
        }))
    }
}

/// A fatal first attempt triggers the one replay. If c2 then reports a stream-local
/// failure or peer answer, it stays cached; a following request reuses it without a
/// third dial. Statuses are deliberately varied to prove provenance is authoritative.
#[tokio::test]
async fn nonfatal_second_attempt_keeps_fresh_connection_cached() {
    for second in [
        FakeFailure {
            status: opsapi::Status::Internal,
            provenance: FailureProvenance::StreamLocal,
        },
        FakeFailure {
            status: opsapi::Status::NotFound,
            provenance: FailureProvenance::PeerAnswer,
        },
    ] {
        let dials = Arc::new(AtomicUsize::new(0));
        let closes = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let r = Reconnecting::new(TwoFailureDialer {
            dials: dials.clone(),
            closes: closes.clone(),
            calls: calls.clone(),
            first: FakeFailure {
                status: opsapi::Status::Unavailable,
                provenance: FailureProvenance::ConnectionFatal,
            },
            second,
        });
        let err = r
            .call("characters.ownerOf", None, b"{}", RetryMode::OnceAfterReconnect)
            .await
            .unwrap_err();
        assert_eq!(err.status, second.status);
        assert_eq!(dials.load(Ordering::SeqCst), 2, "initial dial plus one reconnect");
        assert_eq!(closes.load(Ordering::SeqCst), 1, "only fatal c1 closes");

        let again = r
            .call("characters.ownerOf", None, b"{}", RetryMode::Never)
            .await
            .unwrap_err();
        assert_eq!(again.status, second.status);
        assert_eq!(dials.load(Ordering::SeqCst), 2, "following call reuses c2");
        assert_eq!(closes.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
}

/// One coordinated fake connection represents concurrent QUIC streams without
/// duplicating a transport fixture: one call parks while another fails stream-locally.
/// The parked call and a follow-up call must both retain the same cached connection.
struct CoordinatedConn {
    closes: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    parked: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Semaphore>,
}

#[async_trait]
impl Conn for CoordinatedConn {
    async fn call(
        &self,
        method: &str,
        _identity: Option<&str>,
        _payload: &[u8],
    ) -> Result<Vec<u8>, CallFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match method {
            "park" => {
                self.parked.add_permits(1);
                self.release
                    .acquire()
                    .await
                    .expect("test release semaphore stays open")
                    .forget();
                Ok(b"parked-ok".to_vec())
            }
            "stream-local" => Err(CallFailure {
                mapped: Error::unavailable("fake stream cancelled"),
                provenance: FailureProvenance::StreamLocal,
            }),
            _ => Ok(b"ok".to_vec()),
        }
    }

    fn close(&self) {
        self.closes.fetch_add(1, Ordering::SeqCst);
    }
}

struct CoordinatedDialer {
    dials: Arc<AtomicUsize>,
    conn: Arc<CoordinatedConn>,
}

#[async_trait]
impl Dialer for CoordinatedDialer {
    async fn dial(&self) -> Result<Arc<dyn Conn>, Error> {
        self.dials.fetch_add(1, Ordering::SeqCst);
        Ok(self.conn.clone())
    }
}

#[tokio::test]
async fn stream_local_failure_preserves_concurrent_call_and_cached_connection() {
    let dials = Arc::new(AtomicUsize::new(0));
    let closes = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let parked_signal = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let conn = Arc::new(CoordinatedConn {
        closes: closes.clone(),
        calls: calls.clone(),
        parked: parked_signal.clone(),
        release: release.clone(),
    });
    let reconnecting = Arc::new(Reconnecting::new(CoordinatedDialer {
        dials: dials.clone(),
        conn,
    }));

    let parked = tokio::spawn({
        let reconnecting = reconnecting.clone();
        async move { reconnecting.call("park", None, b"{}", RetryMode::Never).await }
    });
    parked_signal
        .acquire()
        .await
        .expect("parked signal semaphore stays open")
        .forget();

    let failure = reconnecting
        .call(
            "stream-local",
            None,
            b"{}",
            RetryMode::OnceAfterReconnect,
        )
        .await
        .unwrap_err();
    assert_eq!(failure.status, opsapi::Status::Unavailable);
    assert_eq!(dials.load(Ordering::SeqCst), 1);
    assert_eq!(closes.load(Ordering::SeqCst), 0);

    release.add_permits(1);
    assert_eq!(parked.await.unwrap().unwrap(), b"parked-ok");
    assert_eq!(
        reconnecting
            .call("after", None, b"{}", RetryMode::Never)
            .await
            .unwrap(),
        b"ok"
    );
    assert_eq!(dials.load(Ordering::SeqCst), 1, "follow-up reuses shared connection");
    assert_eq!(closes.load(Ordering::SeqCst), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

// ---- The injected-factory swap: register runs every factory --------------
//
// `remote` is generic and imports no `api/` crate, so these tests use LOCAL fake
// factories rather than the real `<name>rpc::remote_factories()` (whose correctness
// is covered by the glue crates + split-proof). A fake factory provides a fake
// capability under a registry key and bumps a shared counter, so we can assert both
// that `register` invoked EVERY factory and that the swap reached the registry.

/// A fake capability — the stand-in for a domain trait like `charactersapi::Ownership`.
trait FakeCap: Send + Sync {}
struct FakeImpl;
impl FakeCap for FakeImpl {}

/// The stub applies EVERY injected factory in `register` (topology-blind, no dial):
/// both factories run (the counter reaches 2) and the capability one lands in the
/// registry under its key — exactly what a real provider swap does.
#[test]
fn stub_runs_every_injected_factory() {
    let ctx = Context::new(); // DB-less: register only touches the registry
    let hits = Arc::new(AtomicUsize::new(0));

    let h1 = hits.clone();
    let h2 = hits.clone();
    let factories: Vec<RemoteFactory> = vec![
        Box::new(move |ctx: &Context, _caller| {
            h1.fetch_add(1, Ordering::SeqCst);
            let cap: Arc<dyn FakeCap> = Arc::new(FakeImpl);
            ctx.registry().provide::<dyn FakeCap>(registry::key("fake", "cap"), cap);
        }),
        Box::new(move |_ctx: &Context, _caller| {
            h2.fetch_add(1, Ordering::SeqCst);
        }),
    ];

    let stub = Stub::new("fake", "127.0.0.1:9000", factories);
    assert_eq!(stub.name(), "fake", "name is the PROVIDER name for validate_requires");
    assert!(stub.requires().is_empty());
    stub.register(&ctx).unwrap();

    assert_eq!(hits.load(Ordering::SeqCst), 2, "register must run every injected factory");
    assert!(
        ctx.registry()
            .try_require::<dyn FakeCap>(&registry::key("fake", "cap"))
            .is_some(),
        "the capability factory's provide must reach the registry"
    );
}

/// A stub with ZERO factories is a wiring bug (nothing to provide): `register` fails
/// loudly rather than registering an inert module — preserving the fail-loud
/// guarantee the old per-provider `match`'s unknown-provider arm gave.
#[test]
fn stub_with_no_factories_fails_loud() {
    let ctx = Context::new();
    let stub = Stub::new("fake", "127.0.0.1:9000", Vec::new());
    let err = stub.register(&ctx).unwrap_err();
    assert!(err.to_string().contains("zero factories"), "{err}");
}

// ---- The per-stub readiness probe (the `/readyz` contribution) -----------
//
// `probe_peer` backs each stub's `httpmw::ReadyCheck`. It dials the peer's QUIC edge
// with a 1s inner bound, so a dead peer errs FAST (not after the outer READY_CHECK
// bound) and a live edge answers Ok. These exercise the real `edge` transport (already
// a dependency), so no fake is needed — the point is the bounded dial itself.

/// An unreachable peer: the probe returns `Err` well within its own 1s bound (a rejected
/// connection returns fast; even a silent drop is capped at 1s). Asserting elapsed < 2s
/// proves the inner timeout owns the dial — it never waits on the outer readyz bound.
#[tokio::test]
async fn probe_unreachable_peer_errs_fast() {
    let started = std::time::Instant::now();
    // 127.0.0.1:1 — a privileged port nothing listens on: connect is refused/dropped.
    let out = probe_peer("127.0.0.1:1".to_string()).await;
    let elapsed = started.elapsed();
    assert!(out.is_err(), "an unreachable peer must fail the readiness probe: {out:?}");
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "the probe's own 1s bound must fire, not the outer readyz bound (took {elapsed:?})"
    );
}

/// A bad peer address never dials at all — it fails at parse, instantly.
#[tokio::test]
async fn probe_bad_addr_errs_at_parse() {
    let err = probe_peer("not-an-addr".to_string()).await.unwrap_err();
    assert!(err.contains("bad peer edge addr"), "{err}");
}

/// A LIVE edge: a real `edge::Server` listening on loopback with the process's shared
/// dev CA — the SAME anchor `probe_peer` resolves internally — so the mTLS handshake
/// completes and the probe reports ready.
#[tokio::test]
async fn probe_live_edge_reports_ready() {
    // The server listens with the shared anchor the probe also dials with; an empty
    // handler set is fine — the probe only completes the handshake, it makes no call.
    let ca = edge::shared_dev_ca().expect("shared dev CA");
    let srv = edge::Server::new();
    let running = srv
        .listen(std::net::SocketAddr::from(([127, 0, 0, 1], 0)), &ca)
        .expect("listen on loopback");

    let out = probe_peer(running.local_addr().to_string()).await;
    assert!(out.is_ok(), "a live edge must pass the readiness probe: {out:?}");

    running.close();
}

// ---- Bounded RemoteBoot hooks (Step 11) -----------------------------------
//
// `Stub::start` used to await each `RemoteBoot` hook unbounded. A hung hook (e.g.
// `configrpc`'s `CachedConfig` boot-fill against a peer that accepts the QUIC
// connection but never answers the call) pinned process startup forever, and
// because `App::start` awaits module starts sequentially, every module started
// after the stub never got a chance to run either.
//
// The first test drives a REAL hanging peer: a live `edge::Server` whose handler
// never resolves, called through a real `edge::Client` — the same await the
// production defect crosses, not a bare closure fake. The crate's existing
// `probe_peer` tests already establish this real-edge-server pattern is a normal,
// cheap seam in this test module.

/// A stub with a single `RemoteBoot` hook that makes a REAL edge call to a live
/// server whose handler never completes. `start_with_boot_timeout` must return
/// `Err` within a bound well short of the real hang, and the error must name both
/// the provider and the timeout duration.
#[tokio::test]
async fn hung_boot_hook_times_out_naming_provider_and_bound() {
    let ca = edge::shared_dev_ca().expect("shared dev CA");
    let mut srv = edge::Server::new();
    // A handler that never resolves — the stand-in for a peer that accepted the
    // connection but is not answering.
    srv.handle(
        "hang",
        Arc::new(|_payload: Vec<u8>| Box::pin(std::future::pending())),
    );
    let running = srv
        .listen(std::net::SocketAddr::from(([127, 0, 0, 1], 0)), &ca)
        .expect("listen on loopback");
    let addr = running.local_addr();

    let ctx = Context::new();
    let boot_calls = Arc::new(AtomicUsize::new(0));
    let hook_calls = boot_calls.clone();
    ctx.contribute(
        BOOT_SLOT,
        RemoteBoot::new("hangy", move || {
            let addr = addr;
            let hook_calls = hook_calls.clone();
            Box::pin(async move {
                hook_calls.fetch_add(1, Ordering::SeqCst);
                let client = edge::Client::dial(addr, &edge::shared_dev_ca().unwrap())
                    .await
                    .map_err(|e| anyhow::anyhow!("dial: {e}"))?;
                client
                    .call_raw("hang", b"{}")
                    .await
                    .map_err(|e| anyhow::anyhow!("call: {e}"))?;
                Ok(())
            })
        }),
    );

    let stub = Stub::new("hangy", &addr.to_string(), vec![Box::new(|_ctx, _caller| {})]);
    let short_bound = Duration::from_millis(200);

    let started = std::time::Instant::now();
    let err = stub
        .start_with_boot_timeout(&ctx, short_bound)
        .await
        .unwrap_err();
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "the injected bound must fire, not the real (unbounded) hang: {elapsed:?}"
    );
    let msg = err.to_string();
    assert!(msg.contains("hangy"), "error must name the provider: {msg}");
    assert!(
        msg.contains("200ms"),
        "error must mention the configured timeout: {msg}"
    );
    assert_eq!(
        boot_calls.load(Ordering::SeqCst),
        1,
        "the hook ran exactly once before hanging"
    );

    running.close();
}

/// A fast hook still runs exactly once and `start` succeeds — the timeout wrapper
/// must not change the happy path.
#[tokio::test]
async fn fast_boot_hook_runs_once_and_succeeds() {
    let ctx = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let hook_calls = calls.clone();
    ctx.contribute(
        BOOT_SLOT,
        RemoteBoot::new("fast", move || {
            let hook_calls = hook_calls.clone();
            Box::pin(async move {
                hook_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }),
    );

    let stub = Stub::new("fast", "127.0.0.1:9000", vec![Box::new(|_ctx, _caller| {})]);
    stub.start_with_boot_timeout(&ctx, Duration::from_secs(5))
        .await
        .unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 1, "the hook ran exactly once");
}

/// A hook's own `Err` (not a timeout) keeps its existing context, unaffected by the
/// new timeout wrapper.
#[tokio::test]
async fn failing_boot_hook_keeps_its_own_error_context() {
    let ctx = Context::new();
    ctx.contribute(
        BOOT_SLOT,
        RemoteBoot::new("broken", || {
            Box::pin(async move { Err(anyhow::anyhow!("peer said no")) })
        }),
    );

    let stub = Stub::new(
        "broken",
        "127.0.0.1:9000",
        vec![Box::new(|_ctx, _caller| {})],
    );
    let err = stub
        .start_with_boot_timeout(&ctx, Duration::from_secs(5))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("broken"), "{msg}");
    assert!(format!("{err:#}").contains("peer said no"), "{err:#}");
}
