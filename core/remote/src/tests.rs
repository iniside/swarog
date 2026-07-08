use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

// ---- Fake transport for the redial-once logic --------------------------

/// A fake connection: `ok` decides whether its single `call` succeeds; a shared
/// counter records closes so a test can assert `reset` closed the dead conn.
struct FakeConn {
    ok: bool,
    closes: Arc<AtomicUsize>,
}

#[async_trait]
impl Conn for FakeConn {
    async fn call(&self, _method: &str, _identity: Option<&str>, _payload: &[u8]) -> Result<Vec<u8>, Error> {
        if self.ok {
            Ok(b"ok".to_vec())
        } else {
            Err(Error::unavailable("fake: dead conn"))
        }
    }
    fn close(&self) {
        self.closes.fetch_add(1, Ordering::SeqCst);
    }
}

/// A fake dialer: the Nth dial (0-based) yields a conn whose `call` succeeds iff
/// `N + 1 >= heal_after`. `dials` counts how many times it was asked to dial.
struct FakeDialer {
    dials: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
    heal_after: usize,
}

#[async_trait]
impl Dialer for FakeDialer {
    async fn dial(&self) -> Result<Arc<dyn Conn>, Error> {
        let n = self.dials.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(FakeConn {
            ok: n + 1 >= self.heal_after,
            closes: self.closes.clone(),
        }))
    }
}

fn reconnecting(heal_after: usize) -> (Reconnecting<FakeDialer>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let dials = Arc::new(AtomicUsize::new(0));
    let closes = Arc::new(AtomicUsize::new(0));
    let r = Reconnecting::new(FakeDialer {
        dials: dials.clone(),
        closes: closes.clone(),
        heal_after,
    });
    (r, dials, closes)
}

/// A healthy first connection: one dial, one call, no redial.
#[tokio::test]
async fn healthy_call_does_not_redial() {
    let (r, dials, closes) = reconnecting(1); // dial #0 → ok
    let out = r.call("characters.ownerOf", None, b"{}").await.unwrap();
    assert_eq!(out, b"ok");
    assert_eq!(dials.load(Ordering::SeqCst), 1, "must not redial a healthy conn");
    assert_eq!(closes.load(Ordering::SeqCst), 0);
}

/// A dead first connection heals on the SINGLE retry: the first call fails, the
/// conn is reset (closed) and re-dialed, and the retry succeeds — exactly two dials.
#[tokio::test]
async fn redials_once_and_succeeds() {
    let (r, dials, closes) = reconnecting(2); // dial #0 → dead, dial #1 → ok
    let out = r.call("characters.ownerOf", None, b"{}").await.unwrap();
    assert_eq!(out, b"ok");
    assert_eq!(dials.load(Ordering::SeqCst), 2, "exactly one reconnect");
    assert_eq!(closes.load(Ordering::SeqCst), 1, "the dead conn was closed on reset");
}

/// A persistently dead peer: the first call fails, one reconnect is attempted, the
/// retry also fails — the error propagates and there is NO third dial.
#[tokio::test]
async fn gives_up_after_one_retry() {
    let (r, dials, closes) = reconnecting(usize::MAX); // every conn dead
    let err = r.call("characters.ownerOf", None, b"{}").await.unwrap_err();
    assert_eq!(err.status, opsapi::Status::Unavailable);
    assert_eq!(dials.load(Ordering::SeqCst), 2, "one initial dial + one retry, no more");
    assert_eq!(closes.load(Ordering::SeqCst), 1, "the first dead conn was closed");
}

/// `close` drops and closes the cached connection.
#[tokio::test]
async fn close_closes_cached_conn() {
    let (r, _dials, closes) = reconnecting(1);
    r.call("characters.ownerOf", None, b"{}").await.unwrap(); // caches a conn
    r.close().await;
    assert_eq!(closes.load(Ordering::SeqCst), 1);
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
