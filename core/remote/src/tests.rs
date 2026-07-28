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

/// A stub with ZERO factories via `Stub::new` is a wiring bug (nothing to provide):
/// `register` fails loudly rather than registering an inert module — preserving the fail-loud
/// guarantee the old per-provider `match`'s unknown-provider arm gave. (The INTENTIONAL
/// peer-only case has its own constructor — see `describe_peer_stub_registers_and_lands_in_peer_slot`.)
#[test]
fn stub_with_no_factories_fails_loud() {
    let ctx = Context::new();
    let stub = Stub::new("fake", "127.0.0.1:9000", Vec::new());
    let err = stub.register(&ctx).unwrap_err();
    assert!(err.to_string().contains("zero factories"), "{err}");
}

/// The D2 peer-only stub (`Stub::describe_peer`): `register` SUCCEEDS with zero factories
/// (it is NOT the accidental-empty wiring bug — it provides nothing by design), and `init`
/// STILL contributes the peer address to `PEER_SLOT` so a describe-driven gateway iterates it
/// and fetches this peer's `__describe`. This is the at-risk path the D2 in-process tests
/// missed: the previous `Stub::new(p, a, vec![])` wiring bailed at `register`, so gateway-svc
/// never reached `init` and never landed its `PEER_SLOT` entries — a real boot failure.
#[test]
fn describe_peer_stub_registers_and_lands_in_peer_slot() {
    let ctx = Context::new();
    let stub = Stub::describe_peer("characters", "127.0.0.1:9000");
    assert_eq!(stub.name(), "characters", "name is the provider name");
    // register: no bail (peer-only), and it provides nothing.
    stub.register(&ctx).expect("a peer-only stub's register must succeed with zero factories");
    // init: the PEER_SLOT contribution still happens — the whole reason the stub exists.
    stub.init(&ctx).expect("peer-only init");
    let peers: Vec<opsapi::PeerAddr> = ctx.contributions(opsapi::PEER_SLOT);
    let found = peers
        .iter()
        .find(|p| p.provider == "characters")
        .expect("a peer-only stub must still contribute its PeerAddr to PEER_SLOT");
    assert_eq!(found.addrs, vec!["127.0.0.1:9000".to_string()]);
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
    *stub.single_verdict().lock().unwrap() = Err("SENTINEL-cached".to_string());
    stub.single_last_probe_at()
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
        let v = stub.single_verdict().lock().unwrap().clone();
        if v.is_ok() {
            became_ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(became_ready, "background probe never stamped Ok for a live peer");
    assert_ne!(
        stub.single_last_probe_at().load(Ordering::SeqCst),
        0,
        "a completed probe must stamp last_probe_at"
    );

    // Peer dies → a later probe fails → the cached verdict flips to `Err`.
    running.close();
    let mut became_err = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        let v = stub.single_verdict().lock().unwrap().clone();
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

// ---- C1: the client-side round-robin connection pool ---------------------
//
// `Pool` holds ONE per-instance caller per resolved address and spreads calls across
// the healthy ones — the property `exactly_one`/a single conn structurally cannot give.
// These prove it with a FAKE per-instance transport (no QUIC, no probe tasks): a fake
// factory builds a `RecordingCaller` per addr (recording which instance received the
// call) with a preset health verdict, so distribution, skip-dead, empty, and set
// reconciliation are asserted by construction, not inferred.

/// Records per-addr call counts AND per-addr build counts across a pool's lifetime, so a
/// test can assert BOTH that traffic spread across instances and that a kept instance was
/// not rebuilt on refresh.
#[derive(Clone, Default)]
struct Recorder {
    calls: Arc<StdMutex<std::collections::HashMap<String, Arc<AtomicUsize>>>>,
    builds: Arc<StdMutex<std::collections::HashMap<String, Arc<AtomicUsize>>>>,
}

impl Recorder {
    fn call_counter(&self, addr: &str) -> Arc<AtomicUsize> {
        self.calls
            .lock()
            .unwrap()
            .entry(addr.to_string())
            .or_insert_with(|| Arc::new(AtomicUsize::new(0)))
            .clone()
    }
    fn hits(&self, addr: &str) -> usize {
        self.call_counter(addr).load(Ordering::SeqCst)
    }
    fn note_build(&self, addr: &str) {
        self.builds
            .lock()
            .unwrap()
            .entry(addr.to_string())
            .or_insert_with(|| Arc::new(AtomicUsize::new(0)))
            .fetch_add(1, Ordering::SeqCst);
    }
    fn builds(&self, addr: &str) -> usize {
        self.builds
            .lock()
            .unwrap()
            .get(addr)
            .map(|c| c.load(Ordering::SeqCst))
            .unwrap_or(0)
    }
}

/// A fake per-instance caller: records that THIS instance's address received the call
/// and echoes the address back as the response body, so a test can see which instance
/// served each request.
struct RecordingCaller {
    addr: String,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Caller for RecordingCaller {
    async fn call(
        &self,
        _method: &str,
        _identity: Option<&str>,
        _payload: &[u8],
        _retry_mode: RetryMode,
    ) -> Result<Vec<u8>, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.addr.clone().into_bytes())
    }
}

/// A fake pool instance factory (no QUIC, no probe task). Every instance is HEALTHY
/// (a completed `Ok` probe, fresh) unless its addr is in `dead`, in which case its
/// cached verdict is a fresh `Err` so both selection AND readyz treat it as down.
fn fake_factory(recorder: Recorder, dead: std::collections::HashSet<String>) -> InstanceFactory {
    Arc::new(move |addr: &str| {
        let addr = addr.to_string();
        recorder.note_build(&addr);
        let calls = recorder.call_counter(&addr);
        let caller: Arc<dyn Caller> = Arc::new(RecordingCaller {
            addr: addr.clone(),
            calls,
        });
        let health = Arc::new(InstanceHealth::seed());
        // Stamp a COMPLETED probe verdict so `healthy()`/`is_selectable()` see a definite
        // answer, not the fail-closed pending seed.
        if dead.contains(&addr) {
            *health.verdict.lock().unwrap() = Err("preset dead".to_string());
        } else {
            *health.verdict.lock().unwrap() = Ok(());
        }
        health
            .last_probe_at
            .store(coarse_now_secs().max(1), Ordering::SeqCst);
        let close: InstanceCloser = Arc::new(|| Box::pin(async {}));
        Instance {
            addr,
            caller,
            health,
            probe: None,
            close,
        }
    })
}

fn list_of(addrs: &[&str]) -> PeerListResolver {
    let addrs: Vec<String> = addrs.iter().map(|s| s.to_string()).collect();
    Arc::new(move || {
        let addrs = addrs.clone();
        Box::pin(async move { Ok(addrs) })
    })
}

/// The distribution property `exactly_one`/a single conn cannot provide: with a resolver
/// answering `[A, B]`, repeated calls SPREAD across BOTH instances round-robin (not always
/// the first), and every call is served by exactly one instance.
#[tokio::test]
async fn pool_distributes_round_robin_across_two_instances() {
    let recorder = Recorder::default();
    let pool = Pool::with_factory(
        list_of(&["A", "B"]),
        fake_factory(recorder.clone(), Default::default()),
    );

    for _ in 0..6 {
        pool.call("m", None, b"{}", RetryMode::Never).await.unwrap();
    }

    assert!(recorder.hits("A") > 0, "A must receive traffic (A={})", recorder.hits("A"));
    assert!(recorder.hits("B") > 0, "B must receive traffic (B={})", recorder.hits("B"));
    assert_eq!(
        recorder.hits("A") + recorder.hits("B"),
        6,
        "every call is served by exactly one instance"
    );
}

/// A dead instance is SKIPPED by selection: with `[A healthy, B dead]`, every call routes
/// to A (never B), and the pool stays READY because >= 1 instance is up.
#[tokio::test]
async fn pool_skips_dead_instance_and_stays_ready() {
    let recorder = Recorder::default();
    let mut dead = std::collections::HashSet::new();
    dead.insert("B".to_string());
    let pool = Pool::with_factory(list_of(&["A", "B"]), fake_factory(recorder.clone(), dead));

    for _ in 0..6 {
        let out = pool.call("m", None, b"{}", RetryMode::Never).await.unwrap();
        assert_eq!(out, b"A", "traffic must route only to the healthy instance A");
    }

    assert_eq!(recorder.hits("B"), 0, "the dead instance must never be selected");
    assert_eq!(recorder.hits("A"), 6);
    assert!(
        pool.readyz().is_ok(),
        "a pool with >= 1 healthy instance stays Ready (some-down, not all-down)"
    );
}

/// An empty instance list is Unavailable, not a panic — and the pool reports `/readyz`
/// down (all-down / none-resolved).
#[tokio::test]
async fn pool_empty_list_returns_unavailable_not_panic() {
    let recorder = Recorder::default();
    let pool = Pool::with_factory(list_of(&[]), fake_factory(recorder, Default::default()));

    let err = pool
        .call("m", None, b"{}", RetryMode::Never)
        .await
        .unwrap_err();
    assert_eq!(err.status, opsapi::Status::Unavailable);
    assert!(pool.readyz().is_err(), "an empty pool is not ready");
}

/// A pool where ALL instances are down reports readyz down (the all-down arm), distinct
/// from the some-down Ready arm above.
#[tokio::test]
async fn pool_all_down_reports_not_ready() {
    let recorder = Recorder::default();
    let mut dead = std::collections::HashSet::new();
    dead.insert("A".to_string());
    dead.insert("B".to_string());
    let pool = Pool::with_factory(list_of(&["A", "B"]), fake_factory(recorder, dead));

    // A refresh populates the (all-dead) set; the call selects None on a POPULATED set
    // (the `select`→None branch, distinct from the n==0 empty-list branch) → Unavailable.
    let err = pool
        .call("m", None, b"{}", RetryMode::Never)
        .await
        .unwrap_err();
    assert_eq!(
        err.status,
        opsapi::Status::Unavailable,
        "a populated but all-down set must select None → Unavailable"
    );
    let msg = pool.readyz().expect_err("all-down pool must be unready");
    assert!(msg.contains("all"), "all-down verdict must name it: {msg}");
}

/// finding-1 (latest-resolve-wins): a SLOW older resolve must NOT clobber a fresher set.
/// The first resolve returns `[A,B]` but is held parked past a second resolve that returns
/// `[A]` and applies; when the slow resolve finally lands, its stale `[A,B]` is DROPPED by
/// the generation guard, so the final set is `[A]`. Without the guard it would be `[A,B]`.
#[tokio::test]
async fn pool_slow_resolve_does_not_clobber_newer_set() {
    let recorder = Recorder::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new()); // first resolve has started
    let release = Arc::new(tokio::sync::Notify::new()); // let the first resolve finish

    let c = calls.clone();
    let e = entered.clone();
    let r = release.clone();
    let list: PeerListResolver = Arc::new(move || {
        let n = c.fetch_add(1, Ordering::SeqCst);
        let e = e.clone();
        let r = r.clone();
        Box::pin(async move {
            if n == 0 {
                // The SLOW, OLDER resolve: signal it started (its generation is claimed),
                // then park until released — it returns the STALE list [A,B].
                e.notify_one();
                r.notified().await;
                Ok(vec!["A".to_string(), "B".to_string()])
            } else {
                // The FAST, NEWER resolve: returns [A] immediately.
                Ok(vec!["A".to_string()])
            }
        })
    });

    let pool = Arc::new(Pool::with_factory(
        list,
        fake_factory(recorder, Default::default()),
    ));

    // t1 claims generation 1, then parks inside its (slow) resolve.
    let p1 = pool.clone();
    let t1 = tokio::spawn(async move { p1.refresh_once().await });
    entered.notified().await; // gen 1 is claimed and parked

    // t2 (inline) claims generation 2, resolves [A] fast, and APPLIES it.
    pool.refresh_once().await;
    {
        let g = pool.instances.lock().unwrap();
        assert_eq!(
            g.iter().map(|i| i.addr.clone()).collect::<Vec<_>>(),
            vec!["A".to_string()],
            "the newer resolve applied [A]"
        );
    }

    // Release the slow older resolve; its stale [A,B] must be DROPPED, not clobber [A].
    release.notify_one();
    t1.await.unwrap();
    let g = pool.instances.lock().unwrap();
    assert_eq!(
        g.iter().map(|i| i.addr.clone()).collect::<Vec<_>>(),
        vec!["A".to_string()],
        "the slow older resolve must not clobber the newer set"
    );
}

/// finding-2 (detached teardown): tearing down removed instances must NOT sit on the
/// request path. A scale-down whose removed instances have a SLOW `close` still returns
/// the triggering `call` promptly — the teardown is detached.
#[tokio::test]
async fn pool_teardown_is_detached_off_the_request_path() {
    let recorder = Recorder::default();
    // The list answers [A,B,C] first (seed), then [] (scale to zero → 3 removals).
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let list: PeerListResolver = Arc::new(move || {
        let n = c.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if n == 0 {
                Ok(vec!["A".to_string(), "B".to_string(), "C".to_string()])
            } else {
                Ok(Vec::new())
            }
        })
    });
    // A factory whose instances have a `close` that blocks a LONG time — if teardown were
    // on the request path the triggering call would wait for all three.
    let factory: InstanceFactory = Arc::new(move |addr: &str| {
        let addr = addr.to_string();
        let calls = recorder.call_counter(&addr);
        let caller: Arc<dyn Caller> = Arc::new(RecordingCaller {
            addr: addr.clone(),
            calls,
        });
        let health = Arc::new(InstanceHealth::seed());
        *health.verdict.lock().unwrap() = Ok(());
        health
            .last_probe_at
            .store(coarse_now_secs().max(1), Ordering::SeqCst);
        let close: InstanceCloser = Arc::new(|| {
            Box::pin(async {
                // Far longer than the test's promptness bound; the detached task is
                // aborted when the test runtime shuts down, so it never hangs the test.
                tokio::time::sleep(Duration::from_secs(30)).await
            })
        });
        Instance {
            addr,
            caller,
            health,
            probe: None,
            close,
        }
    });

    let pool = Pool::with_factory(list, factory);

    // Seed [A,B,C] (does not set the throttle, so the next `call` will refresh).
    pool.refresh_once().await;

    // The triggering call: refresh resolves [] → removes A,B,C (slow closes) → detached.
    let started = std::time::Instant::now();
    let err = pool
        .call("m", None, b"{}", RetryMode::Never)
        .await
        .unwrap_err();
    let elapsed = started.elapsed();

    assert_eq!(err.status, opsapi::Status::Unavailable, "scaled to zero → Unavailable");
    assert!(
        elapsed < Duration::from_secs(2),
        "the triggering call must not serialize behind the 3 slow closes (took {elapsed:?})"
    );
}

/// The set reconciliation: `[A,B]` seeds two; `[A,B] -> [A]` drops B (and returns it for
/// teardown); `[A] -> [A,C]` adds C and KEEPS A (A is not rebuilt — its connection +
/// probe survive the refresh).
#[test]
fn pool_reconcile_adds_and_drops_and_keeps_instances() {
    let recorder = Recorder::default();
    let pool = Pool::with_factory(list_of(&[]), fake_factory(recorder.clone(), Default::default()));

    fn addrs(pool: &Pool) -> Vec<String> {
        pool.instances
            .lock()
            .unwrap()
            .iter()
            .map(|i| i.addr.clone())
            .collect()
    }

    let _ = pool.reconcile_for_test(vec!["A".into(), "B".into()]);
    assert_eq!(addrs(&pool), vec!["A".to_string(), "B".to_string()]);

    // [A,B] -> [A]: B is dropped and returned for teardown.
    let removed = pool.reconcile_for_test(vec!["A".into()]);
    assert_eq!(addrs(&pool), vec!["A".to_string()]);
    assert_eq!(
        removed.iter().map(|i| i.addr.clone()).collect::<Vec<_>>(),
        vec!["B".to_string()],
        "the vanished instance is returned so the caller tears its probe/conn down"
    );

    // [A] -> [A,C]: C is added, A is kept (not rebuilt).
    let _ = pool.reconcile_for_test(vec!["A".into(), "C".into()]);
    assert_eq!(addrs(&pool), vec!["A".to_string(), "C".to_string()]);
    assert_eq!(recorder.builds("A"), 1, "kept instance A must not be rebuilt on refresh");
    assert_eq!(recorder.builds("B"), 1);
    assert_eq!(recorder.builds("C"), 1);
}

/// The per-instance health predicates that drive selection + readyz: pending (never
/// probed) is selectable but not healthy (optimistic cold start, fail-closed readyz); a
/// fresh `Ok` is both; a fresh `Err` corpse is neither; a stale `Ok` (dead probe task)
/// flips to neither.
#[test]
fn instance_health_selectable_and_healthy_branches() {
    let now = 1000u64;

    let pending = InstanceHealth::seed();
    assert!(!pending.healthy(now), "never-probed is not healthy (fail-closed)");
    assert!(pending.is_selectable(now), "never-probed is selectable (optimistic cold start)");

    let ok = InstanceHealth::seed();
    *ok.verdict.lock().unwrap() = Ok(());
    ok.last_probe_at.store(now, Ordering::SeqCst);
    assert!(ok.healthy(now) && ok.is_selectable(now), "fresh Ok is healthy + selectable");

    let dead = InstanceHealth::seed();
    *dead.verdict.lock().unwrap() = Err("down".into());
    dead.last_probe_at.store(now, Ordering::SeqCst);
    assert!(!dead.healthy(now) && !dead.is_selectable(now), "a fresh corpse is neither");

    let stale = InstanceHealth::seed();
    *stale.verdict.lock().unwrap() = Ok(());
    stale.last_probe_at.store(now, Ordering::SeqCst);
    let later = now + PROBE_STALL_MAX.as_secs() + 1;
    assert!(!stale.healthy(later), "stale Ok flips to not-healthy");
    assert!(!stale.is_selectable(later), "a stale-Ok instance is skipped by selection");
}

// ---- C2: a POOLED stub wires a `Pool` capability caller + full-set PEER_SLOT ----
//
// A `PeerSource::pooled` stub is the managed multi-instance wiring: its capability caller
// is a `Pool` (whose round-robin distribution across `[A,B]` is proven by
// `pool_distributes_round_robin_across_two_instances` above — the property `exactly_one`
// structurally refused), and its boot snapshot carries ALL instances into `PEER_SLOT` so a
// co-hosted gateway route table pools across the SAME set. These pin the STUB wiring; the
// pool's own behaviour is the C1 block.

/// The full-set PEER_SLOT population (C2): a pooled stub contributes ALL its boot instances
/// to `PEER_SLOT`, not a collapsed first element. This is the set the gateway route table
/// reads to build its own per-provider `Pool`.
#[test]
fn pooled_stub_contributes_full_instance_set_to_peer_slot() {
    let ctx = Context::new();
    let stub = Stub::new(
        "fake",
        PeerSource::pooled(
            vec!["127.0.0.1:9000".to_string(), "127.0.0.1:9100".to_string()],
            list_of(&["127.0.0.1:9000", "127.0.0.1:9100"]),
        ),
        vec![Box::new(|_ctx, _caller| {})],
    );
    stub.init(&ctx).unwrap();
    let peers: Vec<opsapi::PeerAddr> = ctx.contributions(opsapi::PEER_SLOT);
    let found = peers.iter().find(|p| p.provider == "fake").expect("PEER_SLOT contribution");
    assert_eq!(
        found.addrs,
        vec!["127.0.0.1:9000".to_string(), "127.0.0.1:9100".to_string()],
        "a pooled stub carries the WHOLE instance set into PEER_SLOT (not the first only)"
    );
}

/// A pooled stub's `register` still applies every injected factory (topology-blind), handing
/// each the `Pool` as the `opsapi::Caller` — the consumer's `require` resolves to a pool
/// exactly as it would a single `Reconnecting` conn, unaware which it got.
#[test]
fn pooled_stub_runs_every_factory_over_the_pool_caller() {
    let ctx = Context::new();
    let hits = Arc::new(AtomicUsize::new(0));
    let captured: Arc<StdMutex<Option<Arc<dyn Caller>>>> = Arc::new(StdMutex::new(None));

    let h = hits.clone();
    let cap = captured.clone();
    let factories: Vec<RemoteFactory> = vec![Box::new(move |_ctx: &Context, caller| {
        h.fetch_add(1, Ordering::SeqCst);
        *cap.lock().unwrap() = Some(caller);
    })];

    let stub = Stub::new(
        "fake",
        PeerSource::pooled(vec!["127.0.0.1:9000".to_string()], list_of(&["127.0.0.1:9000"])),
        factories,
    );
    stub.register(&ctx).unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 1, "register must run the factory for a pooled stub");
    assert!(captured.lock().unwrap().is_some(), "the factory was handed the pool caller");
}

/// A pooled stub's `/readyz` is `Pool::readyz` (some-down-vs-all-down over the per-instance
/// verdicts), contributed under the SAME `stub:<provider>` name a single stub uses — and it
/// is fail-closed DOWN at cold start (no instance resolved/probed yet), mirroring the single
/// stub's `Err("probe pending")` seed. Zero I/O: it reads the empty instance set.
#[tokio::test]
async fn pooled_stub_readyz_is_pool_readyz_and_cold_start_down() {
    let ctx = Context::new();
    let stub = Stub::new(
        "fake",
        PeerSource::pooled(vec!["127.0.0.1:9000".to_string()], list_of(&["127.0.0.1:9000"])),
        vec![Box::new(|_ctx, _caller| {})],
    );
    stub.init(&ctx).unwrap();
    let check = ctx
        .contributions::<httpmw::ReadyCheck>(httpmw::READINESS_SLOT)
        .into_iter()
        .find(|c| c.name() == "stub:fake")
        .expect("a pooled stub must contribute a `stub:fake` readiness check");
    let err = check.run().await.expect_err("cold-start pool (no instances resolved) is down");
    assert!(err.contains("no resolved instances"), "cold-start down verdict: {err}");
}

// ---- C3: RetryMode-gated cross-instance failover ----------------------------
//
// The pool's `call` may fail over to a DIFFERENT instance ONLY when three orthogonal
// conditions all hold: (1) RetryMode::OnceAfterReconnect (WHETHER — a mutation NEVER
// re-sends), (2) the failure is a PROVEN connection-death class (`!is_definitive_answer`
// — a peer that ANSWERED ran the op), and (3) a different instance exists (WHERE — the
// cursor advances OFF the dead addr). These prove each condition's failing branch with a
// per-instance INVOCATION COUNTER, so a double-send is observable by construction.
//
// The failure representatives mirror the ONLY errors that reach the Caller boundary as
// `Err` (a domain error rides in-envelope as `Ok(bytes)`): `Error::unavailable` = every
// transport fault incl. a connection death (`!is_definitive_answer`), and
// `Error::not_found` = `UnknownMethod`, the sole "peer answered" definitive Err.

/// Per-instance outcome for the C3 fake transport. `ConnDeath` is the proven
/// connection-death class (maps to `Unavailable`, `!is_definitive_answer`); `AppAnswer` is
/// a definitive peer answer (`NotFound`, `is_definitive_answer` — the op RAN); `Succeed`
/// echoes the addr.
#[derive(Clone)]
enum C3Behavior {
    ConnDeath,
    AppAnswer,
    Succeed,
}

/// A fake per-instance caller that COUNTS its invocations (so a double-send is observable)
/// and returns its preset outcome. `RetryMode` is intentionally ignored here — the C3
/// decision lives in `Pool::call`, one level ABOVE this per-instance caller (which is a
/// `Reconnecting` in production; its own single-conn `RetryMode` handling is unchanged).
struct FailoverCaller {
    addr: String,
    calls: Arc<AtomicUsize>,
    behavior: C3Behavior,
}

#[async_trait]
impl Caller for FailoverCaller {
    async fn call(
        &self,
        _method: &str,
        _identity: Option<&str>,
        _payload: &[u8],
        _retry_mode: RetryMode,
    ) -> Result<Vec<u8>, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.behavior {
            C3Behavior::ConnDeath => Err(Error::unavailable(format!("{}: connection dead", self.addr))),
            C3Behavior::AppAnswer => Err(Error::not_found(format!("{}: no such method", self.addr))),
            C3Behavior::Succeed => Ok(self.addr.clone().into_bytes()),
        }
    }
}

/// A C3 fake factory: every instance is HEALTHY/selectable (the failure is discovered at
/// CALL time, exactly like a mid-request instance death — the probe verdict has NOT
/// flipped), so selection reaches the failing instance and the retry decision is driven by
/// `call`'s gate, not by `is_selectable`. Each instance's outcome comes from `behaviors`
/// (default `Succeed`); the shared `recorder` counts per-addr invocations.
fn failover_factory(
    recorder: Recorder,
    behaviors: std::collections::HashMap<String, C3Behavior>,
) -> InstanceFactory {
    Arc::new(move |addr: &str| {
        let addr = addr.to_string();
        recorder.note_build(&addr);
        let calls = recorder.call_counter(&addr);
        let behavior = behaviors.get(&addr).cloned().unwrap_or(C3Behavior::Succeed);
        let caller: Arc<dyn Caller> = Arc::new(FailoverCaller {
            addr: addr.clone(),
            calls,
            behavior,
        });
        let health = Arc::new(InstanceHealth::seed());
        *health.verdict.lock().unwrap() = Ok(());
        health
            .last_probe_at
            .store(coarse_now_secs().max(1), Ordering::SeqCst);
        let close: InstanceCloser = Arc::new(|| Box::pin(async {}));
        Instance {
            addr,
            caller,
            health,
            probe: None,
            close,
        }
    })
}

fn behaviors(pairs: &[(&str, C3Behavior)]) -> std::collections::HashMap<String, C3Behavior> {
    pairs.iter().map(|(a, b)| (a.to_string(), b.clone())).collect()
}

/// THE double-execute guard (the branch that would double-send if C3 cross-retried a
/// mutation): a `RetryMode::Never` op whose selected instance dies with the connection-death
/// class returns the error and NEVER re-sends to another instance — the exactly-once (at
/// most once) side-effect property. The cursor starts at 0, so the first (index-0)
/// instance `dead` is selected; the second `live` must receive ZERO calls.
#[tokio::test]
async fn mutation_never_cross_retries_on_connection_death() {
    let recorder = Recorder::default();
    let pool = Pool::with_factory(
        list_of(&["dead", "live"]),
        failover_factory(recorder.clone(), behaviors(&[("dead", C3Behavior::ConnDeath)])),
    );

    let err = pool
        .call("m", None, b"{}", RetryMode::Never)
        .await
        .expect_err("a mutation onto a dying instance returns the error");

    assert_eq!(err.status, opsapi::Status::Unavailable, "the connection-death error is returned verbatim");
    assert_eq!(recorder.hits("dead"), 1, "the selected instance was invoked exactly once");
    assert_eq!(
        recorder.hits("live"),
        0,
        "a MUTATION must NEVER be re-sent to another instance (double-execute hazard)"
    );
}

/// The failover the single-conn path structurally cannot do: a `RetryMode::OnceAfterReconnect`
/// (`#[retry_safe]`) op whose selected instance is a proven connection death transparently
/// retries on a DIFFERENT instance and succeeds. `dead` (index 0) is selected first and dies;
/// the retry advances the cursor OFF `dead` to `live`, which serves the request.
#[tokio::test]
async fn retry_safe_read_fails_over_to_a_different_instance() {
    let recorder = Recorder::default();
    let pool = Pool::with_factory(
        list_of(&["dead", "live"]),
        failover_factory(recorder.clone(), behaviors(&[("dead", C3Behavior::ConnDeath)])),
    );

    let out = pool
        .call("m", None, b"{}", RetryMode::OnceAfterReconnect)
        .await
        .expect("a retry-safe read fails over to a healthy instance");

    assert_eq!(out, b"live", "the retry landed on the DIFFERENT (cursor-advanced) instance");
    assert_eq!(recorder.hits("dead"), 1, "the dead instance was tried once");
    assert_eq!(recorder.hits("live"), 1, "the failover reached a different instance");
}

/// An application (definitive-answer) error is NOT cross-retried even for a retry-safe op:
/// re-running an op the peer already RAN elsewhere is the double-execute hazard. `ans`
/// (index 0) answers `NotFound` (`is_definitive_answer` — a real `UnknownMethod`); the pool
/// returns it verbatim and NEVER touches `live`.
#[tokio::test]
async fn application_error_is_not_cross_retried_even_retry_safe() {
    let recorder = Recorder::default();
    let pool = Pool::with_factory(
        list_of(&["ans", "live"]),
        failover_factory(recorder.clone(), behaviors(&[("ans", C3Behavior::AppAnswer)])),
    );

    let err = pool
        .call("m", None, b"{}", RetryMode::OnceAfterReconnect)
        .await
        .expect_err("a definitive peer answer is an error the op RAN into, returned verbatim");

    assert_eq!(err.status, opsapi::Status::NotFound, "the application answer is returned verbatim");
    assert_eq!(recorder.hits("ans"), 1, "the answering instance was invoked once");
    assert_eq!(
        recorder.hits("live"),
        0,
        "an op the peer ANSWERED must NOT be re-run on another instance"
    );
}

/// The cross-instance retry is BOUNDED to exactly one attempt: with `[d1 dead, d2 dead]` and
/// a retry-safe op, the pool tries `d1` then fails over ONCE to `d2` (never a third / never a
/// loop), and returns `d2`'s error. Each dead instance is invoked exactly once.
#[tokio::test]
async fn retry_safe_failover_is_bounded_to_one_cross_instance_attempt() {
    let recorder = Recorder::default();
    let pool = Pool::with_factory(
        list_of(&["d1", "d2"]),
        failover_factory(
            recorder.clone(),
            behaviors(&[("d1", C3Behavior::ConnDeath), ("d2", C3Behavior::ConnDeath)]),
        ),
    );

    let err = pool
        .call("m", None, b"{}", RetryMode::OnceAfterReconnect)
        .await
        .expect_err("both instances dead → the final failover error is returned");

    assert_eq!(err.status, opsapi::Status::Unavailable);
    assert_eq!(recorder.hits("d1"), 1, "the first instance is tried exactly once");
    assert_eq!(recorder.hits("d2"), 1, "the failover instance is tried exactly once (no loop)");
    assert_eq!(
        recorder.hits("d1") + recorder.hits("d2"),
        2,
        "bounded: at most one cross-instance retry, no third attempt"
    );
}

// ---- The COMPOSED retry budget: a real `Reconnecting` UNDER a `Pool` ---------
//
// Every C3 test above injects a per-instance `FailoverCaller` that IGNORES `retry_mode`,
// so the two-layer budget — `Reconnecting`'s per-connection redial+replay INSIDE the
// pool's cross-instance failover — was asserted by nothing. These two tests build the
// production composition (a real `Reconnecting` per instance, over a scripted fake
// transport, under a real `Pool`) and pin the EXACT wire-execution sequence, per
// instance.
//
// This documents the CURRENT behaviour as safe-by-composition; it is NOT a cap:
// * `OnceAfterReconnect` (idempotent `#[retry_safe]` read) composes to at most FOUR wire
//   executions — A initial + A replay, then B initial + B replay. Instance B keeps its
//   OWN reconnect self-heal on the failover path deliberately; capping B to `Never` would
//   regress a real recovery path, and N executions of an idempotent read are safe.
// * `Never` (a mutation) composes to exactly ONE wire execution: the `retry_mode` gate in
//   `Pool::call` is checked FIRST, so failover is unreachable, and `Reconnecting` does not
//   replay either.
//
// Timing: the fixture contains NO timer at all — instances are built with `probe: None`
// (no probe loop), the list resolver is ready immediately, and every dial completes
// synchronously — so nothing races a clock and no paused-clock bookkeeping is needed.

/// One scripted wire execution: a proven connection-fatal failure (the class
/// `Reconnecting` resets+replays on and the pool fails over on), or a success echoing the
/// instance addr.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WireOutcome {
    Fatal,
    Ok,
}

/// The shared, ORDERED execution ledger. Every wire execution appends `"<addr>#<n>"`
/// (`n` = that instance's own 1-based execution count), so a test asserts the exact
/// interleaving WITH per-instance attribution — never a global count that could not tell
/// "A twice then B twice" from "A four times".
#[derive(Clone, Default)]
struct WireLog {
    entries: Arc<StdMutex<Vec<String>>>,
}

impl WireLog {
    fn record(&self, addr: &str, n: usize) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("{addr}#{n}"));
    }
    fn seq(&self) -> Vec<String> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// One instance's scripted transport, SHARED by every connection its dialer hands out —
/// so a `Reconnecting` replay on a FRESH connection still advances the SAME instance's
/// script. Running past the script is a PANIC, not a silent success: an extra retry layer
/// (or a widened budget) fails loudly at the exact instance that over-executed.
struct InstanceScript {
    addr: String,
    log: WireLog,
    budget: usize,
    remaining: StdMutex<std::collections::VecDeque<WireOutcome>>,
    execs: AtomicUsize,
    dials: AtomicUsize,
    closes: AtomicUsize,
}

impl InstanceScript {
    fn new(addr: &str, log: WireLog, plan: Vec<WireOutcome>) -> InstanceScript {
        InstanceScript {
            addr: addr.to_string(),
            log,
            budget: plan.len(),
            remaining: StdMutex::new(plan.into_iter().collect()),
            execs: AtomicUsize::new(0),
            dials: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
        }
    }

    /// Consumes the next scripted outcome, recording the execution first (so an
    /// over-execution is visible in the ledger of the panic message too).
    fn next_outcome(&self) -> Result<Vec<u8>, CallFailure> {
        let n = self.execs.fetch_add(1, Ordering::SeqCst) + 1;
        self.log.record(&self.addr, n);
        let next = self
            .remaining
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front();
        match next {
            Some(WireOutcome::Ok) => Ok(self.addr.clone().into_bytes()),
            // The connection-death class: `ConnectionFatal` provenance (what `Reconnecting`
            // resets + replays on) mapping to `Unavailable` (what the pool fails over on).
            Some(WireOutcome::Fatal) => Err(CallFailure {
                mapped: Error::unavailable(format!("{}: connection dead", self.addr)),
                provenance: FailureProvenance::ConnectionFatal,
            }),
            None => panic!(
                "composed retry budget grew: instance {} executed wire call #{n}, its script \
                 allowed only {} (ledger: {:?})",
                self.addr,
                self.budget,
                self.log.seq()
            ),
        }
    }

    fn execs(&self) -> usize {
        self.execs.load(Ordering::SeqCst)
    }
    fn dials(&self) -> usize {
        self.dials.load(Ordering::SeqCst)
    }
    fn closes(&self) -> usize {
        self.closes.load(Ordering::SeqCst)
    }
}

/// One connection handed out by [`ScriptedDialer`] — every connection of an instance
/// shares that instance's script, so redials do not rewind it.
struct ScriptedConn {
    script: Arc<InstanceScript>,
}

#[async_trait]
impl Conn for ScriptedConn {
    async fn call(
        &self,
        _method: &str,
        _identity: Option<&str>,
        _payload: &[u8],
    ) -> Result<Vec<u8>, CallFailure> {
        self.script.next_outcome()
    }
    fn close(&self) {
        self.script.closes.fetch_add(1, Ordering::SeqCst);
    }
}

/// The instance's dialer: always dials successfully (a dial failure is a different
/// branch, already covered), counting dials so a REDIAL is observed directly rather than
/// inferred from a call count.
struct ScriptedDialer {
    script: Arc<InstanceScript>,
}

#[async_trait]
impl Dialer for ScriptedDialer {
    async fn dial(&self) -> Result<Arc<dyn Conn>, Error> {
        self.script.dials.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(ScriptedConn {
            script: self.script.clone(),
        }))
    }
}

/// Holds the per-addr scripts and hands the pool an [`InstanceFactory`] that builds a REAL
/// `Reconnecting` per instance (the production composition — `edge_instance_factory` with
/// the QUIC dialer swapped for the scripted one, and `probe: None` so no timer exists).
/// Keeps each built script so a test can read that instance's exec/dial/close counters.
#[derive(Clone)]
struct ScriptBook {
    log: WireLog,
    plans: Arc<StdMutex<std::collections::HashMap<String, Vec<WireOutcome>>>>,
    built: Arc<StdMutex<std::collections::HashMap<String, Arc<InstanceScript>>>>,
}

impl ScriptBook {
    fn new(plans: &[(&str, &[WireOutcome])]) -> ScriptBook {
        ScriptBook {
            log: WireLog::default(),
            plans: Arc::new(StdMutex::new(
                plans
                    .iter()
                    .map(|(a, p)| (a.to_string(), p.to_vec()))
                    .collect(),
            )),
            built: Arc::new(StdMutex::new(std::collections::HashMap::new())),
        }
    }

    fn seq(&self) -> Vec<String> {
        self.log.seq()
    }

    /// The built script for `addr` — an instance never built by the pool is itself a test
    /// failure (the pool must hold both instances for the failover question to be real).
    fn instance(&self, addr: &str) -> Arc<InstanceScript> {
        self.built
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(addr)
            .cloned()
            .unwrap_or_else(|| panic!("the pool never built instance {addr}"))
    }

    fn factory(&self) -> InstanceFactory {
        let book = self.clone();
        Arc::new(move |addr: &str| {
            let plan = book
                .plans
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(addr)
                .cloned()
                .unwrap_or_default();
            let script = Arc::new(InstanceScript::new(addr, book.log.clone(), plan));
            book.built
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(addr.to_string(), script.clone());
            // The REAL production caller: a `Reconnecting` owning its own connection cache
            // and its own single redial+replay policy.
            let recon = Arc::new(Reconnecting::new(ScriptedDialer {
                script: script.clone(),
            }));
            let caller: Arc<dyn Caller> = recon.clone();
            let health = Arc::new(InstanceHealth::seed());
            // Healthy + freshly stamped: the failure is discovered at CALL time (a
            // mid-request death), so selection reaches the instance and the retry decision
            // is `call`'s gate, not `is_selectable`.
            *health.verdict.lock().unwrap_or_else(|e| e.into_inner()) = Ok(());
            health
                .last_probe_at
                .store(coarse_now_secs().max(1), Ordering::SeqCst);
            let close: InstanceCloser = Arc::new(move || {
                let recon = recon.clone();
                Box::pin(async move { recon.close().await })
            });
            Instance {
                addr: addr.to_string(),
                caller,
                health,
                probe: None,
                close,
            }
        })
    }
}

/// THE composition test: a real `Reconnecting` per instance UNDER a real `Pool`, one
/// `RetryMode::OnceAfterReconnect` (`#[retry_safe]`) op. Instance A dies fatally on its
/// initial call AND on its own single replay; the pool then fails over to B, which dies
/// fatally on first touch and heals on ITS OWN single redial+replay.
///
/// The pinned sequence is exactly `A#1, A#2, B#1, B#2` — the documented composed worst
/// case of 4 wire executions for an idempotent op. It goes RED in both directions:
/// * capping the failover call to `RetryMode::Never` (the DROPPED "fix" to the non-bug)
///   removes `B#2` and turns the success into an error — B would lose its reconnect
///   self-heal;
/// * any additional retry layer over-runs a script and panics at the offending instance.
#[tokio::test]
async fn composed_retry_safe_op_runs_a_initial_plus_replay_then_b_initial_plus_replay() {
    let book = ScriptBook::new(&[
        // A: fatal on the initial call, fatal again on `Reconnecting`'s one replay.
        ("A", &[WireOutcome::Fatal, WireOutcome::Fatal]),
        // B: fatal on first touch, healed on `Reconnecting`'s one replay.
        ("B", &[WireOutcome::Fatal, WireOutcome::Ok]),
    ]);
    let pool = Pool::with_factory(list_of(&["A", "B"]), book.factory());

    let out = pool
        .call("characters.ownerOf", None, b"{}", RetryMode::OnceAfterReconnect)
        .await
        .expect("B's OWN redial+replay must still heal the call on the failover path");

    assert_eq!(out, b"B", "the answer came from the failover instance");
    assert_eq!(
        book.seq(),
        vec!["A#1", "A#2", "B#1", "B#2"],
        "exact composed sequence: A initial + A replay, then B initial + B replay"
    );
    assert_eq!(book.seq().len(), 4, "the documented composed worst case is 4 wire executions");

    let a = book.instance("A");
    assert_eq!(a.execs(), 2, "A: initial + its own single replay, never a third");
    assert_eq!(a.dials(), 2, "A redialed exactly once (the replay used a FRESH conn)");
    assert_eq!(a.closes(), 2, "both of A's fatally-failed conns were reset");

    let b = book.instance("B");
    assert_eq!(b.execs(), 2, "B: the pool's initial failover call + B's OWN replay");
    assert_eq!(
        b.dials(),
        2,
        "B redialed itself — the failover call retains `Reconnecting`'s self-heal (a cap \
         to RetryMode::Never here would make this 1 and fail the call)"
    );
    assert_eq!(b.closes(), 1, "only B's first (dead) conn was reset; the healed one stays cached");
}

/// The WHETHER-gate (`Pool::call`, `retry_mode != OnceAfterReconnect` checked FIRST): a
/// mutation whose instance dies fatally totals EXACTLY ONE wire execution — neither
/// `Reconnecting`'s replay nor the pool's failover fires, so the side effect ran at most
/// once.
///
/// Failover-unreachability is proven BY CONSTRUCTION, not by absence of errors: instance
/// B is built with an EMPTY script, so any wire execution on B panics; B's dial counter is
/// asserted at 0, so B's transport was never even opened.
#[tokio::test]
async fn composed_mutation_executes_exactly_once_and_never_reaches_failover() {
    let book = ScriptBook::new(&[
        ("A", &[WireOutcome::Fatal]),
        // B has NO budget at all: one touch is a loud panic, not a quiet extra call.
        ("B", &[]),
    ]);
    let pool = Pool::with_factory(list_of(&["A", "B"]), book.factory());

    let err = pool
        .call("characters.create", None, b"{}", RetryMode::Never)
        .await
        .expect_err("a mutation onto a dying instance returns the error verbatim");

    assert_eq!(err.status, opsapi::Status::Unavailable);
    assert_eq!(book.seq(), vec!["A#1"], "exactly one wire execution, on the selected instance");

    let a = book.instance("A");
    assert_eq!(a.execs(), 1, "a mutation is never replayed on its own connection");
    assert_eq!(a.dials(), 1, "no redial for the aborted call (the NEXT request redials)");
    assert_eq!(a.closes(), 1, "the dead conn is still reset — reset precedes the RetryMode gate");

    let b = book.instance("B");
    assert_eq!(b.execs(), 0, "the failover instance ran NOTHING for a mutation");
    assert_eq!(
        b.dials(),
        0,
        "the failover instance's transport was never even dialed — the WHETHER-gate \
         returns before `Pool::call` selects a second instance"
    );
}

// --- describe() client helper (routing-as-data, D1) -------------------------

/// A fake `Caller` that records the method/identity/payload/retry it was called with
/// and answers with a fixed body — enough to prove `remote::describe` calls the
/// reserved op correctly and deserializes the manifest.
struct DescribeCaller {
    body: Vec<u8>,
    seen: std::sync::Mutex<Option<(String, bool, usize, RetryMode)>>,
}

#[async_trait]
impl Caller for DescribeCaller {
    async fn call(
        &self,
        method: &str,
        identity: Option<&str>,
        payload: &[u8],
        retry_mode: RetryMode,
    ) -> Result<Vec<u8>, Error> {
        *self.seen.lock().unwrap() =
            Some((method.to_string(), identity.is_some(), payload.len(), retry_mode));
        Ok(self.body.clone())
    }
}

fn sample_manifest() -> opsapi::DescribeManifest {
    opsapi::DescribeManifest {
        ops: vec![opsapi::OpManifest {
            method: "match.report".into(),
            verb: "POST".into(),
            path: "/match/report".into(),
            auth: opsapi::AuthReq::None,
            success: 202,
            retry_mode: opsapi::RetryMode::OnceAfterReconnect,
            args: vec![opsapi::ArgMapping {
                param: "report_id".into(),
                wire_key: "ReportId".into(),
                source: opsapi::ArgSource::Body,
            }],
        }],
    }
}

#[tokio::test]
async fn describe_calls_reserved_op_no_identity_no_body_retry_safe() {
    let manifest = sample_manifest();
    let caller = DescribeCaller {
        body: serde_json::to_vec(&manifest).unwrap(),
        seen: std::sync::Mutex::new(None),
    };
    let got = crate::describe(&caller).await.unwrap();
    assert_eq!(got, manifest);

    let (method, had_identity, payload_len, retry) = caller.seen.lock().unwrap().clone().unwrap();
    assert_eq!(method, opsapi::DESCRIBE_METHOD);
    assert!(!had_identity, "describe is unauthenticated");
    assert_eq!(payload_len, 0, "describe takes no arguments");
    // Read-only/idempotent: allowed one replay after reconnect (like a `#[retry_safe]` read).
    assert_eq!(retry, RetryMode::OnceAfterReconnect);
}

#[tokio::test]
async fn describe_surfaces_malformed_manifest_as_internal_error() {
    let caller = DescribeCaller {
        body: b"not json".to_vec(),
        seen: std::sync::Mutex::new(None),
    };
    let err = crate::describe(&caller).await.unwrap_err();
    assert_eq!(err.status, opsapi::Status::Internal);
}

/// LIVE serve→fetch round-trip (routing-as-data SERVE side, D1.5b): a real edge server
/// registers the reserved `__describe` op (exactly as `app::run` does on an edge-serving
/// process) carrying a two-MODULE aggregate manifest, and `remote::describe` over a real
/// `edge::Client` dials it and gets EVERY contributed op back. This is the end-to-end
/// path D1's `DescribeCaller` could only fake in-process — proving a managed gateway (D2)
/// dialing a peer's `__describe` really does receive that peer's whole `#[http]` surface.
#[tokio::test]
async fn describe_round_trips_over_a_live_edge_returning_every_op() {
    let ca = edge::DevCA::generate().unwrap();

    // The aggregate `app::run` would serve for a process co-hosting two `#[http]`
    // modules — one op from each, in concat order.
    let mut manifest = sample_manifest(); // match.report
    manifest.ops.push(opsapi::OpManifest {
        method: "characters.create".into(),
        verb: "POST".into(),
        path: "/characters".into(),
        auth: opsapi::AuthReq::Player,
        success: 201,
        retry_mode: opsapi::RetryMode::Never,
        args: Vec::new(),
    });

    let mut server = edge::Server::new();
    server.register_describe(manifest.clone());
    let running = server.listen("127.0.0.1:0".parse().unwrap(), &ca).unwrap();

    let client = edge::Client::dial(running.local_addr(), &ca).await.unwrap();
    let got = crate::describe(&client).await.unwrap();

    // The fetched manifest is byte-for-byte the served one, and BOTH modules' ops are
    // present — not just "a response arrived".
    assert_eq!(got, manifest);
    let methods: Vec<String> = got.ops.iter().map(|o| o.method.clone()).collect();
    assert!(methods.contains(&"match.report".to_string()), "{methods:?}");
    assert!(methods.contains(&"characters.create".to_string()), "{methods:?}");
}
