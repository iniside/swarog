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

// ---- The swap: register provides the right keys with the right types ---

/// The stub for `"characters"` provides BOTH capability keys the local impl would,
/// downcastable to the exact trait objects a consumer / the gateway `require`s —
/// proving the registry swap holds without any QUIC dial (register is lazy).
#[test]
fn stub_provides_characters_capability_keys() {
    let ctx = Context::new(); // DB-less: register only touches the registry
    let stub = Stub::new("characters", "127.0.0.1:9000");
    assert_eq!(stub.name(), "characters", "name is the PROVIDER name for validate_requires");
    assert!(stub.requires().is_empty());
    stub.register(&ctx).unwrap();

    assert!(
        ctx.registry()
            .try_require::<dyn charactersapi::Ownership>(&registry::key("characters", "ownership"))
            .is_some(),
        "characters.ownership must resolve to an Arc<dyn Ownership> (inventory's authz dep)"
    );
    assert!(
        ctx.registry()
            .try_require::<dyn charactersapi::Player>(&registry::key("characters", "player"))
            .is_some(),
        "characters.player must resolve to an Arc<dyn Player> (front-door dep)"
    );
}

// ---- Front-door route bindings: contributed to SLOT/BINDING_SLOT, not LOCAL_SLOT

/// The `"characters"` stub contributes the player ops' `Operation`+`OpBinding` to
/// the two route-table slots (matching the generated `route_bindings()`), and
/// NOTHING to `LOCAL_SLOT` — so the gateway resolves them Remote, over the edge.
#[test]
fn stub_contributes_characters_route_bindings_but_no_local() {
    let ctx = Context::new();
    Stub::new("characters", "127.0.0.1:9000").register(&ctx).unwrap();

    let expected: Vec<String> = charactersapi::player_rpc::route_bindings()
        .into_iter()
        .map(|rb| rb.operation.method)
        .collect();
    assert!(!expected.is_empty(), "player has #[http] ops to contribute");

    let ops: Vec<opsapi::Operation> = ctx.contributions(opsapi::SLOT);
    let bindings: Vec<opsapi::OpBinding> = ctx.contributions(opsapi::BINDING_SLOT);
    let locals: Vec<opsapi::LocalOp> = ctx.contributions(opsapi::LOCAL_SLOT);

    assert_eq!(
        ops.iter().map(|o| o.method.clone()).collect::<Vec<_>>(),
        expected,
        "SLOT carries exactly the player route Operations"
    );
    assert_eq!(
        bindings.iter().map(|b| b.method.clone()).collect::<Vec<_>>(),
        expected,
        "BINDING_SLOT carries the matching OpBindings"
    );
    assert!(locals.is_empty(), "no LocalOp — the stub has no in-process invoker");
}

/// The `"inventory"` stub contributes route bindings ONLY: SLOT/BINDING_SLOT carry
/// the holdings ops, LOCAL_SLOT is empty, and — because inventory is a leaf — the
/// registry has NO inventory capability provide (nothing requires one).
#[test]
fn stub_inventory_is_routes_only_no_capability() {
    let ctx = Context::new();
    Stub::new("inventory", "127.0.0.1:9001").register(&ctx).unwrap();

    let expected: Vec<String> = inventoryapi::holdings_rpc::route_bindings()
        .into_iter()
        .map(|rb| rb.operation.method)
        .collect();
    assert!(!expected.is_empty(), "holdings has #[http] ops to contribute");

    let ops: Vec<opsapi::Operation> = ctx.contributions(opsapi::SLOT);
    let bindings: Vec<opsapi::OpBinding> = ctx.contributions(opsapi::BINDING_SLOT);
    let locals: Vec<opsapi::LocalOp> = ctx.contributions(opsapi::LOCAL_SLOT);

    assert_eq!(
        ops.iter().map(|o| o.method.clone()).collect::<Vec<_>>(),
        expected,
        "SLOT carries exactly the holdings route Operations"
    );
    assert_eq!(bindings.len(), expected.len(), "BINDING_SLOT matches SLOT");
    assert!(locals.is_empty(), "no LocalOp for a routes-only stub");
}

/// An unknown provider name is a wiring bug: `register` fails loudly rather than
/// providing a dead client.
#[test]
fn stub_rejects_unknown_provider() {
    let ctx = Context::new();
    let stub = Stub::new("accounts", "127.0.0.1:9000");
    let err = stub.register(&ctx).unwrap_err();
    assert!(err.to_string().contains("no edge client"), "{err}");
}
