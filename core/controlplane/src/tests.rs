use super::*;

fn attention(body: PanelBody) -> Vec<AttentionItem> {
    match body.primitive {
        Primitive::AttentionItems(items) => items,
        _ => panic!("the purge backlog item answers AttentionItems"),
    }
}

#[test]
fn purge_backlog_draws_nothing_at_zero_and_one_warn_item_linking_the_sweep_otherwise() {
    assert!(attention(purge_backlog_body("X purges pending", 0, "", "x.sweep.v1")).is_empty());
    let one = attention(purge_backlog_body("X purges pending", 1, "2026-11-20", "x.sweep.v1"));
    assert_eq!(one[0].text, "1 player awaiting sweep · oldest 2026-11-20");
    let items = attention(purge_backlog_body("X purges pending", 3, "2026-11-19", "x.sweep.v1"));
    assert_eq!(items.len(), 1, "a backlog is one item");
    assert_eq!(
        items[0],
        AttentionItem {
            severity: Severity::Warn,
            title: "X purges pending".to_string(),
            text: "3 players awaiting sweep · oldest 2026-11-19".to_string(),
            link: Link::Page {
                module: EVENT_LOG_PAGE_MODULE.to_string(),
                page: EVENT_LOG_PAGE.to_string(),
                params: BTreeMap::from([(PAGE_PARAM_SEARCH.to_string(), "x.sweep.v1".to_string())]),
            },
            action_label: "Events".to_string(),
            since: String::new(),
        }
    );
}

#[test]
fn ago_truncates_to_the_largest_unit_and_clamps_a_negative_age() {
    for (secs, want) in [
        (-5, "now"),
        (0, "now"),
        (59, "now"),
        (60, "1 min ago"),
        (3_599, "59 min ago"),
        (3_600, "1 h ago"),
        (86_399, "23 h ago"),
        (86_400, "1 d ago"),
        (12 * 86_400 + 5, "12 d ago"),
    ] {
        assert_eq!(ago(secs), want, "{secs}");
    }
}

#[test]
fn decode_money_reads_the_encoded_shape_and_refuses_any_other() {
    let value = FieldValue::Money { amount: "12.50".to_string(), currency: "gold".to_string() };
    let raw = encode_field(&FieldKind::Money(vec!["gold".to_string()]), &value);
    assert_eq!(decode_money(&raw), Ok(("12.50".to_string(), "gold".to_string())));
    for refused in [
        r#"{"amount":"1","currency":"gold","note":"x"}"#,
        r#"{"amount":"1"}"#,
        r#"{"currency":"gold"}"#,
        r#"{"amount":1,"currency":"gold"}"#,
        "not json",
        "",
    ] {
        assert_eq!(decode_money(refused), Err(MoneyShapeError), "{refused:?}");
    }
}

/// No wildcard arm: a new `Point` variant does not compile until it is named here.
fn written(point: &Point) -> &'static str {
    match point {
        Point::Page(_) => "Page",
        Point::DashboardTile => "DashboardTile",
        Point::DashboardAttention => "DashboardAttention",
        Point::DashboardPanel => "DashboardPanel",
        Point::DashboardWide => "DashboardWide",
        Point::EntityHeader(_) => "EntityHeader",
        Point::EntityTab { .. } => "EntityTab",
        Point::EntityTabCount { .. } => "EntityTabCount",
        Point::EntityTabTile { .. } => "EntityTabTile",
        Point::EntitySummary(_) => "EntitySummary",
        Point::EntityListColumn { .. } => "EntityListColumn",
        Point::EntityLabel { .. } => "EntityLabel",
        Point::NavCounter => "NavCounter",
    }
}

#[test]
fn variants_lists_every_variant_of_each_shape_in_order() {
    let s = String::new;
    let every = [
        Point::Page(s()),
        Point::DashboardTile,
        Point::DashboardAttention,
        Point::DashboardPanel,
        Point::DashboardWide,
        Point::EntityHeader(s()),
        Point::EntityTab { kind: s(), tab: s(), tab_label: s(), columns: 2, params: Vec::new() },
        Point::EntityTabCount { kind: s(), tab: s() },
        Point::EntityTabTile { kind: s(), tab: s() },
        Point::EntitySummary(s()),
        Point::EntityListColumn { kind: s(), label: s(), align: Align::Left },
        Point::EntityLabel { kind: s() },
        Point::NavCounter,
    ];
    assert_eq!(every.iter().map(written).collect::<Vec<_>>(), Point::VARIANTS);
    assert_eq!(every.iter().map(Variants::variant).collect::<Vec<_>>(), Point::VARIANTS);
    assert_eq!(Point::NAME, "Point");
    assert_eq!(Layout::VARIANTS, ["Stack", "Grid21", "Grid2", "Rows"]);
    assert_eq!(Layout::Grid21(Vec::new(), Vec::new()).variant(), "Grid21");
}

/// Proves the registry holds the enums named here once each with their own variants and
/// form; it cannot list the macro's call sites, so it does not prove nothing else is
/// registered.
#[test]
fn declared_enums_registers_each_wrapped_enum_once() {
    let registered: BTreeMap<&str, (&[&str], bool)> =
        declared_enums().iter().map(|e| (e.name, (e.variants, e.drawn))).collect();
    assert_eq!(registered.len(), declared_enums().len(), "a name is registered twice");
    for (name, variants, drawn) in [
        (Point::NAME, Point::VARIANTS, true),
        (Role::NAME, Role::VARIANTS, true),
        (Layout::NAME, Layout::VARIANTS, true),
        (ActionPoint::NAME, ActionPoint::VARIANTS, true),
        (Primitive::NAME, Primitive::VARIANTS, true),
        (InvokeOutcome::NAME, InvokeOutcome::VARIANTS, false),
        (FieldValue::NAME, FieldValue::VARIANTS, true),
        (FilterKind::NAME, FilterKind::VARIANTS, true),
    ] {
        assert_eq!(registered.get(name), Some(&(variants, drawn)), "{name}");
    }
    assert_eq!(registered.len(), 23);
}

#[test]
fn fake_hash_is_pinned_and_separates_salt_from_id() {
    assert_eq!(fake_hash("country", "p-1"), 7_496_518_075_959_870_503);
    assert_ne!(fake_hash("ab", "c"), fake_hash("a", "bc"));
}

#[test]
fn a_noun_takes_its_singular_at_one_only() {
    let noun = Noun::new("player", "players");
    assert_eq!([0, 1, 2].map(|n| noun.of(n)), ["players", "player", "players"]);
}

#[test]
fn a_reveal_stored_without_extra_still_decodes() {
    let stored = r#"{"label":"Key","value":"secret","hint":null,"expires_at":null}"#;
    let reveal: SecretReveal = serde_json::from_str(stored).expect("an older reveal row decodes");
    assert_eq!(reveal.extra, None);
}

#[test]
fn write_refusal_is_the_module_writes_and_the_contributions_readonly() {
    let action = |id: &str, point: ActionPoint, then: Then| ActionDecl {
        id: id.to_string(),
        point,
        label: id.to_string(),
        icon: String::new(),
        order: 0,
        danger: false,
        multi: false,
        min_role: Role::Operator,
        then,
        implemented: true,
        emphasis: Emphasis::Default,
        trailing_icon: false,
        icon_only: false,
        when_empty: false,
        prepare: false,
    };
    let link = Then::Link(Link::Page { module: "m".into(), page: "p".into(), params: BTreeMap::new() });
    let saves = ContributionDecl {
        id: "saves".into(),
        point: Point::Page("p".into()),
        order: 0,
        title: String::new(),
        icon: String::new(),
        min_role: Role::Viewer,
        cache_ttl_s: None,
        deadline_ms: 0,
        readonly: Some("inspection only".into()),
    };
    let mut m = Manifest {
        module: "m".into(),
        contributions: vec![saves],
        actions: vec![
            action("wipe", ActionPoint::PanelTool("saves".into()), Then::Invoke),
            action("open", ActionPoint::PanelTool("saves".into()), link),
            action("kick", ActionPoint::PageHead("p".into()), Then::Invoke),
        ],
        writes: true,
        ..Manifest::default()
    };
    let on_saves = Some(WriteRefusal { readonly: Some(("saves".into(), "inspection only".into())) });
    assert_eq!(m.write_refusal("wipe"), on_saves);
    assert_eq!(m.write_refusal("open"), None, "a link never writes");
    assert_eq!(m.write_refusal("kick"), None);
    assert_eq!(m.write_refusal("ghost"), None, "an undeclared action is judged by writes alone");
    m.writes = false;
    for id in ["wipe", "kick", "ghost"] {
        assert_eq!(m.write_refusal(id), Some(WriteRefusal { readonly: None }), "{id}");
    }
    assert_eq!(m.write_refusal("open"), None);
}

/// A remote module's manifest crosses the wire as JSON, so a tab's declared keys must
/// arrive with it for the portal to forward them.
#[test]
fn an_entity_tabs_params_survive_the_manifest_wire() {
    let point = Point::EntityTab {
        kind: "player".into(),
        tab: "inventory".into(),
        tab_label: "Inventory".into(),
        columns: 1,
        params: vec!["owner".into(), "type".into()],
    };
    let tab = ContributionDecl {
        id: "items".into(),
        point,
        order: 40,
        title: String::new(),
        icon: String::new(),
        min_role: Role::Viewer,
        cache_ttl_s: None,
        deadline_ms: 0,
        readonly: None,
    };
    let m = Manifest { module: "inventory".into(), contributions: vec![tab], ..Manifest::default() };
    let wire = serde_json::to_string(&m).expect("a manifest encodes");
    let back: Manifest = serde_json::from_str(&wire).expect("a manifest decodes");
    assert_eq!(back, m);
    assert!(matches!(
        &back.contributions[0].point,
        Point::EntityTab { params, .. } if params == &["owner", "type"]
    ));
}

// ---------------------------------------------------------------------------
// LocalFace routing and the write gate
// ---------------------------------------------------------------------------

/// A provider that answers every call with its own module name and logs which method ran.
struct Echo {
    manifest: Manifest,
    calls: std::sync::Mutex<Vec<&'static str>>,
}

impl Echo {
    fn new(manifest: Manifest) -> Arc<Echo> {
        Arc::new(Echo { manifest, calls: std::sync::Mutex::new(Vec::new()) })
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().expect("calls lock").clone()
    }

    fn log(&self, method: &'static str) {
        self.calls.lock().expect("calls lock").push(method);
    }
}

#[async_trait]
impl Provider for Echo {
    fn manifest(&self) -> Manifest {
        self.manifest.clone()
    }

    async fn panel(&self, _req: PanelQuery) -> Result<PanelBody, opsapi::Error> {
        self.log("panel");
        Ok(PanelBody::new(Primitive::Empty { title: self.manifest.module.clone(), detail: String::new() }))
    }

    async fn search(&self, _req: SearchQuery) -> Result<Vec<SearchHit>, opsapi::Error> {
        self.log("search");
        let link = Link::Page { module: self.manifest.module.clone(), page: "p".into(), params: BTreeMap::new() };
        Ok(vec![SearchHit { link, title: self.manifest.module.clone(), subtitle: String::new(), badge: None }])
    }

    async fn invoke(&self, _req: InvokeCall) -> Result<InvokeOutcome, opsapi::Error> {
        self.log("invoke");
        Ok(InvokeOutcome::NoOp { message: self.manifest.module.clone() })
    }

    async fn prepare(&self, _req: PrepareCall) -> Result<Then, opsapi::Error> {
        self.log("prepare");
        Ok(Then::Link(Link::Page { module: self.manifest.module.clone(), page: "p".into(), params: BTreeMap::new() }))
    }
}

fn invoke_decl(id: &str, point: ActionPoint) -> ActionDecl {
    ActionDecl {
        id: id.to_string(),
        point,
        label: id.to_string(),
        icon: String::new(),
        order: 0,
        danger: false,
        multi: false,
        min_role: Role::Operator,
        then: Then::Invoke,
        implemented: true,
        emphasis: Emphasis::Default,
        trailing_icon: false,
        icon_only: false,
        when_empty: false,
        prepare: true,
    }
}

fn writer(module: &str) -> Manifest {
    Manifest {
        module: module.into(),
        actions: vec![invoke_decl("kick", ActionPoint::PageHead("p".into()))],
        writes: true,
        ..Manifest::default()
    }
}

fn panel_query(module: &str) -> PanelQuery {
    PanelQuery {
        module: module.into(),
        contribution: "c".into(),
        entity: None,
        search: None,
        cursor: None,
        before: None,
        queue_state: None,
        range: None,
        params: BTreeMap::new(),
        refs: Vec::new(),
    }
}

fn search_query(module: &str) -> SearchQuery {
    SearchQuery { module: module.into(), kind: "player".into(), q: "x".into(), limit: 5 }
}

fn invoke_call(module: &str, action: &str) -> InvokeCall {
    InvokeCall {
        module: module.into(),
        action: action.into(),
        entity: None,
        input: BTreeMap::new(),
        expected_revision: None,
        reason: None,
        operator: "op".into(),
        item: None,
        request_id: None,
    }
}

fn prepare_call(module: &str) -> PrepareCall {
    PrepareCall { module: module.into(), action: "kick".into(), entity: None, item: None, operator: "op".into() }
}

fn as_provider(echo: &Arc<Echo>) -> Arc<dyn Provider> {
    echo.clone()
}

#[tokio::test]
async fn local_face_routes_every_method_by_the_request_module() {
    let (alpha, beta) = (Echo::new(writer("alpha")), Echo::new(writer("beta")));
    let face = LocalFace::new(vec![as_provider(&beta), as_provider(&alpha)]);
    let modules: Vec<String> = face.manifests().await.expect("manifests").into_iter().map(|m| m.module).collect();
    assert_eq!(modules, ["beta", "alpha"], "manifests keeps the providers' order");

    let body = face.panel(panel_query("beta")).await.expect("panel");
    assert!(matches!(body.primitive, Primitive::Empty { ref title, .. } if title == "beta"));
    let hits = face.search(search_query("beta")).await.expect("search");
    assert_eq!(hits[0].title, "beta");
    let outcome = face.invoke(invoke_call("beta", "kick")).await.expect("invoke");
    assert_eq!(outcome, InvokeOutcome::NoOp { message: "beta".into() });
    let then = face.prepare(prepare_call("beta")).await.expect("prepare");
    assert!(matches!(then, Then::Link(Link::Page { ref module, .. }) if module == "beta"));
    assert_eq!(beta.calls(), ["panel", "search", "invoke", "prepare"]);
    assert_eq!(alpha.calls(), Vec::<&str>::new(), "a request for beta reaches alpha");

    face.panel(panel_query("alpha")).await.expect("panel");
    assert_eq!(alpha.calls(), ["panel"]);
}

#[tokio::test]
async fn an_unknown_module_is_not_found_on_every_method_and_reaches_no_provider() {
    let alpha = Echo::new(writer("alpha"));
    let face = LocalFace::new(vec![as_provider(&alpha)]);
    let statuses = [
        face.panel(panel_query("ghost")).await.map(|_| ()),
        face.search(search_query("ghost")).await.map(|_| ()),
        face.invoke(invoke_call("ghost", "kick")).await.map(|_| ()),
        face.prepare(prepare_call("ghost")).await.map(|_| ()),
    ]
    .map(|r| r.expect_err("an unknown module is refused").status);
    assert_eq!(statuses, [opsapi::Status::NotFound; 4]);
    assert_eq!(alpha.calls(), Vec::<&str>::new());
}

#[test]
#[should_panic(expected = "two providers declare module \"alpha\"")]
fn two_providers_of_one_module_panic_at_construction() {
    let twice = [Echo::new(writer("alpha")), Echo::new(writer("alpha"))];
    let _ = LocalFace::new(twice.iter().map(as_provider).collect());
}

/// The module-writes half of `LocalFace::invoke`'s write gate: every invoke of a module
/// declaring `writes: false`, a declared action or not, is Forbidden and never reaches the
/// provider. `write_refusal_is_the_module_writes_and_the_contributions_readonly` pins the
/// rule; the read-only contribution half is
/// `admin::tests::the_local_face_refuses_a_write_on_a_readonly_contribution`.
#[tokio::test]
async fn local_face_refuses_every_invoke_of_a_module_without_writes() {
    let mut quiet = writer("quiet");
    quiet.writes = false;
    let quiet = Echo::new(quiet);
    let face = LocalFace::new(vec![as_provider(&quiet)]);

    for action in ["kick", "ghost"] {
        let err = face.invoke(invoke_call("quiet", action)).await.expect_err("writes: false refuses");
        assert_eq!(err.status, opsapi::Status::Forbidden, "{action}");
        assert!(err.msg.contains("does not declare writes"), "{action}: {}", err.msg);
    }
    assert_eq!(quiet.calls(), Vec::<&str>::new(), "a refused invoke reached the provider");
}

// ---------------------------------------------------------------------------
// Section merge
// ---------------------------------------------------------------------------

fn section(id: &str, label: &str, order: i32) -> NavSection {
    NavSection { id: id.into(), label: label.into(), order }
}

#[test]
fn merge_sections_keeps_a_fixed_id_and_gives_a_shared_id_to_the_smallest_module() {
    let with =
        |module: &str, sections: Vec<NavSection>| Manifest { module: module.into(), sections, ..Manifest::default() };
    // Passed out of name order: the merge sorts by module, not by argument position.
    let manifests = [
        with("zeta", vec![section("players", "Zeta players", 1), section("ops", "Ops", 50)]),
        with("beta", vec![section("social", "Beta social", 20)]),
        with("alpha", vec![section("social", "Alpha social", 25), section("social", "Alpha again", 5)]),
    ];
    let merged = merge_sections(&manifests);
    let drawn: Vec<(&str, &str, i32, Option<&str>)> = merged
        .sections
        .iter()
        .map(|s| (s.section.id.as_str(), s.section.label.as_str(), s.section.order, s.owner))
        .collect();
    assert_eq!(
        drawn,
        [
            ("overview", "Overview", 0, None),
            ("players", "Players", 10, None),
            ("social", "Alpha social", 25, Some("alpha")),
            ("economy", "Economy", 30, None),
            ("platform", "Platform", 40, None),
            ("ops", "Ops", 50, Some("zeta")),
        ]
    );
    let lost: Vec<(&str, &str)> = merged.lost.iter().map(|l| (l.module, l.declared.label.as_str())).collect();
    assert_eq!(lost, [("alpha", "Alpha again"), ("beta", "Beta social"), ("zeta", "Zeta players")]);
}

// ---------------------------------------------------------------------------
// The field codec (Step 8a decode rules)
// ---------------------------------------------------------------------------

fn opts(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

fn money(amount: &str, currency: &str) -> FieldValue {
    FieldValue::Money { amount: amount.into(), currency: currency.into() }
}

/// A kind, the raws it accepts with their value, and the raws it refuses.
type CodecRow = (FieldKind, Vec<(&'static str, FieldValue)>, Vec<&'static str>);

/// One row per kind of the Step 8a decode table.
fn codec_table() -> Vec<CodecRow> {
    let text = |s: &str| FieldValue::Text(s.into());
    vec![
        (FieldKind::String, vec![("", text("")), ("a b", text("a b"))], vec![]),
        (FieldKind::Text, vec![("", text("")), ("line\nline", text("line\nline"))], vec![]),
        (
            FieldKind::Int,
            vec![("0", FieldValue::Int(0)), ("-7", FieldValue::Int(-7)), ("01", FieldValue::Int(1))],
            vec!["", "1.5", " 1", "x", "9223372036854775808"],
        ),
        (
            FieldKind::Float,
            vec![("1.5", FieldValue::Float(1.5)), ("-2", FieldValue::Float(-2.0)), ("1e3", FieldValue::Float(1000.0))],
            vec!["", "x", "NaN", "inf", "-inf", "1e999"],
        ),
        (
            FieldKind::Bool,
            vec![("true", FieldValue::Bool(true)), ("false", FieldValue::Bool(false))],
            vec!["", "True", "1", "yes"],
        ),
        (FieldKind::Enum(opts(&["a", "b"])), vec![("a", text("a")), ("b", text("b"))], vec!["", "c", "A"]),
        (
            FieldKind::Multi(opts(&["a", "b"])),
            vec![
                ("[]", FieldValue::Multi(vec![])),
                (r#"["b","a"]"#, FieldValue::Multi(opts(&["b", "a"]))),
            ],
            vec!["", "a", r#"["a","a"]"#, r#"["c"]"#, "[1]", r#"{"a":1}"#],
        ),
        (
            FieldKind::Json,
            vec![(r#"{"a":[1]}"#, text(r#"{"a":[1]}"#)), ("null", text("null"))],
            vec!["", "{", "{a:1}"],
        ),
        (FieldKind::Ref("player".into()), vec![("p-1", text("p-1"))], vec![""]),
        (
            FieldKind::Money(opts(&["gold"])),
            vec![
                (r#"{"amount":"12.50","currency":"gold"}"#, money("12.50", "gold")),
                (r#"{"amount":"-3","currency":"gold"}"#, money("-3", "gold")),
            ],
            vec![
                "",
                r#"{"amount":"1.","currency":"gold"}"#,
                r#"{"amount":".5","currency":"gold"}"#,
                r#"{"amount":"1e3","currency":"gold"}"#,
                r#"{"amount":"","currency":"gold"}"#,
                r#"{"amount":"1","currency":"gems"}"#,
                r#"{"amount":1,"currency":"gold"}"#,
            ],
        ),
        (
            FieldKind::Duration,
            vec![("10s", text("10s")), ("5m", text("5m")), ("2h", text("2h")), ("7d", text("7d"))],
            vec!["", "s", "10", "10x", "1.5h", "-1s", "10S"],
        ),
        (FieldKind::RequestId, vec![], vec!["", "r-1"]),
    ]
}

/// No wildcard arm: a new `FieldKind` does not compile until the table names it.
fn table_row(kind: &FieldKind) -> &'static str {
    match kind {
        FieldKind::String => "String",
        FieldKind::Text => "Text",
        FieldKind::Int => "Int",
        FieldKind::Float => "Float",
        FieldKind::Bool => "Bool",
        FieldKind::Enum(_) => "Enum",
        FieldKind::Multi(_) => "Multi",
        FieldKind::Json => "Json",
        FieldKind::Ref(_) => "Ref",
        FieldKind::Money(_) => "Money",
        FieldKind::Duration => "Duration",
        FieldKind::RequestId => "RequestId",
    }
}

#[test]
fn decode_field_accepts_and_refuses_per_the_step_8a_table() {
    let table = codec_table();
    let rows: Vec<&str> = table.iter().map(|(kind, _, _)| table_row(kind)).collect();
    assert_eq!(rows, FieldKind::VARIANTS, "the table has one row per kind, in declaration order");
    for (kind, accepted, refused) in &table {
        for (raw, want) in accepted {
            assert_eq!(decode_field(kind, raw).as_ref(), Ok(want), "{kind:?} {raw:?}");
        }
        for raw in refused {
            assert!(decode_field(kind, raw).is_err(), "{kind:?} accepted {raw:?}");
        }
    }
}

/// value -> raw -> value is the identity; raw -> value -> raw may normalise.
#[test]
fn encode_field_round_trips_every_value_decode_field_returns() {
    for (kind, accepted, _) in codec_table() {
        for (raw, value) in accepted {
            let encoded = encode_field(&kind, &value);
            assert_eq!(decode_field(&kind, &encoded), Ok(value.clone()), "{kind:?} {raw:?} -> {encoded:?}");
        }
    }
    assert_eq!(encode_field(&FieldKind::Int, &FieldValue::Int(1)), "1", "\"01\" normalises to \"1\"");
    for value in [0.1, -0.0, 1e300, f64::MIN_POSITIVE, f64::MAX] {
        let encoded = encode_field(&FieldKind::Float, &FieldValue::Float(value));
        assert_eq!(decode_field(&FieldKind::Float, &encoded), Ok(FieldValue::Float(value)), "{value:e}");
    }
    let tricky = FieldValue::Multi(opts(&["a,b", "q\"t"]));
    let kind = FieldKind::Multi(opts(&["a,b", "q\"t"]));
    assert_eq!(decode_field(&kind, &encode_field(&kind, &tricky)), Ok(tricky));
}
