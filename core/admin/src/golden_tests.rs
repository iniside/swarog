//! Golden renders of the portal templates: every primitive macro, the panel chrome in each
//! `PanelState`, the slot placeholders, the modals, the row menu and the search palette, each
//! from a view model built in code and rendered through the production `render::panel` /
//! `render::modal` / `render::fragment` path.
//!
//! Each case is compared with `src/testdata/<name>.html`. Without `ADMIN_BLESS=1` a diff, a
//! missing file, or a `.html` file no case produces fails the run.
//! `ADMIN_BLESS=1 cargo test -p admin golden` rewrites the set and deletes the orphans.
//!
//! Inputs are fixed: one csrf, one form id, fixed `age_s`, and no clock or id generator,
//! because the render layer reads neither.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use controlplane::{
    ActionDecl, ActionPoint, Align, AttentionItem, Axis, Banner, Cell, CellKind, Column, ConfirmSpec, ConfirmTone,
    DataTable, Delta, Emphasis, EntityHeader, EntityRef, Field, FieldKind, FilterDef, FilterKind, FormSpec, KpiTile,
    Link, PageInfo, Palette, PanelBody, Primitive, PropertyList, QueueItem, QueueState, ReasonReq, Role, Row, Series,
    ServiceItem, ServiceState, ServiceStatus, Severity, Then, TimeSeriesChart, Timeline, TimelineEntry, Tone,
    WorkQueue,
};
use minijinja::Environment;

use crate::engine::actions;
use crate::engine::panels::{LastValue, PanelState};
use crate::engine::roles::Operator;
use crate::render::{
    self, ActionItem, ActionResultView, FlashView, FormInstance, MenuView, ModalSpec, PanelInput, RevealView,
    SearchGroupView, SearchHitView, SearchView, SlotView,
};

const CSRF: &str = "csrf-golden";
const MODULE: &str = "golden";
const CONTRIBUTION: &str = "panel";
const FORM_ID: &str = "form-golden-1";
/// A render's own newline in the golden form; see [`golden_form`].
const RAW_NEWLINE: char = '↵';
/// Unchanged lines shown around a difference.
const DIFF_CONTEXT: usize = 2;
/// Lines shown per side of a difference before the rest is counted.
const DIFF_MAX_LINES: usize = 12;

// ---- harness ------------------------------------------------------------------

fn testdata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("testdata")
}

fn blessing() -> bool {
    std::env::var("ADMIN_BLESS").is_ok_and(|v| v == "1")
}

/// The golden form of a render: a line break between two tags that touch, so a change diffs
/// as the lines it touches. A newline the render holds itself becomes [`RAW_NEWLINE`] plus a
/// break, so two renders that differ only there still differ here.
fn golden_form(name: &str, raw: &str) -> String {
    assert!(!raw.contains(RAW_NEWLINE), "golden case {name}: the render holds {RAW_NEWLINE:?}, the newline marker");
    let mut out = raw.replace('\n', &format!("{RAW_NEWLINE}\n")).replace("><", ">\n<");
    out.push('\n');
    out
}

/// A unified diff of the first run of differing lines only, with [`DIFF_CONTEXT`] lines
/// around it and each side capped at [`DIFF_MAX_LINES`].
fn compact_diff(want: &str, got: &str) -> String {
    let w: Vec<&str> = want.split('\n').collect();
    let g: Vec<&str> = got.split('\n').collect();
    let prefix = w.iter().zip(&g).take_while(|(a, b)| a == b).count();
    let suffix = w[prefix..].iter().rev().zip(g[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
    let (w_end, g_end) = (w.len() - suffix, g.len() - suffix);
    let start = prefix.saturating_sub(DIFF_CONTEXT);
    let after = (w_end + DIFF_CONTEXT).min(w.len());
    let mut out = format!(
        "--- golden\n+++ render\n@@ -{},{} +{},{} @@\n",
        start + 1,
        after - start,
        start + 1,
        (prefix - start) + (g_end - prefix) + (after - w_end)
    );
    let mut side = |sign: char, lines: &[&str]| {
        for line in lines.iter().take(DIFF_MAX_LINES) {
            out.push_str(&format!("{sign}{line}\n"));
        }
        if lines.len() > DIFF_MAX_LINES {
            out.push_str(&format!("{sign}… {} more lines\n", lines.len() - DIFF_MAX_LINES));
        }
    };
    side(' ', &w[start..prefix]);
    side('-', &w[prefix..w_end]);
    side('+', &g[prefix..g_end]);
    side(' ', &w[w_end..after]);
    out
}

/// Compares every case with `<dir>/<name>.html`, or with `bless` writes the differing and
/// missing ones and deletes every `.html` no case produces. Returns one message per failure.
fn check(dir: &Path, cases: &[(String, String)], bless: bool) -> Vec<String> {
    let mut failures = Vec::new();
    if bless {
        fs::create_dir_all(dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    }
    let write = |path: &Path, golden: &str| {
        fs::write(path, golden).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        eprintln!("golden: blessed {}", path.display());
    };
    for (name, golden) in cases {
        let path = dir.join(format!("{name}.html"));
        match fs::read_to_string(&path) {
            Ok(want) if want == *golden => {}
            Ok(_) | Err(_) if bless => write(&path, golden),
            Ok(want) => failures.push(format!(
                "{name}: render differs from {}\n{}",
                path.display(),
                compact_diff(&want, golden)
            )),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                failures.push(format!("{name}: missing golden file {}", path.display()))
            }
            Err(e) => failures.push(format!("{name}: read {}: {e}", path.display())),
        }
    }
    let produced: BTreeSet<String> = cases.iter().map(|(name, _)| format!("{name}.html")).collect();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return failures,
        Err(e) => {
            failures.push(format!("read {}: {e}", dir.display()));
            return failures;
        }
    };
    let mut orphans: Vec<PathBuf> = entries
        .map(|entry| entry.unwrap_or_else(|e| panic!("read {}: {e}", dir.display())).path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "html"))
        .filter(|path| !path.file_name().is_some_and(|n| produced.contains(&*n.to_string_lossy())))
        .collect();
    orphans.sort();
    for path in orphans {
        if bless {
            fs::remove_file(&path).unwrap_or_else(|e| panic!("delete {}: {e}", path.display()));
            eprintln!("golden: deleted orphan {}", path.display());
        } else {
            failures.push(format!("orphan golden file {}: no case produces it", path.display()));
        }
    }
    failures
}

#[test]
fn golden_renders_match_testdata() {
    let env = render::env().expect("templates compile");
    let first = cases(&env);
    assert_eq!(first, cases(&env), "two renders of the same view models differ");
    let mut seen = BTreeSet::new();
    for (name, _) in &first {
        assert!(
            !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "golden case name {name:?} is not [a-z0-9_]+"
        );
        assert!(seen.insert(name.clone()), "golden case {name} is defined twice");
    }
    let dir = testdata_dir();
    if blessing() {
        let failures = check(&dir, &first, true);
        assert!(failures.is_empty(), "blessing failed:\n{}", failures.join("\n"));
    }
    let failures = check(&dir, &first, false);
    assert!(
        failures.is_empty(),
        "{} golden failure(s); run `ADMIN_BLESS=1 cargo test -p admin golden` to accept an intended change:\n\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The gate itself, on a scratch directory: a diff, a missing file and an orphan each fail,
/// a file that is not `.html` is ignored, and a bless restores the set.
#[test]
fn golden_gate_fails_on_diff_missing_and_orphan() {
    let dir = std::env::temp_dir().join(format!("admin-golden-gate-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let cases = vec![("alpha".to_string(), "<a>\n<b>\n".to_string()), ("beta".to_string(), "<x>\n".to_string())];
    assert!(check(&dir, &cases, false).iter().all(|f| f.contains("missing golden file")));
    assert_eq!(check(&dir, &cases, false).len(), 2);
    assert!(check(&dir, &cases, true).is_empty());
    assert!(check(&dir, &cases, false).is_empty());

    fs::write(dir.join("alpha.html"), "<a>\n<B>\n").unwrap();
    let failures = check(&dir, &cases, false);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].starts_with("alpha: render differs"), "{failures:?}");
    assert!(failures[0].contains("\n-<B>\n+<b>\n"), "{failures:?}");

    fs::remove_file(dir.join("beta.html")).unwrap();
    fs::write(dir.join("gamma.html"), "<g>\n").unwrap();
    fs::write(dir.join("notes.txt"), "not a golden\n").unwrap();
    let failures = check(&dir, &cases, false);
    assert_eq!(failures.len(), 3, "{failures:?}");
    assert!(failures[0].starts_with("alpha: render differs"), "{failures:?}");
    assert!(failures[1].starts_with("beta: missing golden file"), "{failures:?}");
    assert!(failures[2].starts_with("orphan golden file") && failures[2].contains("gamma.html"), "{failures:?}");

    assert!(check(&dir, &cases, true).is_empty());
    assert!(check(&dir, &cases, false).is_empty());
    assert!(!dir.join("gamma.html").exists());
    assert!(dir.join("notes.txt").exists());
    assert_eq!(fs::read_to_string(dir.join("alpha.html")).unwrap(), "<a>\n<b>\n");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn golden_form_is_injective_over_render_newlines() {
    assert_ne!(golden_form("t", "<a><b>"), golden_form("t", "<a>\n<b>"));
    assert_eq!(golden_form("t", "<a><b>x</b>"), "<a>\n<b>x</b>\n");
}

#[test]
fn golden_diff_shows_only_the_first_differing_run() {
    let want: String = (0..40).map(|i| format!("<l{i}>\n")).collect();
    let got = want.replace("<l20>", "<L20>");
    let diff = compact_diff(&want, &got);
    assert_eq!(diff, "--- golden\n+++ render\n@@ -19,5 +19,5 @@\n <l18>\n <l19>\n-<l20>\n+<L20>\n <l21>\n <l22>\n");
}

// ---- view-model builders -----------------------------------------------------------

fn cell(kind: CellKind, value: &str) -> Cell {
    Cell { kind, value: value.to_string(), tone: None, link: None, bar: None }
}

fn toned(kind: CellKind, value: &str, tone: Tone) -> Cell {
    Cell { tone: Some(tone), ..cell(kind, value) }
}

fn bar(value: &str, pct: u8, tone: Option<Tone>) -> Cell {
    Cell { bar: Some(pct), tone, ..cell(CellKind::Mono, value) }
}

fn entity(kind: &str, id: &str) -> EntityRef {
    EntityRef { kind: kind.to_string(), id: id.to_string() }
}

fn column(key: &str, label: &str, align: Align) -> Column {
    Column { key: key.to_string(), label: label.to_string(), kind: CellKind::Text, sortable: false, align }
}

fn page_link(module: &str, page: &str) -> Link {
    Link::Page { module: module.to_string(), page: page.to_string(), params: BTreeMap::new() }
}

fn props(items: Vec<(&str, Cell)>) -> PropertyList {
    PropertyList {
        items: items.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        columns: 1,
        item: None,
        code: None,
    }
}

fn summary() -> Primitive {
    Primitive::PropertyList(props(vec![
        ("Version", cell(CellKind::Mono, "1.4.2")),
        ("State", cell(CellKind::Badge, "running")),
        ("Region", cell(CellKind::Text, "eu-west")),
    ]))
}

fn body(primitive: Primitive) -> PanelBody {
    PanelBody::new(primitive)
}

fn decl(id: &str, label: &str, icon: &str, then: Then) -> ActionDecl {
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
        when_empty: false,
        prepare: false,
    }
}

fn confirm(title: &str, tone: ConfirmTone) -> ConfirmSpec {
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

/// The view item of `d` for an operator of `role` on a module that `writes`. It mirrors
/// `routes::action_item`, which is private to `routes`: admission by `actions::admit`, the
/// csrf in `vals`, and the action URL.
fn action(module: &str, d: &ActionDecl, role: Role, writes: bool) -> ActionItem {
    let operator = Operator { login: "golden".to_string(), role };
    let (enabled, hint) = match actions::admit(&operator, d, writes) {
        Ok(()) => (true, None),
        Err(refusal) => (false, Some(refusal.hint())),
    };
    ActionItem {
        module: module.to_string(),
        id: d.id.clone(),
        label: d.label.clone(),
        icon: d.icon.clone(),
        danger: d.danger,
        emphasis: render::emphasis_name(d.emphasis),
        trailing_icon: d.trailing_icon,
        when_empty: d.when_empty,
        url: format!("/admin/_action/{}/{}", render::encode(module), render::encode(&d.id)),
        enabled,
        hint,
        then: render::then_name(&d.then),
        href: match &d.then {
            Then::Link(link) => Some(render::link_url(link)),
            _ => None,
        },
        vals: BTreeMap::from([(actions::FIELD_CSRF.to_string(), CSRF.to_string())]),
    }
}

fn tool(d: &ActionDecl) -> ActionItem {
    action(MODULE, d, Role::Admin, true)
}

/// A panel form instance's inputs; the post URL and form id are fixed.
struct FormCase {
    posted: Option<Vec<(String, String)>>,
    errors: BTreeMap<String, String>,
    banner: Option<String>,
    locked: bool,
}

impl FormCase {
    fn fresh() -> FormCase {
        FormCase { posted: None, errors: BTreeMap::new(), banner: None, locked: false }
    }
}

/// One `render::panel` input: the fields `routes::panel_fragment` fills from a resolved
/// `PanelView`, owned here.
struct PanelCase {
    title: &'static str,
    icon: &'static str,
    mode: &'static str,
    state: PanelState,
    query: Vec<(String, String)>,
    entity: Option<EntityRef>,
    search: Option<&'static str>,
    error: Option<&'static str>,
    age_s: Option<u64>,
    last_value: Option<LastValue>,
    min_role: &'static str,
    fallback_title: &'static str,
    body: Option<PanelBody>,
    actions: Vec<ActionItem>,
    row_menu_kinds: Vec<String>,
    form: Option<FormCase>,
}

impl PanelCase {
    fn new(state: PanelState, body: Option<PanelBody>) -> PanelCase {
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
            body,
            actions: Vec::new(),
            row_menu_kinds: Vec::new(),
            form: None,
        }
    }

    fn tile(state: PanelState, body: Option<PanelBody>) -> PanelCase {
        PanelCase { title: "Players online", icon: "users", mode: "tile", ..PanelCase::new(state, body) }
    }

    fn render(&self, env: &Environment<'_>) -> Result<String, minijinja::Error> {
        let path = format!("/admin/_panel/{MODULE}/{CONTRIBUTION}");
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
            body: self.body.as_ref(),
            actions: &self.actions,
            row_menu_kinds: &self.row_menu_kinds,
            csrf: CSRF,
            form: self.form.as_ref().map(|f| FormInstance {
                post_url: format!("/admin/_action/{MODULE}/save"),
                form_id: FORM_ID.to_string(),
                posted: f.posted.as_deref(),
                errors: &f.errors,
                banner: f.banner.clone(),
                locked: f.locked,
            }),
        };
        render::fragment(env, "primitives.html", "panel_fragment", &render::panel(&input))
    }
}

fn slot(mode: &'static str, denied: bool) -> SlotView {
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

fn kpi(delta: Option<Delta>) -> KpiTile {
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

fn delta(value: &str, tone: Option<Tone>) -> Option<Delta> {
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

fn services(count: usize, columns: u8, value_label: Option<&str>) -> Primitive {
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

fn field(key: &str, label: &str, kind: FieldKind, default: Option<&str>) -> Field {
    Field {
        key: key.to_string(),
        label: label.to_string(),
        kind,
        required: false,
        help: None,
        default: default.map(str::to_string),
        readonly: false,
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

/// One field of every `FieldKind`.
fn profile_form() -> FormSpec {
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
        ],
        hidden: BTreeMap::new(),
        submit_label: "Save profile".to_string(),
        note: Some("Changes apply on next login.".to_string()),
        revision: Some(3),
    }
}

fn posted(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

// ---- cases ---------------------------------------------------------------------

/// Every case as `(name, golden form)`, in a fixed order.
fn cases(env: &Environment<'_>) -> Vec<(String, String)> {
    let mut out: Vec<(&'static str, Result<String, minijinja::Error>)> = Vec::new();
    let mut panel = |name: &'static str, case: PanelCase| out.push((name, case.render(env)));

    // ---- each PanelState, in panel chrome ----
    panel("state_ok", PanelCase::new(PanelState::Ok, Some(body(summary()))));
    panel("state_empty", PanelCase::new(PanelState::Empty, None));
    panel(
        "state_empty_payload",
        PanelCase::new(
            PanelState::Empty,
            Some(body(Primitive::Empty {
                title: "No grants yet".to_string(),
                detail: "Grants appear here once issued.".to_string(),
            })),
        ),
    );
    let readonly_tool = decl("restart", "Restart", "refresh", Then::Invoke);
    panel(
        "state_readonly",
        PanelCase {
            actions: vec![action(MODULE, &readonly_tool, Role::Admin, false)],
            ..PanelCase::new(
                PanelState::Readonly,
                Some(PanelBody {
                    banner: Some(Banner { tone: Tone::Info, text: "Managed by the deploy pipeline.".to_string() }),
                    ..body(summary())
                }),
            )
        },
    );
    panel(
        "state_stale_hides_module_banner",
        PanelCase {
            error: Some("deadline of 2000ms elapsed"),
            age_s: Some(185),
            actions: vec![tool(&decl("restart", "Restart", "refresh", Then::Invoke))],
            ..PanelCase::new(
                PanelState::Stale,
                Some(PanelBody {
                    banner: Some(Banner { tone: Tone::Warn, text: "This banner shows only when fresh.".to_string() }),
                    ..body(summary())
                }),
            )
        },
    );
    panel(
        "state_error",
        PanelCase {
            error: Some("connection refused"),
            actions: vec![tool(&decl("restart", "Restart", "refresh", Then::Invoke))],
            ..PanelCase::new(PanelState::Error, None)
        },
    );
    panel("state_error_no_detail", PanelCase::new(PanelState::Error, None));
    panel("state_denied", PanelCase { min_role: "admin", ..PanelCase::new(PanelState::Denied, None) });

    // ---- KPI tiles: delta tones, then each state ----
    let tile = |d: Option<Delta>| PanelCase::tile(PanelState::Ok, Some(body(Primitive::KpiTile(kpi(d)))));
    panel("tile_delta_ok_up", tile(delta("+4.2%", Some(Tone::Ok))));
    panel(
        "tile_delta_err_down",
        PanelCase::tile(
            PanelState::Ok,
            Some(body(Primitive::KpiTile(KpiTile {
                unit: Some("ms".to_string()),
                spark: Palette::Chart2,
                link: Some(page_link("platform", "latency")),
                ..kpi(delta("+18 ms", Some(Tone::Err)))
            }))),
        ),
    );
    panel(
        "tile_delta_warn_bad_up_capacity",
        PanelCase::tile(
            PanelState::Ok,
            Some(body(Primitive::KpiTile(KpiTile {
                spark: Palette::Warn,
                capacity: Some((62.0, 100.0)),
                ..kpi(delta("+9", Some(Tone::Warn)))
            }))),
        ),
    );
    panel("tile_delta_info_flat", tile(delta("±0", Some(Tone::Info))));
    panel(
        "tile_delta_none_period_only",
        PanelCase::tile(
            PanelState::Ok,
            Some(body(Primitive::KpiTile(KpiTile {
                delta: Some(Delta { value: String::new(), period: "last 24 h".to_string(), tone: None }),
                series: vec![4.0],
                ..kpi(None)
            }))),
        ),
    );
    panel(
        "tile_stale",
        PanelCase {
            age_s: Some(185),
            error: Some("deadline of 2000ms elapsed"),
            ..PanelCase::tile(PanelState::Stale, Some(body(Primitive::KpiTile(kpi(delta("+4.2%", Some(Tone::Ok)))))))
        },
    );
    panel(
        "tile_error_last_value",
        PanelCase {
            error: Some("connection refused"),
            last_value: Some(LastValue { value: "12,408 players".to_string(), age_s: 420 }),
            ..PanelCase::tile(PanelState::Error, None)
        },
    );
    panel("tile_error", PanelCase { error: Some("connection refused"), ..PanelCase::tile(PanelState::Error, None) });
    panel("tile_empty", PanelCase::tile(PanelState::Empty, None));
    panel("tile_denied", PanelCase { min_role: "operator", ..PanelCase::tile(PanelState::Denied, None) });

    // ---- panel chrome: heading, meta, note, module banner in every tone ----
    panel(
        "chrome_heading_meta_note",
        PanelCase::new(
            PanelState::Ok,
            Some(PanelBody {
                heading: Some("eu-west-1".to_string()),
                meta: Some("3 nodes".to_string()),
                note: Some("Sampled every 30 s".to_string()),
                ..body(summary())
            }),
        ),
    );
    for (name, tone) in [
        ("chrome_banner_ok", Tone::Ok),
        ("chrome_banner_warn", Tone::Warn),
        ("chrome_banner_err", Tone::Err),
        ("chrome_banner_info", Tone::Info),
    ] {
        panel(
            name,
            PanelCase::new(
                PanelState::Ok,
                Some(PanelBody {
                    banner: Some(Banner { tone, text: "Module banner text.".to_string() }),
                    ..body(summary())
                }),
            ),
        );
    }

    // ---- tools: emphasis, trailing icon, danger, disabled, when_empty ----
    let primary = ActionDecl { emphasis: Emphasis::Primary, ..decl("deploy", "Deploy", "up", Then::Invoke) };
    let default = decl("edit", "Edit", "edit", Then::Form(profile_form()));
    let ghost = ActionDecl {
        emphasis: Emphasis::Ghost,
        ..decl("pause", "Pause", "pause", Then::Confirm(confirm("Pause?", ConfirmTone::Warn)))
    };
    let trailing = ActionDecl {
        emphasis: Emphasis::Ghost,
        trailing_icon: true,
        ..decl("logs", "Open logs", "external", Then::Link(page_link("platform", "logs")))
    };
    let danger = ActionDecl {
        danger: true,
        emphasis: Emphasis::Primary,
        ..decl("drain", "Drain", "trash", Then::Confirm(confirm("Drain?", ConfirmTone::Danger)))
    };
    let denied = ActionDecl { min_role: Role::Admin, ..decl("wipe", "Wipe", "trash", Then::Invoke) };
    let unimplemented = ActionDecl { implemented: false, ..decl("export", "Export", "download", Then::Invoke) };
    panel(
        "tools_emphasis",
        PanelCase {
            entity: Some(entity("server", "eu-1")),
            actions: vec![
                tool(&primary),
                tool(&default),
                tool(&ghost),
                tool(&trailing),
                tool(&danger),
                action(MODULE, &denied, Role::Operator, true),
                tool(&unimplemented),
            ],
            ..PanelCase::new(PanelState::Ok, Some(body(summary())))
        },
    );
    let create = ActionDecl {
        when_empty: true,
        emphasis: Emphasis::Primary,
        ..decl("create", "Create key", "plus", Then::Invoke)
    };
    panel(
        "tools_when_empty",
        PanelCase { actions: vec![tool(&create), tool(&default)], ..PanelCase::new(PanelState::Empty, None) },
    );
    panel(
        "tools_when_empty_payload",
        PanelCase {
            actions: vec![tool(&create), tool(&default)],
            ..PanelCase::new(
                PanelState::Empty,
                Some(body(Primitive::Empty {
                    title: "No keys".to_string(),
                    detail: "Create the first one.".to_string(),
                })),
            )
        },
    );

    // ---- PropertyList and cells ----
    let rotate = ActionDecl {
        danger: true,
        ..decl("rotate", "Rotate", "refresh", Then::Confirm(confirm("Rotate key?", ConfirmTone::Danger)))
    };
    let copy = decl("copy", "Copy", "copy", Then::Invoke);
    panel(
        "props_code_footer_tools",
        PanelCase {
            entity: Some(entity("apikey", "k-7")),
            actions: vec![tool(&rotate), tool(&copy)],
            ..PanelCase::new(
                PanelState::Ok,
                Some(PanelBody {
                    note: Some("Rotated 3 days ago".to_string()),
                    ..body(Primitive::PropertyList(PropertyList {
                        item: Some("key-7".to_string()),
                        code: Some("curl -H 'X-Api-Key: gb_live_…' https://api.example/v1/ping".to_string()),
                        ..props(vec![("Scope", cell(CellKind::Mono, "read"))])
                    }))
                }),
            )
        },
    );
    panel(
        "props_item_head_tools",
        PanelCase {
            actions: vec![tool(&copy)],
            ..PanelCase::new(
                PanelState::Ok,
                Some(PanelBody {
                    note: Some("Note in the head".to_string()),
                    ..body(Primitive::PropertyList(PropertyList {
                        item: Some("key-7".to_string()),
                        ..props(vec![("Scope", cell(CellKind::Mono, "read"))])
                    }))
                }),
            )
        },
    );
    panel(
        "props_two_columns",
        PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::PropertyList(PropertyList {
                columns: 2,
                ..props(vec![
                    ("Version", cell(CellKind::Mono, "1.4.2")),
                    ("Region", cell(CellKind::Text, "eu-west")),
                    ("Nodes", cell(CellKind::Number, "3")),
                    ("Disk", cell(CellKind::Bytes, "1.2 GB")),
                ])
            }))),
        ),
    );
    panel(
        "cell_kinds",
        PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::PropertyList(props(vec![
                ("Text", cell(CellKind::Text, "plain")),
                ("Text err", toned(CellKind::Text, "failing", Tone::Err)),
                ("Mono", cell(CellKind::Mono, "a1b2c3")),
                ("Mono warn", toned(CellKind::Mono, "slow", Tone::Warn)),
                ("Badge derived", cell(CellKind::Badge, "failed")),
                ("Badge unknown", cell(CellKind::Badge, "mystery")),
                ("Badge toned", toned(CellKind::Badge, "pending", Tone::Ok)),
                (
                    "Ref",
                    Cell { link: Some(Link::Entity(entity("item", "sword-7"))), ..cell(CellKind::Ref, "item:sword-7") },
                ),
                ("Number", cell(CellKind::Number, "1,204")),
                ("Bytes", cell(CellKind::Bytes, "512 KB")),
                ("Time", cell(CellKind::Time, "2026-10-21 09:12")),
                ("Time info", toned(CellKind::Time, "in 5 min", Tone::Info)),
                (
                    "User",
                    Cell {
                        link: Some(Link::Entity(entity("player", "p-1001"))),
                        ..cell(CellKind::User, "anna.kowalska")
                    },
                ),
            ])))),
        ),
    );
    panel(
        "cell_bar",
        PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::PropertyList(props(vec![
                ("Default", bar("40%", 40, None)),
                ("Ok tone", bar("30%", 30, Some(Tone::Ok))),
                ("Warn", bar("75%", 75, Some(Tone::Warn))),
                ("Err", bar("100%", 100, Some(Tone::Err))),
                ("Clamped", bar("140%", 140, Some(Tone::Info))),
            ])))),
        ),
    );

    // ---- ServiceStatus: one and two columns, with and without value_label ----
    panel("services_one_column", PanelCase::new(PanelState::Ok, Some(body(services(3, 1, None)))));
    panel("services_one_column_value_label", PanelCase::new(PanelState::Ok, Some(body(services(3, 1, Some("p95"))))));
    panel("services_two_columns", PanelCase::new(PanelState::Ok, Some(body(services(5, 2, None)))));
    panel("services_two_columns_value_label", PanelCase::new(PanelState::Ok, Some(body(services(5, 2, Some("lag"))))));

    // ---- DataTable ----
    let player_cell = |id: &str| Cell { link: Some(Link::Entity(entity("player", id))), ..cell(CellKind::User, id) };
    let table = |rows: Vec<Row>, total: Option<u64>, next: Option<&str>| {
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
        })
    };
    let row = |id: &str, level: &str, status: &str, highlight: bool| Row {
        entity: Some(entity("player", id)),
        cells: vec![player_cell(id), cell(CellKind::Number, level), cell(CellKind::Badge, status)],
        highlight,
    };
    panel(
        "table_highlight_row_menu",
        PanelCase {
            query: posted(&[("status", "active"), ("cursor", "c1")]),
            row_menu_kinds: strings(&["player"]),
            ..PanelCase::new(
                PanelState::Ok,
                Some(body(table(
                    vec![
                        row("p-1001", "12", "active", false),
                        row("p-1002", "40", "banned", true),
                        Row {
                            entity: None,
                            cells: vec![
                                cell(CellKind::Text, "system"),
                                cell(CellKind::Number, "0"),
                                cell(CellKind::Badge, "ok"),
                            ],
                            highlight: false,
                        },
                    ],
                    Some(1234),
                    Some("c2"),
                ))),
            )
        },
    );
    panel(
        "table_empty_search",
        PanelCase {
            query: posted(&[("q", "zed")]),
            search: Some("zed"),
            ..PanelCase::new(PanelState::Empty, Some(body(table(Vec::new(), Some(0), None))))
        },
    );

    // ---- Timeline ----
    let entry = |at: &str, actor: Option<EntityRef>, verb: &str, summary: &str| TimelineEntry {
        at: at.to_string(),
        actor,
        verb: verb.to_string(),
        target: None,
        summary: summary.to_string(),
        detail: None,
        icon: String::new(),
        tone: None,
    };
    panel(
        "timeline_two_days_older",
        PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::Timeline(Timeline {
                entries: vec![
                    TimelineEntry {
                        target: Some(entity("item", "sword-7")),
                        detail: Some("batch 42".to_string()),
                        icon: "gift".to_string(),
                        tone: Some(Tone::Ok),
                        ..entry("2026-10-21T09:12:00Z", Some(entity("player", "p-1001")), "grant", "granted 1 item")
                    },
                    entry("not-a-time", Some(entity("user", "ops.anna")), "", "imported legacy entry"),
                    entry("2026-10-21T08:03:00Z", None, "", "nightly compaction finished"),
                    TimelineEntry {
                        detail: Some("attempt 2 of 5".to_string()),
                        icon: "refresh".to_string(),
                        tone: Some(Tone::Warn),
                        ..entry("2026-10-20T23:59:00Z", Some(entity("svc", "billing")), "retry", "webhook retried")
                    },
                ],
                cursor_before: Some("2026-10-20T23:59:00Z".to_string()),
                live: false,
            }))),
        ),
    );
    let no_entries = Timeline { entries: Vec::new(), cursor_before: None, live: false };
    panel("timeline_empty", PanelCase::new(PanelState::Empty, Some(body(Primitive::Timeline(no_entries)))));

    // ---- WorkQueue ----
    let retry = decl("retry", "Retry", "refresh", Then::Invoke);
    let drop_job = ActionDecl {
        danger: true,
        ..decl("drop", "Drop", "trash", Then::Confirm(confirm("Drop job?", ConfirmTone::Danger)))
    };
    let park = decl("park", "Park", "park", Then::Invoke);
    let queue_states = vec![
        QueueState { key: "pending".to_string(), label: "Pending".to_string(), count: 12, tone: None },
        QueueState { key: "failed".to_string(), label: "Failed".to_string(), count: 3, tone: Some(Tone::Err) },
        QueueState { key: "sent".to_string(), label: "Sent".to_string(), count: 1204, tone: Some(Tone::Ok) },
    ];
    panel(
        "queue_items_actions",
        PanelCase {
            actions: vec![tool(&retry), tool(&drop_job), tool(&park)],
            ..PanelCase::new(
                PanelState::Ok,
                Some(body(Primitive::WorkQueue(WorkQueue {
                    states: queue_states.clone(),
                    selected: "failed".to_string(),
                    items: vec![
                        QueueItem {
                            id: "job-81".to_string(),
                            entity: Some(entity("player", "p-1001")),
                            state: "failed".to_string(),
                            attempts: 3,
                            max_attempts: 5,
                            next_attempt_at: None,
                            last_error: Some("timeout after 30s".to_string()),
                            payload_preview: Some(r#"{"amount":5}"#.to_string()),
                            actions: strings(&["drop", "retry"]),
                        },
                        QueueItem {
                            id: "job-82".to_string(),
                            entity: None,
                            state: "sent".to_string(),
                            attempts: 2,
                            max_attempts: 5,
                            next_attempt_at: Some("2026-10-21 10:00".to_string()),
                            last_error: None,
                            payload_preview: None,
                            actions: strings(&["park", "retry"]),
                        },
                    ],
                }))),
            )
        },
    );
    panel(
        "queue_empty",
        PanelCase::new(
            PanelState::Empty,
            Some(body(Primitive::WorkQueue(WorkQueue {
                states: queue_states,
                selected: "pending".to_string(),
                items: Vec::new(),
            }))),
        ),
    );

    // ---- TimeSeriesChart ----
    panel(
        "chart_two_axes",
        PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::TimeSeriesChart(TimeSeriesChart {
                buckets: strings(&["00:00", "06:00", "12:00", "18:00"]),
                series: vec![
                    Series {
                        label: "Requests".to_string(),
                        unit: "rps".to_string(),
                        values: vec![120.0, 340.0, 280.0, 410.0],
                        axis: Axis::Left,
                    },
                    Series {
                        label: "p95".to_string(),
                        unit: "ms".to_string(),
                        values: vec![35.5, 42.0, 38.0, 51.25],
                        axis: Axis::Right,
                    },
                ],
            }))),
        ),
    );

    // ---- the other draw modes: attention, header, fact, count, nav, inline ----
    let attention = Primitive::AttentionItems(vec![
        AttentionItem {
            severity: Severity::Err,
            title: "billing-svc is down".to_string(),
            text: "No heartbeat for 4 min".to_string(),
            link: page_link("platform", "services"),
            action_label: "Open".to_string(),
            since: "4 min".to_string(),
        },
        AttentionItem {
            severity: Severity::Warn,
            title: "3 failed jobs".to_string(),
            text: "Webhook deliveries are retrying".to_string(),
            link: Link::Entity(entity("queue", "webhooks")),
            action_label: "Review".to_string(),
            since: "12 min".to_string(),
        },
    ]);
    let in_mode = |mode: &'static str, state: PanelState, primitive: Option<Primitive>| PanelCase {
        mode,
        error: (state == PanelState::Error).then_some("connection refused"),
        ..PanelCase::new(state, primitive.map(body))
    };
    panel("attention_ok", PanelCase { icon: "", ..in_mode("attention", PanelState::Ok, Some(attention)) });
    panel("attention_error", in_mode("attention", PanelState::Error, None));
    let header = EntityHeader {
        title: "Anna Kowalska".to_string(),
        subtitle: "player since 2024".to_string(),
        status: Some(cell(CellKind::Badge, "active")),
        avatar_seed: "p-1001".to_string(),
    };
    panel("header_ok", in_mode("header", PanelState::Ok, Some(Primitive::EntityHeader(header))));
    panel("header_error", PanelCase { fallback_title: "p-1001", ..in_mode("header", PanelState::Error, None) });
    let one_cell = |c: Cell| Some(Primitive::Cell(c));
    panel("fact_cell", in_mode("fact", PanelState::Ok, one_cell(toned(CellKind::Badge, "verified", Tone::Ok))));
    panel("fact_error", in_mode("fact", PanelState::Error, None));
    panel("count_cell", in_mode("count", PanelState::Ok, one_cell(cell(CellKind::Number, "14"))));
    panel("count_error", in_mode("count", PanelState::Error, None));
    panel("nav_counter_err", in_mode("nav", PanelState::Ok, one_cell(toned(CellKind::Number, "3", Tone::Err))));
    panel("nav_counter_warn", in_mode("nav", PanelState::Ok, one_cell(toned(CellKind::Number, "7", Tone::Warn))));
    panel("inline_cell", in_mode("inline", PanelState::Ok, one_cell(cell(CellKind::Badge, "banned"))));

    // ---- panel form: fresh, errors with a locked instance, readonly ----
    panel(
        "panel_form",
        PanelCase {
            form: Some(FormCase::fresh()),
            ..PanelCase::new(PanelState::Ok, Some(body(Primitive::Form(profile_form()))))
        },
    );
    panel(
        "panel_form_errors_locked",
        PanelCase {
            form: Some(FormCase {
                posted: Some(posted(&[
                    ("display_name", ""),
                    ("bio", "hi"),
                    ("level", "abc"),
                    ("ratio", "0.5"),
                    ("muted", "false"),
                    ("tier", "pro"),
                    ("regions", "us"),
                    ("meta", r#"{"v":2}"#),
                    ("owner", "p-2"),
                    ("credit", "5"),
                    ("credit__currency", "USD"),
                    ("cooldown", "1h"),
                ])),
                errors: BTreeMap::from([
                    ("display_name".to_string(), "must not be empty".to_string()),
                    ("level".to_string(), "must be an integer".to_string()),
                ]),
                banner: Some("Someone else saved this form first.".to_string()),
                locked: true,
            }),
            ..PanelCase::new(PanelState::Ok, Some(body(Primitive::Form(profile_form()))))
        },
    );
    panel("panel_form_readonly", PanelCase::new(PanelState::Readonly, Some(body(Primitive::Form(profile_form())))));

    // ---- slot placeholders: loading_block and the denied states, by draw mode ----
    for (name, mode, denied) in [
        ("slot_panel_loading", "panel", false),
        ("slot_panel_denied", "panel", true),
        ("slot_tile_loading", "tile", false),
        ("slot_tile_denied", "tile", true),
        ("slot_attention_loading", "attention", false),
        ("slot_attention_denied", "attention", true),
        ("slot_inline_loading", "fact", false),
    ] {
        out.push((name, render::fragment(env, "primitives.html", "slot", &slot(mode, denied))));
    }

    // ---- modals ----
    let no_errors = BTreeMap::new();
    let modal = |d: &ActionDecl, spec: ModalSpec<'_>| {
        let view = render::modal(
            d,
            spec,
            format!("/admin/_action/{MODULE}/{}", d.id),
            CSRF.to_string(),
            FORM_ID.to_string(),
            None,
            &no_errors,
        );
        render::fragment(env, "modal.html", "modal", &view)
    };
    let ban_spec = ConfirmSpec {
        title: "Ban p-1001?".to_string(),
        body: "The player is signed out and cannot sign in.".to_string(),
        tone: ConfirmTone::Danger,
        impact: vec![
            ("Sessions".to_string(), cell(CellKind::Number, "3")),
            ("Status".to_string(), cell(CellKind::Badge, "banned")),
            ("Owner".to_string(), cell(CellKind::User, "anna.kowalska")),
        ],
        reason: ReasonReq::Required,
        reason_hint: Some("e.g. chargeback fraud, ticket #".to_string()),
        type_to_confirm: Some("p-1001".to_string()),
        confirm_label: "Ban".to_string(),
    };
    let ban = ActionDecl { danger: true, ..decl("ban", "Ban player", "", Then::Confirm(ban_spec.clone())) };
    out.push(("modal_confirm_danger_impact_reason", modal(&ban, ModalSpec::Confirm(&ban_spec))));
    let pause_spec = ConfirmSpec {
        body: "Deliveries stop until resumed.".to_string(),
        reason: ReasonReq::Optional,
        confirm_label: "Pause".to_string(),
        ..confirm("Pause webhooks?", ConfirmTone::Warn)
    };
    let pause = decl("pause", "Pause queue", "pause", Then::Confirm(pause_spec.clone()));
    out.push(("modal_confirm_warn_optional_reason", modal(&pause, ModalSpec::Confirm(&pause_spec))));
    let grant_spec = FormSpec {
        fields: vec![
            Field { required: true, ..field("item_id", "Item", FieldKind::Ref("item".to_string()), None) },
            field("qty", "Quantity", FieldKind::Int, Some("1")),
        ],
        hidden: BTreeMap::new(),
        submit_label: "Grant".to_string(),
        note: Some("Granted items appear in the next sync.".to_string()),
        revision: None,
    };
    let grant = decl("grant", "Grant item", "gift", Then::Form(grant_spec.clone()));
    out.push(("modal_form", modal(&grant, ModalSpec::Form(&grant_spec))));
    let result = ActionResultView { modal: None, toast: Some("Queue paused".to_string()), tone: "info" };
    out.push(("modal_result_toast", render::fragment(env, "modal.html", "result", &result)));
    let flash = FlashView {
        message: "API key created".to_string(),
        reveal: Some(RevealView {
            label: "Key gb_live_…".to_string(),
            value: "gb_live_0123456789abcdef".to_string(),
            hint: None,
            expires_at: Some("2026-12-01".to_string()),
        }),
        tone: "ok",
    };
    out.push(("reveal_secret", render::fragment(env, "modal.html", "reveal", &flash)));

    // ---- row menu and search palette ----
    let row_entity = entity("player", "p-1001");
    let profile = decl("profile", "Open profile", "", Then::Link(Link::Entity(row_entity.clone())));
    let menu_items = [
        action("players", &decl("kick", "Kick", "x", Then::Invoke), Role::Admin, true),
        action("players", &ban, Role::Admin, true),
        action("players", &profile, Role::Admin, true),
        action("economy", &grant, Role::Admin, true),
        action("economy", &ActionDecl { min_role: Role::Admin, ..grant.clone() }, Role::Viewer, true),
    ];
    let menu = MenuView {
        entity: row_entity.clone(),
        groups: render::menu_groups(menu_items.iter().map(|a| render::bind(a, Some(&row_entity), None)).collect()),
    };
    out.push(("menu_row_actions", render::fragment(env, "primitives.html", "menu", &menu)));
    let hit = |title: &str, subtitle: &str, badge: Option<&str>| SearchHitView {
        hl: false,
        title: title.to_string(),
        subtitle: subtitle.to_string(),
        badge: badge.map(str::to_string),
        href: format!("/admin/e/player/{subtitle}"),
    };
    let search = SearchView::new(
        "ann".to_string(),
        String::new(),
        vec![
            SearchGroupView {
                module: "players".to_string(),
                label: "Players".to_string(),
                icon: "user".to_string(),
                hits: vec![hit("Anna Kowalska", "p-1001", Some("active")), hit("Annette", "p-2040", None)],
                error: None,
            },
            SearchGroupView {
                module: "billing".to_string(),
                label: "Invoices".to_string(),
                icon: "coins".to_string(),
                hits: Vec::new(),
                error: Some("deadline of 800ms elapsed".to_string()),
            },
        ],
    );
    out.push(("search_results_and_error", render::fragment(env, "primitives.html", "search", &search)));
    let nothing = SearchView::new(
        "zzz".to_string(),
        String::new(),
        vec![SearchGroupView {
            module: "players".to_string(),
            label: "Players".to_string(),
            icon: "user".to_string(),
            hits: Vec::new(),
            error: None,
        }],
    );
    out.push(("search_no_matches", render::fragment(env, "primitives.html", "search", &nothing)));

    out.into_iter()
        .map(|(name, rendered)| {
            let raw = rendered.unwrap_or_else(|e| panic!("golden case {name} failed to render: {e:#}"));
            (name.to_string(), golden_form(name, &raw))
        })
        .collect()
}
