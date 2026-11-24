//! Panel resolution: the deadline, role, `NotFound` and `writes: false` states, the KPI
//! tile's stale and error copies, the dashboard range and the module banner.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use controlplane::{
    ActionDecl, ActionPoint, Banner, Cell, CellKind, ContributionDecl, Emphasis, FormSpec, InvokeCall, InvokeOutcome,
    KpiTile, Link, Manifest, Palette, PanelBody, PanelQuery, Point, Primitive, PropertyList, ReasonReq, Role,
    SearchHit, SearchQuery, Source, Then, TimeRange, Tone,
};

use super::*;
use crate::engine::manifests::{refresh_once, Snapshot};
use crate::engine::roles::Operator;
use crate::engine::Engine;

#[derive(Clone)]
enum Reply {
    Body(Box<Primitive>),
    Fail(opsapi::Error),
    Hang,
}

fn body(primitive: Primitive) -> Reply {
    Reply::Body(Box::new(primitive))
}

/// A provider of one module "m" that answers every panel with its current reply and counts
/// the calls.
struct Scripted {
    manifest: Manifest,
    reply: Mutex<Reply>,
    calls: AtomicU32,
}

impl Scripted {
    fn reply(&self, reply: Reply) {
        *self.reply.lock().unwrap() = reply;
    }

    fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl controlplane::Provider for Scripted {
    fn manifest(&self) -> Manifest {
        self.manifest.clone()
    }

    async fn panel(&self, _req: PanelQuery) -> Result<PanelBody, opsapi::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let reply = self.reply.lock().unwrap().clone();
        match reply {
            Reply::Body(primitive) => Ok(PanelBody::new(*primitive)),
            Reply::Fail(error) => Err(error),
            Reply::Hang => std::future::pending().await,
        }
    }

    async fn search(&self, _req: SearchQuery) -> Result<Vec<SearchHit>, opsapi::Error> {
        Ok(Vec::new())
    }

    async fn invoke(&self, _req: InvokeCall) -> Result<InvokeOutcome, opsapi::Error> {
        Err(opsapi::Error::forbidden("a scripted provider writes nothing"))
    }
}

fn panel(id: &str, min_role: Role) -> ContributionDecl {
    ContributionDecl {
        id: id.to_string(),
        point: Point::Page("p".to_string()),
        order: 0,
        title: id.to_string(),
        icon: String::new(),
        min_role,
        cache_ttl_s: None,
        deadline_ms: 0,
        readonly: None,
    }
}

fn action(id: &str, point: ActionPoint, then: Then) -> ActionDecl {
    ActionDecl {
        id: id.to_string(),
        point,
        label: id.to_string(),
        icon: String::new(),
        order: 0,
        danger: false,
        multi: false,
        min_role: Role::Viewer,
        then,
        implemented: true,
        emphasis: Emphasis::Default,
        trailing_icon: false,
        icon_only: false,
        when_empty: false,
        prepare: false,
    }
}

fn props() -> Primitive {
    let cell = Cell {
        kind: CellKind::Text,
        value: "12".to_string(),
        tone: None,
        link: None,
        bar: None,
        more: Vec::new(),
        row: false,
        label: None,
    };
    Primitive::PropertyList(PropertyList { items: vec![("Level".to_string(), cell)], columns: 1, item: None, code: None })
}

fn tile(unit: Option<&str>) -> Primitive {
    Primitive::KpiTile(KpiTile {
        label: "Players".to_string(),
        value: "12".to_string(),
        unit: unit.map(str::to_string),
        delta: None,
        series: Vec::new(),
        spark: Palette::Chart1,
        capacity: None,
        link: None,
    })
}

fn form() -> FormSpec {
    FormSpec {
        fields: Vec::new(),
        hidden: BTreeMap::new(),
        submit_label: "Save".to_string(),
        note: None,
        revision: None,
        title: None,
        modes: Vec::new(),
        reason: ReasonReq::None,
        reason_label: None,
        reason_hint: None,
    }
}

/// An engine serving module "m" (`decl` and `actions`) from one source, as a split peer.
async fn single(decl: ContributionDecl, actions: Vec<ActionDecl>, writes: bool, reply: Reply) -> (Engine, Arc<Scripted>) {
    let manifest = Manifest { module: "m".to_string(), contributions: vec![decl], actions, writes, ..Manifest::default() };
    let scripted = Arc::new(Scripted { manifest, reply: Mutex::new(reply), calls: AtomicU32::new(0) });
    let provider: Arc<dyn controlplane::Provider> = scripted.clone();
    let source = Source { peer: "m-svc".to_string(), face: Arc::new(controlplane::LocalFace::new(vec![provider])) };
    let engine = Engine::new();
    engine.install(Snapshot::seeded(vec![source], None));
    refresh_once(&engine).await;
    (engine, scripted)
}

fn query(decl: &ContributionDecl, range: Option<TimeRange>) -> PanelQuery {
    PanelQuery {
        module: "m".to_string(),
        contribution: decl.id.clone(),
        entity: None,
        search: None,
        cursor: None,
        before: None,
        queue_state: None,
        range,
        params: BTreeMap::new(),
        refs: Vec::new(),
    }
}

async fn resolve_as(engine: &Engine, role: Role, decl: &ContributionDecl) -> PanelView {
    let operator = Operator { login: "ops".to_string(), role };
    super::resolve(engine, &engine.snapshot(), &operator, "m", decl, query(decl, None)).await
}

async fn resolve_admin(engine: &Engine, decl: &ContributionDecl) -> PanelView {
    resolve_as(engine, Role::Admin, decl).await
}

/// A provider that does not answer within the deadline is an error with nothing cached;
/// with a fresh cached copy it is stale on that copy, the deadline kept as its error.
#[tokio::test(start_paused = true)]
async fn a_panel_past_its_deadline_is_an_error_or_stale_on_a_fresh_copy() {
    let cold = ContributionDecl { deadline_ms: 50, ..panel("load", Role::Viewer) };
    let (engine, scripted) = single(cold.clone(), Vec::new(), true, Reply::Hang).await;
    let view = resolve_admin(&engine, &cold).await;
    assert_eq!(view.state, PanelState::Error);
    assert!(view.body.is_none());
    assert_eq!(view.error.as_deref(), Some("deadline of 50ms elapsed"));
    assert_eq!(scripted.calls(), 1);

    let warm = ContributionDecl { cache_ttl_s: Some(3600), ..cold };
    let (engine, scripted) = single(warm.clone(), Vec::new(), true, body(props())).await;
    assert_eq!(resolve_admin(&engine, &warm).await.state, PanelState::Ok, "the first answer is cached");
    scripted.reply(Reply::Hang);
    let view = resolve_admin(&engine, &warm).await;
    assert_eq!(view.state, PanelState::Stale);
    assert_eq!(view.body.map(|b| b.primitive), Some(props()), "the cached copy");
    assert_eq!(view.error.as_deref(), Some("deadline of 50ms elapsed"));
    assert!(view.age_s.is_some());
    assert!(view.last_value.is_none(), "a stale panel shows its copy, never a last value");
}

/// An operator below the contribution's `min_role` gets `denied` and the provider is never
/// called; at the role it is called.
#[tokio::test]
async fn a_role_below_min_role_is_denied_with_no_provider_call() {
    let decl = panel("load", Role::Operator);
    let (engine, scripted) = single(decl.clone(), Vec::new(), true, body(props())).await;
    let view = resolve_as(&engine, Role::Viewer, &decl).await;
    assert_eq!(view.state, PanelState::Denied);
    assert!(view.body.is_none() && view.error.is_none());
    assert_eq!(scripted.calls(), 0, "a denied panel makes no call");
    assert_eq!(resolve_as(&engine, Role::Operator, &decl).await.state, PanelState::Ok);
    assert_eq!(scripted.calls(), 1);
}

/// A `NotFound` answer is `empty` with no body and no error; any other error status is
/// `error`.
#[tokio::test]
async fn a_not_found_answer_is_empty_and_another_error_is_an_error() {
    let decl = panel("load", Role::Viewer);
    let (engine, scripted) = single(decl.clone(), Vec::new(), true, Reply::Fail(opsapi::Error::not_found("gone"))).await;
    let view = resolve_admin(&engine, &decl).await;
    assert_eq!(view.state, PanelState::Empty);
    assert!(view.body.is_none() && view.error.is_none(), "{:?}", view.error);
    scripted.reply(Reply::Fail(opsapi::Error::unavailable("down")));
    let view = resolve_admin(&engine, &decl).await;
    assert_eq!(view.state, PanelState::Error);
    assert!(view.error.as_deref().is_some_and(|e| e.contains("down")), "{:?}", view.error);
}

/// On a module with `writes: false`: a panel without a write affordance, or whose only tool
/// is a `Then::Link`, is `ok`; one with a `PanelForm` action or a `Form` body is
/// `readonly`. The same `Form` body on a module that writes is `ok`.
#[tokio::test]
async fn a_writes_false_panel_is_readonly_only_with_a_write_affordance() {
    let decl = panel("load", Role::Viewer);
    let link = Then::Link(Link::Page { module: "m".to_string(), page: "p".to_string(), params: BTreeMap::new() });
    let cases: Vec<(&str, Vec<ActionDecl>, Primitive, bool, PanelState)> = vec![
        ("no action", Vec::new(), props(), false, PanelState::Ok),
        ("a link tool", vec![action("open", ActionPoint::PanelTool("load".to_string()), link)], props(), false, PanelState::Ok),
        (
            "a panel form action",
            vec![action("save", ActionPoint::PanelForm("load".to_string()), Then::Invoke)],
            props(),
            false,
            PanelState::Readonly,
        ),
        ("a form body", Vec::new(), Primitive::Form(form()), false, PanelState::Readonly),
        ("a form body on a writing module", Vec::new(), Primitive::Form(form()), true, PanelState::Ok),
    ];
    for (what, actions, primitive, writes, want) in cases {
        let (engine, _) = single(decl.clone(), actions, writes, body(primitive)).await;
        let view = resolve_admin(&engine, &decl).await;
        assert_eq!(view.state, want, "{what}: {:?}", view.error);
    }
}

/// A failed KPI tile with a copy younger than its ttl is stale on it; with an expired copy it
/// is an error showing the copy's value and unit; a non-tile expired copy shows no value.
#[tokio::test]
async fn a_failed_kpi_tile_is_stale_on_a_fresh_copy_and_an_error_with_an_expired_value() {
    let fresh = ContributionDecl { cache_ttl_s: Some(3600), ..panel("players", Role::Viewer) };
    let (engine, scripted) = single(fresh.clone(), Vec::new(), true, body(tile(Some("k")))).await;
    assert_eq!(resolve_admin(&engine, &fresh).await.state, PanelState::Ok);
    scripted.reply(Reply::Fail(opsapi::Error::internal("boom")));
    let view = resolve_admin(&engine, &fresh).await;
    assert_eq!(view.state, PanelState::Stale);
    assert_eq!(view.body.map(|b| b.primitive), Some(tile(Some("k"))));
    assert!(view.last_value.is_none());

    // A zero ttl expires the copy as it is written, so the next failure reads it expired.
    let expired = ContributionDecl { cache_ttl_s: Some(0), ..fresh };
    for (unit, want) in [(Some("k"), "12 k"), (None, "12")] {
        let (engine, scripted) = single(expired.clone(), Vec::new(), true, body(tile(unit))).await;
        assert_eq!(resolve_admin(&engine, &expired).await.state, PanelState::Ok);
        scripted.reply(Reply::Fail(opsapi::Error::internal("boom")));
        let view = resolve_admin(&engine, &expired).await;
        assert_eq!(view.state, PanelState::Error);
        assert!(view.body.is_none() && view.age_s.is_none());
        assert_eq!(view.last_value.map(|v| v.value).as_deref(), Some(want));
    }

    let (engine, scripted) = single(expired.clone(), Vec::new(), true, body(props())).await;
    assert_eq!(resolve_admin(&engine, &expired).await.state, PanelState::Ok);
    scripted.reply(Reply::Fail(opsapi::Error::internal("boom")));
    let view = resolve_admin(&engine, &expired).await;
    assert_eq!(view.state, PanelState::Error);
    assert!(view.last_value.is_none(), "only a KPI tile shows a last value");
}

/// A cached copy is fresh within its ttl, expired past it until [`LAST_VALUE_MAX_AGE`] old,
/// and gone after that.
#[test]
fn a_cached_copy_is_fresh_then_expired_then_gone_after_an_hour() {
    let mut cache = PanelCache::default();
    let ttl = Duration::from_secs(60);
    cache.put("k".to_string(), PanelBody::new(tile(None)), ttl);
    let fetched = cache.entries["k"].1;
    assert!(matches!(cache.get("k", fetched), Some(Cached::Fresh(_, _))));
    assert!(matches!(cache.get("k", fetched + ttl), Some(Cached::Expired(_, _))), "the ttl is exclusive");
    assert!(matches!(cache.get("k", fetched + LAST_VALUE_MAX_AGE), Some(Cached::Expired(_, _))));
    assert!(cache.get("k", fetched + LAST_VALUE_MAX_AGE + Duration::from_secs(1)).is_none());
    assert!(cache.get("other", fetched).is_none());
}

/// An absent `range` is 24 h, each switch value parses to its range and back, and any other
/// value is refused.
#[test]
fn parse_range_defaults_to_24h_and_refuses_an_unknown_value() {
    assert_eq!(parse_range(None), Ok(TimeRange::H24));
    for (range, key, _) in RANGES {
        assert_eq!(parse_range(Some(key)), Ok(range));
        assert_eq!(range_key(range), key);
    }
    for raw in ["", "30d", "24H", " 24h"] {
        assert!(parse_range(Some(raw)).is_err(), "{raw:?}");
    }
}

/// A dashboard request without `range` keys the cache as one asking for 24 h, and the
/// ranges, or no range at all, key apart.
#[test]
fn an_absent_range_shares_the_24h_cache_key_and_each_range_keys_apart() {
    let decl = panel("load", Role::Viewer);
    let absent = cache_key(&query(&decl, Some(parse_range(None).unwrap())));
    let named = cache_key(&query(&decl, Some(parse_range(Some("24h")).unwrap())));
    assert_eq!(absent, named);
    let keys: std::collections::BTreeSet<String> =
        [None, Some(TimeRange::H1), Some(TimeRange::H24), Some(TimeRange::D7)]
            .into_iter()
            .map(|range| cache_key(&query(&decl, range)))
            .collect();
    assert_eq!(keys.len(), 4);
}

/// The search text is part of the cache key: each search writes its own last-good copy, and
/// a failed answer to a search falls back to that search's copy, never another's.
#[tokio::test]
async fn each_search_keys_its_own_cached_copy() {
    let decl = ContributionDecl { cache_ttl_s: Some(3600), ..panel("load", Role::Viewer) };
    let (engine, scripted) = single(decl.clone(), Vec::new(), true, body(props())).await;
    let operator = Operator { login: "ops".to_string(), role: Role::Admin };
    let searched = |text: &str| PanelQuery { search: Some(text.to_string()), ..query(&decl, None) };
    let view = super::resolve(&engine, &engine.snapshot(), &operator, "m", &decl, searched("a")).await;
    assert_eq!(view.body.map(|b| b.primitive), Some(props()));
    assert_eq!(scripted.calls(), 1);

    scripted.reply(body(tile(None)));
    let view = super::resolve(&engine, &engine.snapshot(), &operator, "m", &decl, searched("b")).await;
    assert_eq!(view.body.map(|b| b.primitive), Some(tile(None)), "another search is another key");
    assert_eq!(scripted.calls(), 2);

    scripted.reply(Reply::Fail(opsapi::Error::internal("boom")));
    for (text, want) in [("a", props()), ("b", tile(None))] {
        let view = super::resolve(&engine, &engine.snapshot(), &operator, "m", &decl, searched(text)).await;
        assert_eq!(view.state, PanelState::Stale, "search {text}");
        assert_eq!(view.body.map(|b| b.primitive), Some(want), "search {text} falls back to its own copy");
    }
    assert_eq!(scripted.calls(), 4);
}

/// The module's banner shows in `ok` and `readonly` only.
#[test]
fn the_module_banner_shows_only_in_ok_and_readonly() {
    let banner = Banner { tone: Tone::Info, text: "maintenance".to_string() };
    let states = [
        (PanelState::Ok, true),
        (PanelState::Readonly, true),
        (PanelState::Empty, false),
        (PanelState::Stale, false),
        (PanelState::Error, false),
        (PanelState::Denied, false),
    ];
    for (state, shown) in states {
        assert_eq!(module_banner(state, Some(&banner)).is_some(), shown, "{state:?}");
        assert!(module_banner(state, None).is_none());
    }
}
