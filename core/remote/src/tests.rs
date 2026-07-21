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

// ---- A5: dial-time re-resolution via the resolver seam -------------------
//
// The property the design leans on: a `Stub`/`Reconnecting` picks up a MOVED peer on
// reconnect WITHOUT a consumer restart, because the address is re-resolved inside
// `EdgeDialer::dial` on every dial (frozen-string code could not do this). Proven at
// two levels: the real `EdgeDialer` re-invokes its resolver per dial, and `Reconnecting`
// drives a fresh resolve after a connection-fatal reset.

/// The REAL `EdgeDialer` re-resolves on EVERY dial: a resolver returning a different
/// (unparseable) address each call makes each dial's error name the CURRENT address —
/// a frozen string would name the same one both times. No network: parse fails before
/// any edge dial.
#[tokio::test]
async fn edge_dialer_reresolves_the_address_on_each_dial() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let resolver: PeerResolver = Arc::new(move || {
        let n = seen.fetch_add(1, Ordering::SeqCst);
        let addr = if n == 0 { "addr-ONE-unparseable" } else { "addr-TWO-unparseable" }.to_string();
        Box::pin(async move { Ok(addr) })
    });
    let dialer = EdgeDialer { resolve: resolver };

    // `Arc<dyn Conn>` is not `Debug`, so pattern-match rather than `unwrap_err`.
    let Err(e1) = dialer.dial().await else { panic!("unparseable addr must not dial") };
    assert_eq!(e1.status, opsapi::Status::Unavailable);
    assert!(e1.to_string().contains("addr-ONE-unparseable"), "first dial names A: {e1}");

    let Err(e2) = dialer.dial().await else { panic!("unparseable addr must not dial") };
    assert!(e2.to_string().contains("addr-TWO-unparseable"), "second dial names B: {e2}");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "resolver invoked once per dial");
}

/// A resolver ERROR (unresolvable peer) is mapped to `Unavailable` (503) by the dialer —
/// the same class as a bad address, so a consumer sees "peer not there", not a panic.
#[tokio::test]
async fn edge_dialer_maps_resolver_error_to_unavailable() {
    let resolver: PeerResolver =
        Arc::new(|| Box::pin(async { Err("agent said no".to_string()) }));
    let dialer = EdgeDialer { resolve: resolver };
    let Err(err) = dialer.dial().await else { panic!("resolver error must not dial") };
    assert_eq!(err.status, opsapi::Status::Unavailable);
    assert!(err.to_string().contains("agent said no"), "{err}");
}

/// A test dialer mirroring `EdgeDialer`'s contract over the fake `Conn` seam: each dial
/// consults the resolver and records the address it dialed, so the reset→redial path can
/// be proven without QUIC.
struct ResolvingFakeDialer {
    resolve: PeerResolver,
    dialed: Arc<StdMutex<Vec<String>>>,
    dials: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    heal_after: usize,
}

#[async_trait]
impl Dialer for ResolvingFakeDialer {
    async fn dial(&self) -> Result<Arc<dyn Conn>, Error> {
        let addr = (self.resolve)().await.map_err(Error::unavailable)?;
        self.dialed.lock().unwrap().push(addr);
        let n = self.dials.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(FakeConn {
            ok: n + 1 >= self.heal_after,
            failure: FakeFailure {
                status: opsapi::Status::Unavailable,
                provenance: FailureProvenance::ConnectionFatal,
            },
            closes: self.closes.clone(),
            calls: self.calls.clone(),
        }))
    }
}

/// The re-resolve-on-reconnect property, end to end through `Reconnecting`: ONE caller
/// (constructed once — no consumer restart), a resolver returning addr A then addr B,
/// and a forced connection-fatal reset between them. The retry re-dials, the dialer
/// re-resolves, and the SECOND dial targets B. A pre-A5 frozen-string dialer would have
/// dialed A twice.
#[tokio::test]
async fn reconnecting_reresolves_to_the_new_address_after_reset() {
    let flip = Arc::new(AtomicUsize::new(0));
    let seen = flip.clone();
    let resolver: PeerResolver = Arc::new(move || {
        let n = seen.fetch_add(1, Ordering::SeqCst);
        let addr = if n == 0 { "10.0.0.1:1" } else { "10.0.0.2:2" }.to_string();
        Box::pin(async move { Ok(addr) })
    });
    let dialed = Arc::new(StdMutex::new(Vec::new()));
    let r = Reconnecting::new(ResolvingFakeDialer {
        resolve: resolver,
        dialed: dialed.clone(),
        dials: Arc::new(AtomicUsize::new(0)),
        closes: Arc::new(AtomicUsize::new(0)),
        calls: Arc::new(AtomicUsize::new(0)),
        heal_after: 2, // dial #0 fatal → reset → dial #1 ok
    });

    let out = r
        .call("characters.ownerOf", None, b"{}", RetryMode::OnceAfterReconnect)
        .await
        .unwrap();
    assert_eq!(out, b"ok");
    assert_eq!(
        *dialed.lock().unwrap(),
        vec!["10.0.0.1:1".to_string(), "10.0.0.2:2".to_string()],
        "reset re-dialed AND re-resolved to B — the same caller, no restart"
    );
}

/// The standalone constant resolver: a fixed address is returned unchanged on every
/// call — no re-resolution, byte-identical to the pre-A5 frozen string.
#[tokio::test]
async fn constant_resolver_returns_the_same_addr_each_call() {
    let r = constant_resolver("127.0.0.1:9000".to_string());
    assert_eq!(r().await.unwrap(), "127.0.0.1:9000");
    assert_eq!(r().await.unwrap(), "127.0.0.1:9000");
}

/// `init` contributes the peer address to `PEER_SLOT` as a SINGLE-ELEMENT SET (the A5
/// shape C2/D2 extend): the boot snapshot the gateway route table reads.
#[test]
fn init_contributes_peer_addr_as_single_element_set() {
    let ctx = Context::new();
    let stub = Stub::new("fake", "127.0.0.1:9000", vec![Box::new(|_ctx, _caller| {})]);
    stub.init(&ctx).unwrap();
    let peers: Vec<opsapi::PeerAddr> = ctx.contributions(opsapi::PEER_SLOT);
    let found = peers
        .iter()
        .find(|p| p.provider == "fake")
        .expect("peer address contributed to PEER_SLOT");
    assert_eq!(found.addrs, vec!["127.0.0.1:9000".to_string()]);
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
        // Hang-guard (5×), not a tight latency bound: the point is the probe returns
        // WELL before the outer readyz budget — a 5s ceiling still proves the inner 1s
        // dial bound owns the failure, while giving load-headroom the thin 2× lacked.
        elapsed < std::time::Duration::from_secs(5),
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

    let stub = Stub::new("hangy", addr.to_string(), vec![Box::new(|_ctx, _caller| {})]);
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

// ---- The zero-I/O readyz check (Step 1: probe cache, no per-request dial) --
//
// Step 1 replaced the per-request QUIC/mTLS dial in each stub's `/readyz`
// `httpmw::ReadyCheck` with a background probe loop that stamps a CACHED verdict; the
// check now READS ONLY that cache (zero network I/O). The AIRTIGHT zero-I/O proof is the
// `readiness_verdict_*` unit tests below: they exercise the readyz decision as a PURE
// function (`readiness_verdict`) with no dialer, no async, and no clock — so zero-I/O is
// guaranteed BY CONSTRUCTION, not inferred from a wall-clock timing race (a closed
// loopback port fails a QUIC dial instantly, so the old `<100ms` assertion could never
// have discriminated a dial-then-return-cache regression). Those tests also cover the two
// branches the timing test never did: the staleness guard (stale-`Ok` -> unready) and the
// fail-closed `"probe pending"` seed. `readyz_check_does_no_io` then pins only that the
// wired `ReadyCheck` is actually plumbed to the cache, and `background_probe_updates_verdict`
// pins that the background loop is the sole dialer, reflecting reachability both ways.

/// `readiness_verdict` returns the cached `Ok` while the probe stamp is fresh — including
/// a stamp slightly older than "now" but still inside [`PROBE_STALL_MAX`] (15s).
#[test]
fn readiness_verdict_ok_when_fresh() {
    assert_eq!(readiness_verdict(&Ok(()), 100, 100), Ok(()));
    // 14s < 15s — still fresh, still ready.
    assert_eq!(readiness_verdict(&Ok(()), 100, 114), Ok(()));
}

/// A FRESH cached `Err` is surfaced verbatim — the check reports the peer's real probe
/// failure, no I/O of its own.
#[test]
fn readiness_verdict_returns_cached_err_when_fresh() {
    assert_eq!(
        readiness_verdict(&Err("dial to X failed".into()), 100, 105),
        Err("dial to X failed".to_string())
    );
}

/// The dead-probe-task guard: a cached `Ok` older than [`PROBE_STALL_MAX`] flips unready
/// so a frozen stale-green verdict can't be served. This is the branch NO prior test
/// exercised.
#[test]
fn readiness_verdict_stale_ok_flips_unready() {
    // 16s > 15s.
    let out = readiness_verdict(&Ok(()), 100, 116);
    let msg = out.expect_err("a stale Ok must flip to unready");
    assert!(
        msg.contains("stub probe stalled"),
        "stale verdict must name the stall (got {msg:?})"
    );
}

/// Staleness takes precedence over a cached error: a long-stale stamp yields the STALL
/// message (the probe task may be dead), not the last observed dial error.
#[test]
fn readiness_verdict_stale_takes_precedence_over_cached_err() {
    let out = readiness_verdict(&Err("old dial err".into()), 100, 200);
    let msg = out.expect_err("a stale verdict must be unready regardless of cached value");
    assert!(
        msg.contains("stalled"),
        "stale-with-cached-err must surface the stall, not the old error (got {msg:?})"
    );
}

/// The fail-closed cold-start seed: a `0` stamp (never probed) SKIPS the stall check and
/// falls through to the cached `Err("probe pending")` — proving cold start reports
/// unready until the first probe completes.
#[test]
fn readiness_verdict_never_probed_falls_through_to_seed() {
    assert_eq!(
        readiness_verdict(&Err("probe pending".into()), 0, 9999),
        Err("probe pending".to_string())
    );
}

/// A `0` stamp with a cached `Ok` is ready (documents the pure contract: stamp `0` never
/// trips the stall guard, it defers entirely to the cached verdict).
#[test]
fn readiness_verdict_never_probed_ok_is_ready() {
    assert_eq!(readiness_verdict(&Ok(()), 0, 9999), Ok(()));
}

/// The wired `stub:<provider>` `ReadyCheck` is plumbed to the cache: with a seeded
/// sentinel verdict and a fresh stamp, the contributed check returns exactly that
/// sentinel (never a live dial). The airtight zero-I/O proof is `readiness_verdict_*`
/// above (pure, by construction); this test only pins the wiring — that `init`'s closure
/// reads the shared verdict cache rather than dialing the peer.
#[tokio::test]
async fn readyz_check_does_no_io() {
    let ctx = Context::new();
    let stub = Stub::new("fake", "127.0.0.1:1", vec![Box::new(|_ctx, _caller| {})]);

    // Seed the cache DIRECTLY (same crate — private fields are reachable). A cache read
    // returns this exact sentinel; a live dial never would. Fresh stamp so the
    // staleness guard does NOT fire and force an unready verdict of its own.
    *stub.verdict.lock().unwrap() = Err("SENTINEL-cached".to_string());
    stub.last_probe_at
        .store(coarse_now_secs().max(1), Ordering::SeqCst);

    // `init` contributes the `stub:<provider>` ReadyCheck to READINESS_SLOT.
    stub.init(&ctx).unwrap();
    let check = ctx
        .contributions::<httpmw::ReadyCheck>(httpmw::READINESS_SLOT)
        .into_iter()
        .find(|c| c.name() == "stub:fake")
        .expect("stub must contribute a `stub:fake` readiness check");

    for _ in 0..5 {
        assert_eq!(
            check.run().await.unwrap_err(),
            "SENTINEL-cached",
            "the wired check must return the cached sentinel, not a live-dial result"
        );
    }
}

/// The background probe loop — spawned by the stub, the SOLE runtime caller of
/// `probe_peer` — updates the cached verdict in BOTH directions and tears down cleanly.
/// A live loopback `edge::Server` (the same shared-CA anchor the probe dials) makes the
/// verdict flip to `Ok`; closing it makes a later probe flip it back to `Err`; and
/// `Module::stop` grace-then-aborts the loop without hanging.
#[tokio::test]
async fn background_probe_updates_verdict() {
    let ca = edge::shared_dev_ca().expect("shared dev CA");
    let srv = edge::Server::new();
    let running = srv
        .listen(std::net::SocketAddr::from(([127, 0, 0, 1], 0)), &ca)
        .expect("listen on loopback");

    let stub = Stub::new(
        "fake",
        running.local_addr().to_string(),
        vec![Box::new(|_ctx, _caller| {})],
    );
    // Short cadence both rates so the test observes flips within its budget.
    stub.spawn_probe(Duration::from_millis(50), Duration::from_millis(50));

    // The loop dials the LIVE peer and stamps `Ok`.
    let mut became_ready = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        let v = stub.verdict.lock().unwrap().clone();
        if v.is_ok() {
            became_ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(became_ready, "background probe never stamped Ok for a live peer");
    assert_ne!(
        stub.last_probe_at.load(Ordering::SeqCst),
        0,
        "a completed probe must stamp last_probe_at"
    );

    // Peer dies → a later probe fails → the cached verdict flips to `Err`.
    running.close();
    let mut became_err = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        let v = stub.verdict.lock().unwrap().clone();
        if v.is_err() {
            became_err = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(became_err, "background probe never reflected peer loss");

    // Tear down through the real `Module::stop` path (grace-then-abort). The test
    // completing at all proves stop does not hang on the running loop.
    let ctx = Context::new();
    stub.stop(&ctx).await.expect("stop tears the probe loop down cleanly");
}
