use super::*;

// ---- view-model builders -----------------------------------------------------------

pub(super) fn cell(kind: CellKind, value: &str) -> Cell {
    Cell { kind, value: value.to_string(), tone: None, link: None, bar: None, more: Vec::new(), row: false, label: None }
}

pub(super) fn toned(kind: CellKind, value: &str, tone: Tone) -> Cell {
    Cell { tone: Some(tone), ..cell(kind, value) }
}

pub(super) fn bar(value: &str, pct: u8, tone: Option<Tone>) -> Cell {
    Cell { bar: Some(pct), tone, ..cell(CellKind::Mono, value) }
}

pub(super) fn entity(kind: &str, id: &str) -> EntityRef {
    EntityRef { kind: kind.to_string(), id: id.to_string() }
}

fn column(key: &str, label: &str, align: Align) -> Column {
    Column {
        key: key.to_string(),
        label: label.to_string(),
        kind: CellKind::Text,
        sortable: false,
        align,
        sorted: None,
        wrap: false,
    }
}

pub(super) fn page_link(module: &str, page: &str) -> Link {
    Link::Page { module: module.to_string(), page: page.to_string(), params: BTreeMap::new() }
}

pub(super) fn props(items: Vec<(&str, Cell)>) -> PropertyList {
    PropertyList {
        items: items.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        columns: 1,
        item: None,
        code: None,
    }
}

pub(super) fn summary() -> Primitive {
    Primitive::PropertyList(props(vec![
        ("Version", cell(CellKind::Mono, "1.4.2")),
        ("State", cell(CellKind::Badge, "running")),
        ("Region", cell(CellKind::Text, "eu-west")),
    ]))
}

pub(super) fn body(primitive: Primitive) -> PanelBody {
    PanelBody::new(primitive)
}

pub(super) fn decl(id: &str, label: &str, icon: &str, then: Then) -> ActionDecl {
    ActionDecl {
        id: id.to_string(),
        point: ActionPoint::PanelTool(CONTRIBUTION.to_string()),
        label: label.to_string(),
        icon: icon.to_string(),
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

pub(super) fn confirm(title: &str, tone: ConfirmTone) -> ConfirmSpec {
    ConfirmSpec {
        title: title.to_string(),
        body: String::new(),
        tone,
        impact: Vec::new(),
        reason: ReasonReq::None,
        reason_hint: None,
        type_to_confirm: None,
        confirm_label: "Confirm".to_string(),
    }
}

pub(super) fn operator(role: Role) -> Operator {
    Operator { login: "golden".to_string(), role }
}

/// `d` as `routes::action_item` builds it for an operator of `role` on a module that `writes`,
/// unbound, as `routes::panel_tools` hands it to a panel.
pub(super) fn action(module: &str, d: &ActionDecl, role: Role, writes: bool) -> ActionItem {
    routes::action_item(&operator(role), CSRF, module, &declaring(d, writes), d, None)
}

/// A manifest declaring only `d`, writing when `writes`.
pub(super) fn declaring(d: &ActionDecl, writes: bool) -> Manifest {
    Manifest { writes, actions: vec![d.clone()], ..Manifest::default() }
}

pub(super) fn tool(d: &ActionDecl) -> ActionItem {
    action(MODULE, d, Role::Admin, true)
}

/// One `render::panel` input: the fields `routes::panel_fragment` fills from a resolved
/// `PanelView`, owned here.
pub(super) struct PanelCase {
    pub(super) title: &'static str,
    pub(super) icon: &'static str,
    pub(super) mode: &'static str,
    pub(super) state: PanelState,
    pub(super) query: Vec<(String, String)>,
    pub(super) entity: Option<EntityRef>,
    pub(super) search: Option<&'static str>,
    pub(super) error: Option<&'static str>,
    pub(super) age_s: Option<u64>,
    pub(super) last_value: Option<LastValue>,
    pub(super) min_role: &'static str,
    pub(super) fallback_title: &'static str,
    pub(super) readonly: Option<String>,
    pub(super) body: Option<PanelBody>,
    pub(super) actions: Vec<ActionItem>,
    pub(super) list_columns: Vec<ListColumn>,
    pub(super) labels: Labels,
    pub(super) row_menu_kinds: Vec<String>,
    pub(super) header_kinds: Vec<String>,
    /// Renders the form with a fresh instance to post as.
    pub(super) form: bool,
}

impl PanelCase {
    pub(super) fn new(state: PanelState, body: Option<PanelBody>) -> PanelCase {
        PanelCase {
            title: "Golden panel",
            icon: "server",
            mode: "panel",
            state,
            query: Vec::new(),
            entity: None,
            search: None,
            error: None,
            age_s: None,
            last_value: None,
            min_role: "viewer",
            fallback_title: "",
            readonly: None,
            body,
            actions: Vec::new(),
            list_columns: Vec::new(),
            labels: Labels::new(),
            row_menu_kinds: Vec::new(),
            header_kinds: strings(&["player"]),
            form: false,
        }
    }

    pub(super) fn tile(state: PanelState, body: Option<PanelBody>) -> PanelCase {
        PanelCase { title: "Players online", icon: "users", mode: "tile", ..PanelCase::new(state, body) }
    }

    pub(super) fn render(&self, env: &Environment<'_>) -> Result<String, minijinja::Error> {
        self.with_view(|view| render::fragment(env, "primitives.html", "panel_fragment", view))
    }

    /// The case's `render::panel` view model as JSON: what the template is handed, including
    /// the fields no template draws yet.
    pub(super) fn view(&self) -> serde_json::Value {
        self.with_view(|view| serde_json::to_value(view).expect("a panel view serializes"))
    }

    fn with_view<T>(&self, f: impl FnOnce(&render::PanelRender) -> T) -> T {
        let path = format!("/admin/_panel/{MODULE}/{CONTRIBUTION}");
        let no_errors = BTreeMap::new();
        let input = PanelInput {
            module: MODULE,
            contribution: CONTRIBUTION,
            title: self.title,
            icon: self.icon,
            mode: self.mode,
            state: self.state,
            path: &path,
            query: &self.query,
            entity: self.entity.as_ref(),
            search: self.search,
            error: self.error,
            age_s: self.age_s,
            last_value: self.last_value.as_ref(),
            min_role: self.min_role,
            fallback_title: self.fallback_title,
            readonly: self.readonly.as_deref(),
            body: self.body.as_ref(),
            actions: &self.actions,
            list_columns: &self.list_columns,
            labels: &self.labels,
            row_menu_kinds: &self.row_menu_kinds,
            header_kinds: &self.header_kinds,
            csrf: CSRF,
            form: self.form.then(|| FormInstance {
                post_url: routes::panel_form_save(&operator(Role::Admin), MODULE, &save_manifest(), CONTRIBUTION)
                    .expect("an admin may save")
                    .1,
                form_id: FORM_ID.to_string(),
                posted: None,
                errors: &no_errors,
                banner: None,
                locked: false,
            }),
        };
        f(&render::panel(&input))
    }
}

/// A writing module whose one action is the `PanelForm` Save a form case posts to.
fn save_manifest() -> Manifest {
    Manifest {
        module: MODULE.to_string(),
        actions: vec![ActionDecl {
            point: ActionPoint::PanelForm(CONTRIBUTION.to_string()),
            ..decl("save", "Save", "check", Then::Invoke)
        }],
        writes: true,
        ..Manifest::default()
    }
}

pub(super) fn slot(mode: &'static str, denied: bool) -> SlotView {
    SlotView {
        module: MODULE.to_string(),
        contribution: CONTRIBUTION.to_string(),
        title: "Golden panel".to_string(),
        icon: "server".to_string(),
        url: format!("/admin/_panel/{MODULE}/{CONTRIBUTION}"),
        denied,
        min_role: "admin",
        mode,
    }
}

pub(super) fn contribution(id: &str, point: Point, min_role: Role) -> ContributionDecl {
    ContributionDecl {
        id: id.to_string(),
        point,
        order: 0,
        title: "Golden panel".to_string(),
        icon: "server".to_string(),
        min_role,
        cache_ttl_s: None,
        deadline_ms: 800,
        readonly: None,
    }
}

/// The slot `routes::slot` builds for contribution `id` at `point` for a Viewer, so a higher
/// `min_role` draws as denied.
pub(super) fn point_slot(id: &str, point: Point, min_role: Role, tiles: bool) -> SlotView {
    routes::slot(&operator(Role::Viewer), MODULE, &contribution(id, point, min_role), None, tiles)
}

/// The shell of a screen for a Viewer, with nothing in its nav, popovers or flash.
pub(super) fn shell_view(crumb: &str) -> ShellView {
    ShellView {
        operator: OperatorView::new("golden", crate::role_name(Role::Viewer)),
        csrf: CSRF.to_string(),
        sections: Vec::new(),
        active: None,
        crumbs: vec![Crumb { label: crumb.to_string(), href: None }],
        scopes: Vec::new(),
        flash: None,
        deployment: Some("emberline-prod".to_string()),
        health: None,
        complete: true,
    }
}

/// `view` drawn as screen `name` through `render::screen_page`, cut from the `.app` root to the
/// end of `<main>`: what follows, the popovers and the search palette, and the document head
/// before it are the same on every screen, pinned once by the uncut `screen_shell_page` case.
pub(super) fn screen(env: &Environment<'_>, name: &str, view: &impl serde::Serialize) -> Result<String, minijinja::Error> {
    let raw = render::screen_page(env, name, view)?;
    let (open, close) = ("<div class=\"app\"", "</main>");
    let start = raw.find(open).expect("the shell draws its app frame");
    let end = start + raw[start..].find(close).expect("<main> closes") + close.len();
    Ok(raw[start..end].to_string())
}

pub(super) fn kpi(delta: Option<Delta>) -> KpiTile {
    KpiTile {
        label: "Players online".to_string(),
        value: "12,408".to_string(),
        unit: None,
        delta,
        series: vec![3.0, 5.0, 4.0, 6.0, 8.0],
        spark: Palette::Chart1,
        capacity: None,
        link: None,
    }
}

pub(super) fn delta(value: &str, tone: Option<Tone>) -> Option<Delta> {
    Some(Delta { value: value.to_string(), period: "vs yesterday".to_string(), tone })
}

fn service(name: &str, state: ServiceState, value: Option<&str>, note: Option<&str>) -> ServiceItem {
    ServiceItem {
        name: name.to_string(),
        state,
        version: Some("1.4.2".to_string()),
        value: value.map(str::to_string),
        uptime: None,
        note: note.map(str::to_string),
        history: vec![
            ("10:00".to_string(), ServiceState::Running),
            ("10:05".to_string(), ServiceState::Degraded),
            ("10:10".to_string(), ServiceState::Down),
            ("10:15".to_string(), state),
        ],
    }
}

pub(super) fn services(count: usize, columns: u8, value_label: Option<&str>) -> Primitive {
    let all = [
        service("gateway-svc", ServiceState::Running, Some("33 ms"), None),
        service("billing-svc", ServiceState::Degraded, Some("lag 4 s"), Some("replica lagging")),
        service("chat-svc", ServiceState::Restarting, None, None),
        service("mail-svc", ServiceState::Crashed, Some("—"), Some("exit 137")),
        service("lore-svc", ServiceState::Down, None, None),
    ];
    Primitive::ServiceStatus(ServiceStatus {
        items: all.into_iter().take(count).collect(),
        columns,
        value_label: value_label.map(str::to_string),
    })
}

pub(super) fn field(key: &str, label: &str, kind: FieldKind, default: Option<&str>) -> Field {
    Field {
        key: key.to_string(),
        label: label.to_string(),
        kind,
        required: false,
        help: None,
        default: default.map(str::to_string),
        readonly: false,
        mono: false,
        placeholder: None,
    }
}

pub(super) fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

/// One field of every `FieldKind`.
pub(super) fn profile_form() -> FormSpec {
    FormSpec {
        fields: vec![
            Field {
                required: true,
                help: Some("Shown to other players.".to_string()),
                ..field("display_name", "Display name", FieldKind::String, Some("Anna"))
            },
            field("bio", "Bio", FieldKind::Text, None),
            field("level", "Level", FieldKind::Int, Some("12")),
            field("ratio", "Drop ratio", FieldKind::Float, Some("0.25")),
            field("muted", "Muted", FieldKind::Bool, Some("true")),
            field("tier", "Tier", FieldKind::Enum(strings(&["free", "plus", "pro"])), Some("plus")),
            field("regions", "Regions", FieldKind::Multi(strings(&["eu", "us", "asia"])), Some(r#"["eu","asia"]"#)),
            Field { readonly: true, ..field("meta", "Meta", FieldKind::Json, Some(r#"{"v":1}"#)) },
            field("owner", "Owner", FieldKind::Ref("player".to_string()), Some("p-1001")),
            field(
                "credit",
                "Credit",
                FieldKind::Money(strings(&["EUR", "USD"])),
                Some(r#"{"amount":"9.99","currency":"EUR"}"#),
            ),
            field("cooldown", "Cooldown", FieldKind::Duration, Some("30m")),
            field("request_id", "Request id", FieldKind::RequestId, None),
        ],
        hidden: BTreeMap::new(),
        submit_label: "Save profile".to_string(),
        note: Some("Changes apply on next login.".to_string()),
        revision: Some(3),
        title: None,
        modes: Vec::new(),
        reason: ReasonReq::None,
        reason_label: None,
        reason_hint: None,
    }
}

/// The wallet's Grant or revoke form (pages-platform.js:172-176): two modes, each with its
/// own submit label, over one set of fields ending in the request id.
pub(super) fn moded_form() -> FormSpec {
    let mode = |id: &str, label: &str| controlplane::FormMode {
        id: id.to_string(),
        label: label.to_string(),
        action: id.to_string(),
        submit_label: label.to_string(),
    };
    FormSpec {
        fields: vec![
            Field { required: true, ..field("player", "Player", FieldKind::Ref("user".to_string()), None) },
            field("amount", "Amount", FieldKind::Money(strings(&["EMB", "GLD"])), None),
            Field { required: true, ..field("reason", "Reason", FieldKind::String, None) },
            field("request_id", "Request id", FieldKind::RequestId, None),
        ],
        hidden: BTreeMap::new(),
        submit_label: "Apply".to_string(),
        note: Some("Idempotent per request id.".to_string()),
        revision: None,
        title: None,
        modes: vec![mode("grant", "Grant"), mode("revoke", "Revoke")],
        reason: ReasonReq::None,
        reason_label: None,
        reason_hint: None,
    }
}

pub(super) fn posted(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

pub(super) fn player_cell(id: &str) -> Cell {
    Cell { link: Some(Link::Entity(entity("player", id))), ..cell(CellKind::User, id) }
}

/// The players `DataTable` of the `table_*` cases.
pub(super) fn players_table(rows: Vec<Row>, total: Option<u64>, next: Option<&str>) -> Primitive {
    Primitive::DataTable(DataTable {
        columns: vec![
            column("player", "Player", Align::Left),
            column("level", "Level", Align::Right),
            column("status", "Status", Align::Left),
        ],
        rows,
        page: PageInfo { next_cursor: next.map(str::to_string), size: 25, total },
        filters: vec![FilterDef {
            key: "status".to_string(),
            label: "Status".to_string(),
            kind: FilterKind::Enum(strings(&["active", "banned"])),
        }],
        search_placeholder: Some("Search players".to_string()),
        selectable: true,
        list_columns_for: None,
        list_columns_at: 0,
        item_noun: None,
    })
}

pub(super) fn player_row(id: &str, level: &str, status: &str, highlight: bool) -> Row {
    Row {
        entity: Some(entity("player", id)),
        cells: vec![player_cell(id), cell(CellKind::Number, level), cell(CellKind::Badge, status)],
        highlight,
        item: None,
        actions: Vec::new(),
    }
}

fn list_column(module: &str, label: &str, align: Align, outcome: ColumnOutcome) -> ListColumn {
    ListColumn { module: module.to_string(), label: label.to_string(), align, outcome }
}

/// The players list page: sorted by level, its list columns inserted after the player
/// column, counted in players. Rows: two players, a guild and one with no entity. Two
/// `EntityListColumn`s for `player`: Rank answers p-1001 only, Guild failed.
pub(super) fn table_list_columns_case() -> PanelCase {
    let status = |value: &str, more: Vec<Cell>| Cell { more, ..cell(CellKind::Label, value) };
    let row = |entity: Option<EntityRef>, cells: Vec<Cell>| Row {
        entity,
        cells,
        highlight: false,
        item: None,
        actions: Vec::new(),
    };
    let table = DataTable {
        columns: vec![
            column("player", "Player", Align::Left),
            Column { sorted: Some(SortDir::Desc), ..column("level", "Level", Align::Right) },
            column("status", "Status", Align::Left),
        ],
        list_columns_for: Some("player".to_string()),
        list_columns_at: 1,
        item_noun: Some(Noun::new("player", "players")),
        selectable: false,
        ..match players_table(Vec::new(), Some(1234), Some("c2")) {
            Primitive::DataTable(t) => t,
            _ => unreachable!("players_table is a DataTable"),
        }
    };
    let rows = vec![
        row(
            Some(entity("player", "p-1001")),
            vec![
                player_cell("p-1001"),
                cell(CellKind::Number, "12"),
                status("active", vec![cell(CellKind::Muted, "since 2024")]),
            ],
        ),
        row(
            Some(entity("guild", "g-7")),
            vec![cell(CellKind::Text, "Night Owls"), cell(CellKind::Number, "30"), status("guild", Vec::new())],
        ),
        row(None, vec![cell(CellKind::Text, "system"), cell(CellKind::Number, "0"), status("ok", Vec::new())]),
        row(
            Some(entity("player", "p-1002")),
            vec![player_cell("p-1002"), cell(CellKind::Number, "40"), status("banned", Vec::new())],
        ),
    ];
    PanelCase {
        list_columns: vec![
            list_column(
                "ranks",
                "Rank",
                Align::Right,
                ColumnOutcome::Cells(BTreeMap::from([(
                    "p-1001".to_string(),
                    toned(CellKind::Badge, "gold", Tone::Ok),
                )])),
            ),
            list_column("guilds", "Guild", Align::Left, ColumnOutcome::Failed("connection refused".to_string())),
        ],
        ..PanelCase::new(PanelState::Ok, Some(body(Primitive::DataTable(DataTable { rows, ..table }))))
    }
}

/// A players table filtered by a Select with `us` chosen, a Bool set to `true`, and a Range,
/// a Ref and a second Select left unset.
pub(super) fn table_filter_kinds_case() -> PanelCase {
    let filter = |key: &str, label: &str, kind: FilterKind| FilterDef { key: key.to_string(), label: label.to_string(), kind };
    let table = DataTable {
        filters: vec![
            filter("region", "Region", FilterKind::Select(strings(&["eu", "us", "asia"]))),
            filter("level", "Level", FilterKind::Range),
            filter("online", "Online", FilterKind::Bool),
            filter("guild", "Guild", FilterKind::Ref("guild".to_string())),
            filter("status", "Status", FilterKind::Select(strings(&["active", "banned"]))),
        ],
        ..match players_table(vec![player_row("p-1001", "12", "active", false)], Some(1), None) {
            Primitive::DataTable(t) => t,
            _ => unreachable!("players_table is a DataTable"),
        }
    };
    PanelCase {
        query: posted(&[("region", "us"), ("online", "true")]),
        ..PanelCase::new(PanelState::Ok, Some(body(Primitive::DataTable(table))))
    }
}

/// `primitive` drawn with a label for `p-1001` and none for `p-1002`.
pub(super) fn labelled_case(mode: &'static str, primitive: Primitive) -> PanelCase {
    PanelCase {
        mode,
        labels: Labels::from([(("player".to_string(), "p-1001".to_string()), "Anna#1001".to_string())]),
        ..PanelCase::new(PanelState::Ok, Some(body(primitive)))
    }
}

/// A players table whose `p-1001` cell draws its label and whose `p-1002` cell, with no
/// label, draws its id.
pub(super) fn table_labelled_case() -> PanelCase {
    let rows = vec![player_row("p-1001", "12", "active", false), player_row("p-1002", "40", "banned", false)];
    PanelCase {
        labels: Labels::from([(("player".to_string(), "p-1001".to_string()), "Anna#1001".to_string())]),
        ..PanelCase::new(PanelState::Ok, Some(body(players_table(rows, Some(2), None))))
    }
}

/// Two players of a table counted in players, with a next page.
pub(super) fn noun_table(total: Option<u64>) -> Primitive {
    let rows = vec![player_row("p-1001", "12", "active", false), player_row("p-1002", "40", "banned", false)];
    noun_table_of(rows, total, Some("c3"))
}

pub(super) fn noun_table_of(rows: Vec<Row>, total: Option<u64>, next: Option<&str>) -> Primitive {
    match players_table(rows, total, next) {
        Primitive::DataTable(t) => Primitive::DataTable(DataTable { item_noun: Some(Noun::new("player", "players")), ..t }),
        _ => unreachable!("players_table is a DataTable"),
    }
}

pub(super) fn wrap_table(rows: Vec<Row>) -> Primitive {
    match players_table(rows, Some(1), None) {
        Primitive::DataTable(mut t) => {
            t.columns[1].wrap = true;
            t.columns[2].wrap = true;
            Primitive::DataTable(t)
        }
        _ => unreachable!("players_table is a DataTable"),
    }
}

/// The players table ordered by level, lowest first.
pub(super) fn sorted_ascending_table() -> Primitive {
    let rows = vec![player_row("p-1001", "12", "active", false), player_row("p-1002", "40", "banned", false)];
    match players_table(rows, Some(2), None) {
        Primitive::DataTable(mut t) => {
            t.columns[1].sorted = Some(SortDir::Asc);
            Primitive::DataTable(t)
        }
        _ => unreachable!("players_table is a DataTable"),
    }
}

/// `d` drawn on each `Row` of [`CONTRIBUTION`] that lists it.
fn table_row(d: ActionDecl) -> ActionDecl {
    ActionDecl { point: ActionPoint::TableRow(CONTRIBUTION.to_string()), ..d }
}

/// A player's sessions tab: a detail table on the entity page of `player:p-1001` whose rows
/// carry `TableRow` actions. The first row has no entity, so its actions bind the panel's;
/// the second opens a device, so they bind the device. A `PanelTool` beside them draws in
/// the head only.
pub(super) fn table_row_actions_case() -> PanelCase {
    let revoke = table_row(ActionDecl {
        danger: true,
        icon: String::new(),
        ..decl("revoke", "Revoke", "", Then::Confirm(confirm("Revoke session?", ConfirmTone::Danger)))
    });
    let inspect =
        table_row(ActionDecl { emphasis: Emphasis::Ghost, ..decl("inspect", "Inspect", "eye", Then::Invoke) });
    let open = table_row(decl("open", "Open", "external", Then::Link(page_link("platform", "logs"))));
    let wipe = table_row(ActionDecl { min_role: Role::Admin, ..decl("wipe", "Wipe", "trash", Then::Invoke) });
    let row = |entity: Option<EntityRef>, item: &str, actions: &[&str]| Row {
        entity,
        cells: vec![cell(CellKind::Mono, item), cell(CellKind::Time, "2026-10-21 09:12")],
        highlight: false,
        item: Some(item.to_string()),
        actions: strings(actions),
    };
    let table = DataTable {
        columns: vec![column("session", "Session", Align::Left), column("seen", "Last seen", Align::Left)],
        rows: vec![
            row(None, "sess-1", &["inspect", "revoke", "open", "wipe"]),
            row(Some(entity("device", "d-9")), "sess-2", &["inspect"]),
            row(None, "sess-3", &[]),
        ],
        page: PageInfo { next_cursor: None, size: 25, total: None },
        filters: Vec::new(),
        search_placeholder: None,
        selectable: false,
        list_columns_for: None,
        list_columns_at: 0,
        item_noun: None,
    };
    PanelCase {
        entity: Some(entity("player", "p-1001")),
        actions: vec![
            tool(&inspect),
            tool(&revoke),
            tool(&open),
            action(MODULE, &wipe, Role::Operator, true),
            tool(&decl("export", "Export", "download", Then::Invoke)),
        ],
        ..PanelCase::new(PanelState::Ok, Some(body(Primitive::DataTable(table))))
    }
}

/// A save slot's download as the mockup draws it (pages-core.js:141): an icon-only ghost
/// `TableRow` action draws its icon alone with its label as the accessible name, enabled on
/// the first row and refused as not implemented on the second.
pub(super) fn table_row_icon_only_case() -> PanelCase {
    let icon_only = |d: ActionDecl| table_row(ActionDecl { emphasis: Emphasis::Ghost, icon_only: true, ..d });
    let download = icon_only(decl("download", "Download", "download", Then::Invoke));
    let fetch = icon_only(ActionDecl { implemented: false, ..decl("fetch", "Fetch", "download", Then::Invoke) });
    let row = |slot: &str, action: &str| Row {
        entity: None,
        cells: vec![cell(CellKind::Mono, slot)],
        highlight: false,
        item: Some(slot.to_string()),
        actions: strings(&[action]),
    };
    let table = DataTable {
        columns: vec![column("slot", "Slot", Align::Left)],
        rows: vec![row("autosave", "download"), row("manual-1", "fetch")],
        page: PageInfo { next_cursor: None, size: 25, total: None },
        filters: Vec::new(),
        search_placeholder: None,
        selectable: false,
        list_columns_for: None,
        list_columns_at: 0,
        item_noun: None,
    };
    PanelCase {
        entity: Some(entity("player", "p-1001")),
        actions: vec![tool(&download), tool(&fetch)],
        ..PanelCase::new(PanelState::Ok, Some(body(Primitive::DataTable(table))))
    }
}

/// A first page of 1,234 players, 25 a page: the pager reads `"1 / 50"`.
pub(super) fn table_first_page_case() -> PanelCase {
    PanelCase::new(
        PanelState::Ok,
        Some(body(players_table(
            vec![player_row("p-1001", "12", "active", false), player_row("p-1002", "40", "banned", false)],
            Some(1234),
            Some("c2"),
        ))),
    )
}

/// `d` bound to each `QueueItem` of [`CONTRIBUTION`] that lists it.
fn queue_row(d: ActionDecl) -> ActionDecl {
    ActionDecl { point: ActionPoint::QueueRow(CONTRIBUTION.to_string()), ..d }
}

fn queue_states() -> Vec<QueueState> {
    vec![
        QueueState { key: "pending".to_string(), label: "Pending".to_string(), count: 12, tone: None },
        QueueState { key: "failed".to_string(), label: "Failed".to_string(), count: 3, tone: Some(Tone::Err) },
        QueueState { key: "sent".to_string(), label: "Sent".to_string(), count: 1204, tone: Some(Tone::Ok) },
    ]
}

pub(super) fn queue_item(id: &str, state: &str, attempts: u32, max_attempts: u32, actions: &[&str]) -> QueueItem {
    QueueItem {
        id: id.to_string(),
        entity: None,
        state: state.to_string(),
        attempts,
        max_attempts,
        next_attempt_at: None,
        last_error: None,
        payload_preview: None,
        actions: strings(actions),
        cells: Vec::new(),
    }
}

/// A queue with no extra column, toolbar, selection or page beyond the one shown.
pub(super) fn work_queue(selected: &str, items: Vec<QueueItem>) -> WorkQueue {
    WorkQueue {
        states: queue_states(),
        selected: selected.to_string(),
        items,
        item_label: "Job".to_string(),
        item_noun: Noun::new("job", "jobs"),
        columns: Vec::new(),
        page: PageInfo { next_cursor: None, size: 25, total: None },
        filters: Vec::new(),
        search_placeholder: None,
        selectable: false,
    }
}

fn retry_decl() -> ActionDecl {
    decl("retry", "Retry", "refresh", Then::Invoke)
}

fn drop_decl() -> ActionDecl {
    ActionDecl { danger: true, ..decl("drop", "Drop", "trash", Then::Confirm(confirm("Drop job?", ConfirmTone::Danger))) }
}

/// `QueueRow` actions on each item: the first non-danger one labelled, the rest ghost icons;
/// `purge`, refused to the operator's role, drawn disabled as a ghost icon.
pub(super) fn queue_items_actions_case() -> PanelCase {
    let park = decl("park", "Park", "park", Then::Invoke);
    let purge = ActionDecl { min_role: Role::Admin, ..decl("purge", "Purge", "trash", Then::Invoke) };
    PanelCase {
        actions: vec![
            tool(&queue_row(retry_decl())),
            tool(&queue_row(drop_decl())),
            tool(&queue_row(park)),
            action(MODULE, &queue_row(purge), Role::Operator, true),
        ],
        ..PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::WorkQueue(work_queue(
                "failed",
                vec![
                    QueueItem {
                        entity: Some(entity("player", "p-1001")),
                        last_error: Some("timeout after 30s".to_string()),
                        payload_preview: Some(r#"{"amount":5}"#.to_string()),
                        ..queue_item("job-81", "failed", 3, 5, &["drop", "retry"])
                    },
                    QueueItem {
                        next_attempt_at: Some("2026-10-21 10:00".to_string()),
                        ..queue_item("job-82", "sent", 2, 5, &["park", "retry", "purge"])
                    },
                ],
            )))),
        )
    }
}

/// A `PanelTool` and a `QueueRow` action on one WorkQueue panel, both listed by the item and
/// both handed over by `routes::panel_tools`: the tool draws in the panel head only, the row
/// action on the item only.
pub(super) fn queue_panel_tool_case() -> PanelCase {
    let manifest = Manifest {
        module: MODULE.to_string(),
        actions: vec![
            ActionDecl { implemented: false, ..decl("pause", "Pause sending", "pause", Then::Invoke) },
            queue_row(ActionDecl {
                danger: true,
                ..decl("cancel", "Cancel", "x", Then::Confirm(confirm("Cancel message?", ConfirmTone::Danger)))
            }),
        ],
        writes: true,
        ..Manifest::default()
    };
    PanelCase {
        actions: routes::panel_tools(&operator(Role::Admin), CSRF, MODULE, &manifest, CONTRIBUTION),
        ..PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::WorkQueue(work_queue(
                "pending",
                vec![queue_item("job-90", "pending", 0, 5, &["pause", "cancel"])],
            )))),
        )
    }
}

/// The mail outbox's table contract on a first page searched and filtered by URL: extra
/// columns with per-item cells, a selectable column, the toolbar, the foot, and attempts
/// counts below, at and above the fixed 5-square bar.
pub(super) fn queue_table_contract_case() -> PanelCase {
    let to = |email: &str, state: &str| Cell {
        link: Some(Link::Page {
            module: "mail".to_string(),
            page: "outbox".to_string(),
            params: BTreeMap::from([
                ("q".to_string(), email.to_string()),
                ("state".to_string(), state.to_string()),
            ]),
        }),
        ..cell(CellKind::User, email)
    };
    let account = |id: &str| Cell { link: Some(Link::Entity(entity("account", id))), ..cell(CellKind::Ref, id) };
    let cancel = queue_row(ActionDecl {
        danger: true,
        ..decl("cancel", "Cancel", "x", Then::Confirm(confirm("Cancel message?", ConfirmTone::Danger)))
    });
    let with_cells = |item: QueueItem, cells: Vec<Cell>| QueueItem { cells, ..item };
    PanelCase {
        query: posted(&[("q", "anna"), ("template", "welcome")]),
        search: Some("anna"),
        actions: vec![tool(&queue_row(retry_decl())), tool(&cancel)],
        ..PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::WorkQueue(WorkQueue {
                item_label: "Message".to_string(),
                item_noun: Noun::new("message", "messages"),
                columns: vec![
                    column("to", "To", Align::Left),
                    column("account", "Account", Align::Left),
                    column("template", "Template", Align::Left),
                    column("size", "Size", Align::Right),
                ],
                page: PageInfo { next_cursor: Some("c2".to_string()), size: 25, total: Some(309) },
                filters: vec![FilterDef {
                    key: "template".to_string(),
                    label: "Template".to_string(),
                    kind: FilterKind::Text,
                }],
                search_placeholder: Some("Recipient, template or message id".to_string()),
                selectable: true,
                ..work_queue(
                    "pending",
                    vec![
                        with_cells(
                            queue_item("msg-1001", "pending", 1, 2, &["cancel"]),
                            vec![
                                to("anna@example.com", "pending"),
                                account("a-17"),
                                cell(CellKind::Text, "welcome"),
                                cell(CellKind::Bytes, "2 KB"),
                            ],
                        ),
                        with_cells(
                            QueueItem {
                                last_error: Some("421 throttled".to_string()),
                                ..queue_item("msg-1002", "failed", 7, 8, &["retry", "cancel"])
                            },
                            vec![
                                to("anna.k@example.com", "failed"),
                                account("a-18"),
                                cell(CellKind::Text, "welcome"),
                                cell(CellKind::Bytes, "3 KB"),
                            ],
                        ),
                        with_cells(
                            queue_item("msg-1003", "sent", 1, 1, &[]),
                            vec![
                                to("annette@example.com", "sent"),
                                account("a-19"),
                                cell(CellKind::Text, "welcome"),
                                cell(CellKind::Bytes, "1 KB"),
                            ],
                        ),
                    ],
                )
            }))),
        )
    }
}

/// One queue column whose long values wrap.
pub(super) fn queue_wrap_column_case() -> PanelCase {
    let item = QueueItem {
        cells: vec![cell(CellKind::Text, "a long reason that wraps onto the next line")],
        ..queue_item("job-91", "pending", 0, 5, &[])
    };
    PanelCase::new(
        PanelState::Ok,
        Some(body(Primitive::WorkQueue(WorkQueue {
            columns: vec![Column { wrap: true, ..column("reason", "Reason", Align::Left) }],
            ..work_queue("pending", vec![item])
        }))),
    )
}

/// A page past the first: no `"1 / N"`, no next page, the count against the state's total.
pub(super) fn queue_later_page_case() -> PanelCase {
    PanelCase {
        query: posted(&[("cursor", "c2")]),
        ..PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::WorkQueue(WorkQueue {
                page: PageInfo { next_cursor: None, size: 25, total: Some(27) },
                ..work_queue(
                    "pending",
                    vec![queue_item("job-26", "pending", 0, 5, &[]), queue_item("job-27", "pending", 0, 5, &[])],
                )
            }))),
        )
    }
}

/// A queue with a search box and a filter, and no item: under `query` and `search` it was
/// narrowed to nothing, or, unfiltered, it is empty in the selected state.
pub(super) fn queue_narrowed_case(query: &[(&str, &str)], search: Option<&'static str>) -> PanelCase {
    PanelCase {
        query: posted(query),
        search,
        ..PanelCase::new(
            PanelState::Empty,
            Some(body(Primitive::WorkQueue(WorkQueue {
                filters: vec![FilterDef {
                    key: "template".to_string(),
                    label: "Template".to_string(),
                    kind: FilterKind::Text,
                }],
                search_placeholder: Some("Recipient, template or message id".to_string()),
                ..work_queue("pending", Vec::new())
            }))),
        )
    }
}
