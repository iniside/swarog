use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

// ---- Fake transport for the redial-once logic --------------------------

/// A fake connection: `ok` decides whether its single `call` succeeds; a failing
/// call errors with `status` (historically only `Unavailable`, which is exactly why
/// the definitive-answer classification went untested); a shared counter records
/// closes so a test can assert `reset` closed the dead conn.
struct FakeConn {
    ok: bool,
    status: opsapi::Status,
    closes: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Conn for FakeConn {
    async fn call(&self, _method: &str, _identity: Option<&str>, _payload: &[u8]) -> Result<Vec<u8>, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.ok {
            Ok(b"ok".to_vec())
        } else {
            Err(Error::new(self.status, "fake: call failed"))
        }
    }
    fn close(&self) {
        self.closes.fetch_add(1, Ordering::SeqCst);
    }
}

/// A fake dialer: the Nth dial (0-based) yields a conn whose `call` succeeds iff
/// `N + 1 >= heal_after`; a failing conn errors with `fail_status`. `dials` counts
/// how many times it was asked to dial.
struct FakeDialer {
    dials: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    heal_after: usize,
    fail_status: opsapi::Status,
}

#[async_trait]
impl Dialer for FakeDialer {
    async fn dial(&self) -> Result<Arc<dyn Conn>, Error> {
        let n = self.dials.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(FakeConn {
            ok: n + 1 >= self.heal_after,
            status: self.fail_status,
            closes: self.closes.clone(),
            calls: self.calls.clone(),
        }))
    }
}

fn reconnecting(
    heal_after: usize,
) -> (Reconnecting<FakeDialer>, Arc<AtomicUsize>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    reconnecting_failing_with(heal_after, opsapi::Status::Unavailable)
}

/// Like [`reconnecting`], but failing conns error with `fail_status` — so tests can
/// exercise the definitive-answer (`NotFound`) vs transport-default (`Internal`)
/// classification, not just `Unavailable`.
fn reconnecting_failing_with(
    heal_after: usize,
    fail_status: opsapi::Status,
) -> (Reconnecting<FakeDialer>, Arc<AtomicUsize>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let dials = Arc::new(AtomicUsize::new(0));
    let closes = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let r = Reconnecting::new(FakeDialer {
        dials: dials.clone(),
        closes: closes.clone(),
        calls: calls.clone(),
        heal_after,
        fail_status,
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

/// A DEFINITIVE peer answer (`NotFound`, the typed unknown-method mapping) proves the
/// connection is healthy: no reset, no redial, the original error is returned — for
/// BOTH retry modes (the classification precedes the retry_mode branch).
#[tokio::test]
async fn notfound_does_not_reset_or_redial() {
    for mode in [RetryMode::Never, RetryMode::OnceAfterReconnect] {
        let (r, dials, closes, calls) =
            reconnecting_failing_with(usize::MAX, opsapi::Status::NotFound);
        let err = r
            .call("characters.ownerOf", None, b"{}", mode)
            .await
            .unwrap_err();
        assert_eq!(err.status, opsapi::Status::NotFound, "original answer returned ({mode:?})");
        assert_eq!(dials.load(Ordering::SeqCst), 1, "no redial on a definitive answer ({mode:?})");
        assert_eq!(closes.load(Ordering::SeqCst), 0, "healthy conn must NOT be reset ({mode:?})");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no replay ({mode:?})");
    }
}

/// A non-Unavailable, non-NotFound status (`Internal`) still takes the reset path:
/// reset is the DEFAULT — only a proven definitive answer skips it. Pins the
/// classification direction against a future `== Unavailable` rewrite (M4): a new
/// transport-fault status must fall into reset, not into keep-cached.
#[tokio::test]
async fn internal_status_still_resets() {
    let (r, dials, closes, _) =
        reconnecting_failing_with(usize::MAX, opsapi::Status::Internal);
    let err = r
        .call("characters.create", None, b"{}", RetryMode::Never)
        .await
        .unwrap_err();
    assert_eq!(err.status, opsapi::Status::Internal);
    assert_eq!(dials.load(Ordering::SeqCst), 1);
    assert_eq!(
        closes.load(Ordering::SeqCst),
        1,
        "an unclassified status MUST reset — reset is the default, not `== Unavailable`"
    );
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

/// A fake dialer whose dial #0 conn fails with `first_status` and every later
/// dial's conn fails with `second_status` — for exercising a DIFFERENT status on
/// the replay than on the first attempt (a definitive second answer must keep the
/// fresh connection, even though the first attempt was a transport failure).
struct TwoStatusDialer {
    dials: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    first_status: opsapi::Status,
    second_status: opsapi::Status,
}

#[async_trait]
impl Dialer for TwoStatusDialer {
    async fn dial(&self) -> Result<Arc<dyn Conn>, Error> {
        let n = self.dials.fetch_add(1, Ordering::SeqCst);
        let status = if n == 0 { self.first_status } else { self.second_status };
        Ok(Arc::new(FakeConn {
            ok: false,
            status,
            closes: self.closes.clone(),
            calls: self.calls.clone(),
        }))
    }
}

/// The first attempt fails `Unavailable` (a transport fault — reset, redial); the
/// replay on the fresh connection fails `NotFound` (a DEFINITIVE answer). The
/// mirrored guard on the second-attempt arm must keep c2 cached: only c1 (the
/// genuinely dead connection) is reset, and the final error surfaces as NotFound.
#[tokio::test]
async fn definitive_second_attempt_keeps_healthy_connection() {
    let dials = Arc::new(AtomicUsize::new(0));
    let closes = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let r = Reconnecting::new(TwoStatusDialer {
        dials: dials.clone(),
        closes: closes.clone(),
        calls: calls.clone(),
        first_status: opsapi::Status::Unavailable,
        second_status: opsapi::Status::NotFound,
    });
    let err = r
        .call("characters.ownerOf", None, b"{}", RetryMode::OnceAfterReconnect)
        .await
        .unwrap_err();
    assert_eq!(err.status, opsapi::Status::NotFound, "the definitive replay answer surfaces");
    assert_eq!(dials.load(Ordering::SeqCst), 2, "one initial dial + one retry, no more");
    assert_eq!(
        closes.load(Ordering::SeqCst),
        1,
        "only c1 (transport failure) was reset; c2 (definitive answer) stays cached"
    );
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
