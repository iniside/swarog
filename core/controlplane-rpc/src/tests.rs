//! Loopback proofs for the generated `cp.*` glue: a real edge server carrying
//! [`register_face`] and the face [`remote_factory`] contributes through a real
//! `remote::Stub`, so every call crosses QUIC and `remote`'s reconnecting caller.

use std::collections::BTreeMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use lifecycle::Module;

use super::*;

const MODULE: &str = "fixture";
const PEER: &str = "fixture-peer";
const WAIT: Duration = Duration::from_secs(10);

/// What the provider received, so a test can assert the request crossed the wire intact.
#[derive(Clone, Debug, PartialEq)]
enum Seen {
    Panel(PanelQuery),
    Search(SearchQuery),
    Invoke(InvokeCall),
    Prepare(PrepareCall),
}

/// A provider whose answers carry every optional field, and which can park its first
/// `invoke` or `prepare` so a test can kill the connection while the call is in flight.
#[derive(Default)]
struct Fixture {
    seen: Mutex<Vec<Seen>>,
    invokes: AtomicUsize,
    prepares: AtomicUsize,
    park_first: bool,
    entered: tokio::sync::Notify,
}

impl Fixture {
    fn parking() -> Arc<Fixture> {
        Arc::new(Fixture { park_first: true, ..Fixture::default() })
    }

    fn record(&self, seen: Seen) {
        self.seen.lock().expect("seen lock").push(seen);
    }

    /// The first call of a counted method parks forever after telling the test it has
    /// arrived; later calls answer.
    async fn maybe_park(&self, count: &AtomicUsize) {
        if count.fetch_add(1, Ordering::SeqCst) == 0 && self.park_first {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
    }
}

fn cell(value: &str) -> Cell {
    Cell {
        kind: CellKind::Mono,
        value: value.into(),
        tone: Some(Tone::Warn),
        link: None,
        bar: Some(40),
        more: Vec::new(),
        row: false,
        label: None,
    }
}

/// `kick` is a `Then::Invoke` and `copy` a `Then::Copy`, so both variants without a spec cross
/// the wire.
fn fixture_manifest() -> Manifest {
    let kick = ActionDecl {
        id: "kick".into(),
        point: ActionPoint::PageHead("p".into()),
        label: "Kick".into(),
        icon: String::new(),
        order: 0,
        danger: true,
        multi: false,
        min_role: Role::Operator,
        then: Then::Invoke,
        implemented: true,
        emphasis: Emphasis::Default,
        trailing_icon: false,
        icon_only: false,
        when_empty: false,
        prepare: true,
    };
    let copy = ActionDecl {
        id: "copy".into(),
        point: ActionPoint::PanelTool("poison".into()),
        label: "Copy payload".into(),
        danger: false,
        then: Then::Copy,
        emphasis: Emphasis::Ghost,
        prepare: false,
        ..kick.clone()
    };
    Manifest {
        module: MODULE.into(),
        actions: vec![kick, copy],
        writes: true,
        ..Manifest::default()
    }
}

/// Step 9a's envelope with every field set, around a `PropertyList` carrying `code`.
fn fixture_panel() -> PanelBody {
    PanelBody {
        primitive: Primitive::PropertyList(PropertyList {
            items: vec![("Cursor".into(), cell("42"))],
            columns: 2,
            item: Some("evt-7".into()),
            code: Some("{\"payload\": true}".into()),
        }),
        meta: Some("since 14:02".into()),
        heading: Some("evt-7".into()),
        note: Some("last 4 h".into()),
        banner: Some(Banner { tone: Tone::Err, text: "poisoned".into() }),
        foot: Some("1 of 1".into()),
        tag: Some("paused".into()),
    }
}

fn fixture_link() -> Link {
    Link::Page { module: MODULE.into(), page: "p".into(), params: BTreeMap::from([("id".into(), "7".into())]) }
}

/// The prepared confirm Step 9a describes: its text is built from `PrepareCall.item`.
fn prepared_confirm(item: &str) -> Then {
    Then::Confirm(ConfirmSpec {
        title: format!("Skip {item}"),
        body: format!("Skips {item} only."),
        tone: ConfirmTone::Danger,
        impact: vec![("Event".into(), cell(item))],
        reason: ReasonReq::Required,
        reason_hint: Some("why".into()),
        type_to_confirm: Some(item.into()),
        confirm_label: "Skip".into(),
    })
}

#[async_trait]
impl Provider for Fixture {
    fn manifest(&self) -> Manifest {
        fixture_manifest()
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, opsapi::Error> {
        self.record(Seen::Panel(req));
        Ok(fixture_panel())
    }

    async fn search(&self, req: SearchQuery) -> Result<Vec<SearchHit>, opsapi::Error> {
        self.record(Seen::Search(req));
        Ok(vec![SearchHit {
            link: fixture_link(),
            title: "Hit".into(),
            subtitle: "sub".into(),
            badge: Some("new".into()),
        }])
    }

    async fn invoke(&self, req: InvokeCall) -> Result<InvokeOutcome, opsapi::Error> {
        self.maybe_park(&self.invokes).await;
        self.record(Seen::Invoke(req));
        Ok(InvokeOutcome::Ok { message: "kicked".into(), reveal: None, redirect: Some(fixture_link()) })
    }

    async fn prepare(&self, req: PrepareCall) -> Result<Then, opsapi::Error> {
        self.maybe_park(&self.prepares).await;
        let item = req.item.clone().unwrap_or_default();
        self.record(Seen::Prepare(req));
        Ok(prepared_confirm(&item))
    }
}

/// A real edge server with the `cp.*` face over `fixture`, on a loopback port.
fn serve(fixture: &Arc<Fixture>) -> edge::RunningServer {
    let mut server = edge::Server::new();
    register_face(&mut server, vec![fixture.clone() as Arc<dyn Provider>]);
    let ca = edge::shared_dev_ca().expect("shared dev CA");
    server.listen("127.0.0.1:0".parse().expect("loopback addr"), &ca).expect("edge listen")
}

/// The face a split portal holds: [`remote_factory`] applied by a real `remote::Stub`.
/// Dial `n` resolves `addrs[n]` (the last address repeats), and the returned counter
/// is the number of dials, so a test sees whether a redial happened and where it landed.
fn face_through_stub(addrs: Vec<SocketAddr>) -> (Source, Arc<AtomicUsize>) {
    let dials = Arc::new(AtomicUsize::new(0));
    let counted = dials.clone();
    let boot = addrs[0].to_string();
    let resolver: remote::PeerResolver = Arc::new(move || {
        let n = counted.fetch_add(1, Ordering::SeqCst);
        let addr = addrs[n.min(addrs.len() - 1)].to_string();
        let resolved: Pin<Box<dyn Future<Output = Result<String, String>> + Send>> =
            Box::pin(async move { Ok(addr) });
        resolved
    });
    let stub = remote::Stub::new(MODULE, remote::PeerSource::resolving(boot, resolver), vec![remote_factory(PEER)]);
    let ctx = lifecycle::Context::new();
    stub.register(&ctx).expect("stub register");
    let mut sources = ctx.contributions(SOURCE_SLOT);
    assert_eq!(sources.len(), 1, "remote_factory contributes exactly one source");
    let source = sources.remove(0);
    assert_eq!(source.peer, PEER);
    (source, dials)
}

fn panel_query() -> PanelQuery {
    PanelQuery {
        module: MODULE.into(),
        contribution: "poison".into(),
        entity: None,
        search: Some("q".into()),
        cursor: Some("c1".into()),
        before: None,
        queue_state: None,
        range: None,
        params: BTreeMap::from([("sub".into(), "s1".into())]),
        refs: Vec::new(),
    }
}

fn invoke_call() -> InvokeCall {
    InvokeCall {
        module: MODULE.into(),
        action: "kick".into(),
        entity: None,
        input: BTreeMap::from([("reason".into(), "spam".into())]),
        expected_revision: Some(3),
        reason: Some("spam".into()),
        operator: "op".into(),
        item: Some("evt-7".into()),
        request_id: Some("req-1".into()),
    }
}

fn prepare_call() -> PrepareCall {
    PrepareCall {
        module: MODULE.into(),
        action: "kick".into(),
        entity: None,
        item: Some("evt-7".into()),
        operator: "op".into(),
    }
}

/// Every `cp.*` method crosses a loopback edge with its request and answer intact,
/// including Step 9a's full `PanelBody` envelope, a manifest holding a `Then::Copy` action
/// and a prepared confirm built from the posted item; an invoke of the copy is refused
/// Invalid by the serving face and reaches no provider; a provider-side NotFound comes back
/// as NotFound, not as a transport error.
#[tokio::test(flavor = "multi_thread")]
async fn every_method_round_trips_over_a_loopback_edge() {
    let fixture = Arc::new(Fixture::default());
    let running = serve(&fixture);
    let (source, _dials) = face_through_stub(vec![running.local_addr()]);
    let face = source.face;

    assert_eq!(face.manifests().await.expect("manifests"), vec![fixture_manifest()]);

    assert_eq!(face.panel(panel_query()).await.expect("panel"), fixture_panel());

    let search = SearchQuery { module: MODULE.into(), kind: "player".into(), q: "ali".into(), limit: 5 };
    let hits = face.search(search.clone()).await.expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].link, fixture_link());
    assert_eq!(hits[0].badge.as_deref(), Some("new"));

    assert_eq!(
        face.invoke(invoke_call()).await.expect("invoke"),
        InvokeOutcome::Ok { message: "kicked".into(), reveal: None, redirect: Some(fixture_link()) }
    );

    assert_eq!(face.prepare(prepare_call()).await.expect("prepare"), prepared_confirm("evt-7"));

    let copy = InvokeCall { action: "copy".into(), ..invoke_call() };
    let err = face.invoke(copy).await.expect_err("a copy has no invoke");
    assert_eq!(err.status, opsapi::Status::Invalid, "the serving face refuses a copy: {}", err.msg);

    assert_eq!(
        *fixture.seen.lock().expect("seen lock"),
        vec![
            Seen::Panel(panel_query()),
            Seen::Search(search),
            Seen::Invoke(invoke_call()),
            Seen::Prepare(prepare_call()),
        ],
        "every request reached the provider unchanged"
    );

    let mut unknown = panel_query();
    unknown.module = "absent".into();
    let err = face.panel(unknown).await.expect_err("no provider for module absent");
    assert_eq!(err.status, opsapi::Status::NotFound);

    running.close();
}

/// Starts `call` on a face whose first peer is `a`, waits until the provider holds it,
/// then hard-closes `a` so the in-flight call fails connection-fatal. `b` is the address
/// the reconnecting caller resolves on a redial.
async fn kill_in_flight<T: Send + 'static>(
    fixture: &Arc<Fixture>,
    call: impl FnOnce(Arc<dyn ControlPlane>) -> Pin<Box<dyn Future<Output = Result<T, opsapi::Error>> + Send>>,
) -> (Result<T, opsapi::Error>, Arc<dyn ControlPlane>, usize, edge::RunningServer) {
    let a = serve(fixture);
    let b = serve(fixture);
    let (source, dials) = face_through_stub(vec![a.local_addr(), b.local_addr()]);
    let pending = tokio::spawn(call(source.face.clone()));
    tokio::time::timeout(WAIT, fixture.entered.notified()).await.expect("the provider never received the call");
    a.close();
    let result = tokio::time::timeout(WAIT, pending).await.expect("the call never returned").expect("call task");
    (result, source.face, dials.load(Ordering::SeqCst), b)
}

/// `invoke` is not `#[retry_safe]`, so the generated client sends it with
/// `RetryMode::Never`: when the connection dies with the call in flight, the caller
/// returns the error and never redials to send it again, though a live peer is waiting.
#[tokio::test(flavor = "multi_thread")]
async fn invoke_is_not_replayed_after_a_connection_fatal_error() {
    let fixture = Fixture::parking();
    let (result, face, dials, b) =
        kill_in_flight(&fixture, |face| Box::pin(async move { face.invoke(invoke_call()).await })).await;

    let err = result.expect_err("the killed invoke fails");
    assert_eq!(err.status, opsapi::Status::Unavailable);
    assert_eq!(fixture.invokes.load(Ordering::SeqCst), 1, "invoke ran once and was not replayed");
    assert_eq!(dials, 1, "no redial followed the connection-fatal error");

    // The redial target was live all along: the next, independent invoke reaches it.
    face.invoke(invoke_call()).await.expect("a new invoke reaches the second peer");
    assert_eq!(fixture.invokes.load(Ordering::SeqCst), 2);
    b.close();
}

/// The control for the test above: the same kill on the `#[retry_safe]` `prepare`
/// redials and replays once onto the live peer, so the harness does see a replay.
#[tokio::test(flavor = "multi_thread")]
async fn prepare_is_replayed_once_after_a_connection_fatal_error() {
    let fixture = Fixture::parking();
    let (result, _face, dials, b) =
        kill_in_flight(&fixture, |face| Box::pin(async move { face.prepare(prepare_call()).await })).await;

    assert_eq!(result.expect("the replay answers"), prepared_confirm("evt-7"));
    assert_eq!(fixture.prepares.load(Ordering::SeqCst), 2, "prepare ran once per connection");
    assert_eq!(dials, 2, "the connection-fatal error redialled once");
    b.close();
}
