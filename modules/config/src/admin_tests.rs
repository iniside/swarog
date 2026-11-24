//! The config provider: the manifest, the type rule, the Keys, Edit and Revisions bodies, the
//! value normalisation; and, over a live Postgres, the store reads through `Provider::panel`
//! (prefix search, type filter, selection, the SQL-computed times, the revision history and its
//! writers) and the writes through `Provider::invoke`.

use controlplane::{form_spec_errors, order, CellKind, DataTable, FormSpec, Layout, Point, Primitive, Role, Timeline};

use super::*;
use crate::tests::{changed_event_count, cleanup, test_pool, unique_ns, CONFIG_DB_TEST_GUARD};
use crate::Service;

fn table(body: &PanelBody) -> &DataTable {
    match &body.primitive {
        Primitive::DataTable(t) => t,
        other => panic!("a DataTable, got {other:?}"),
    }
}

fn form(body: &PanelBody) -> &FormSpec {
    match &body.primitive {
        Primitive::Form(f) => f,
        other => panic!("a Form, got {other:?}"),
    }
}

fn timeline(body: &PanelBody) -> &Timeline {
    match &body.primitive {
        Primitive::Timeline(t) => t,
        other => panic!("a Timeline, got {other:?}"),
    }
}

fn is_empty(body: &PanelBody) -> bool {
    matches!(&body.primitive, Primitive::Empty { title, .. } if title == NO_KEY)
}

fn query(contribution: &str, search: Option<&str>, params: &[(&str, &str)]) -> PanelQuery {
    PanelQuery {
        module: MODULE.into(),
        contribution: contribution.into(),
        entity: None,
        search: search.map(str::to_string),
        cursor: None,
        before: None,
        queue_state: None,
        range: None,
        params: params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        refs: Vec::new(),
    }
}

fn row(namespace: &str, key: &str, value: &str, revision: i64) -> KeyRow {
    KeyRow {
        namespace: namespace.into(),
        key: key.into(),
        value: value.into(),
        revision,
        updated: "12:30".into(),
        by: Some("ola.w".into()),
    }
}

fn link_params(cell: &Cell) -> BTreeMap<String, String> {
    match &cell.link {
        Some(Link::Page { module, page, params }) => {
            assert_eq!((module.as_str(), page.as_str()), (MODULE, PAGE));
            params.clone()
        }
        other => panic!("a page link, got {other:?}"),
    }
}

/// A field's key, kind, required, mono, default and placeholder.
type FieldShape<'a> = (&'a str, &'a FieldKind, bool, bool, Option<&'a str>, Option<&'a str>);

#[test]
fn manifest_declares_the_page_and_its_writes() {
    let m = manifest();
    assert_eq!(m.module, "config");
    assert_eq!(m.entries.len(), 1);
    let nav = &m.entries[0];
    assert_eq!((nav.section.as_str(), nav.id.as_str(), nav.page.as_str()), ("platform", "config", "settings"));
    assert_eq!((nav.label.as_str(), nav.icon.as_str()), ("Config & flags", "sliders"));
    assert_eq!(nav.order, order::platform_nav::CONFIG);
    assert!(nav.counter.is_none());
    let page = &m.pages[0];
    assert_eq!((page.id.as_str(), page.title.as_str(), page.subtitle.as_str()), (PAGE, TITLE, SUBTITLE));
    assert_eq!(page.layout, Layout::Grid21(vec!["keys".into()], vec!["edit".into(), "revisions".into()]));
    assert_eq!(page.params, vec!["key".to_string(), "type".to_string()]);
    assert_eq!(page.head_actions, vec![CREATE.to_string()]);
    let got: Vec<(&str, i32, &str)> =
        m.contributions.iter().map(|c| (c.id.as_str(), c.order, c.title.as_str())).collect();
    assert_eq!(got, vec![("keys", 10, "Keys"), ("edit", 20, "Edit"), ("revisions", 30, "Revisions")]);
    for c in &m.contributions {
        assert_eq!(c.point, Point::Page(PAGE.into()));
        assert_eq!(c.min_role, Role::Viewer);
        assert!(c.cache_ttl_s.is_none() && c.readonly.is_none());
    }
    assert!(m.entity_kinds.is_empty() && m.search.is_empty());
    assert!(m.writes);
    let got: Vec<(&str, &ActionPoint, &str, &str, i32, Emphasis)> = m
        .actions
        .iter()
        .map(|a| (a.id.as_str(), &a.point, a.label.as_str(), a.icon.as_str(), a.order, a.emphasis))
        .collect();
    let head = ActionPoint::PageHead(PAGE.into());
    let edit = ActionPoint::PanelForm(EDIT.into());
    assert_eq!(
        got,
        vec![
            ("setting.create", &head, "Key", "plus", 10, Emphasis::Primary),
            ("setting.edit", &edit, "Save revision", "", 10, Emphasis::Default),
        ]
    );
    for a in &m.actions {
        assert_eq!(a.min_role, Role::Operator);
        assert!(a.implemented && !a.danger && !a.prepare && !a.multi);
        assert!(m.write_refusal(&a.id).is_none(), "{}", a.id);
    }
    assert_eq!(m.actions[1].then, Then::Invoke);
    let Then::Form(create) = &m.actions[0].then else { panic!("the create is a form") };
    assert!(form_spec_errors(create).is_empty(), "{:?}", form_spec_errors(create));
    let fields: Vec<FieldShape<'_>> = create
        .fields
        .iter()
        .map(|f| (f.key.as_str(), &f.kind, f.required, f.mono, f.default.as_deref(), f.placeholder.as_deref()))
        .collect();
    let types = FieldKind::Enum(["bool", "int", "float", "string", "json"].map(String::from).to_vec());
    assert_eq!(
        fields,
        vec![
            ("key", &FieldKind::String, true, true, None, Some("namespace.key")),
            ("type", &types, true, false, Some("string"), None),
            ("value", &FieldKind::Text, true, true, None, None),
        ]
    );
    assert_eq!((create.submit_label.as_str(), create.title.as_deref()), ("Create key", Some("New key")));
    assert_eq!(create.reason, ReasonReq::Optional);
}

#[test]
fn value_type_follows_the_stored_text() {
    let cases = [
        ("", ValueType::String),
        (" 4", ValueType::String),
        ("4 ", ValueType::String),
        ("true", ValueType::Bool),
        ("false", ValueType::Bool),
        ("TRUE", ValueType::String),
        ("1", ValueType::Int),
        ("-3", ValueType::Int),
        ("007", ValueType::String),
        ("1e3", ValueType::Float),
        ("2.5", ValueType::Float),
        ("9223372036854775808", ValueType::Float),
        ("\"x\"", ValueType::Json),
        ("null", ValueType::Json),
        ("[1, 2]", ValueType::Json),
        ("{\"a\": 1}", ValueType::Json),
        ("arena", ValueType::String),
    ];
    for (text, want) in cases {
        assert_eq!(ValueType::of(text), want, "{text:?}");
    }
    for t in ValueType::ALL {
        assert_eq!(ValueType::parse(t.name()), Some(t));
    }
    assert_eq!(ValueType::parse("text"), None);
}

#[test]
fn pick_reads_the_key_param() {
    let params = |v: &str| BTreeMap::from([(PARAM_KEY.to_string(), v.to_string())]);
    assert_eq!(pick(&BTreeMap::new()), Pick::First);
    assert_eq!(pick(&params("store.featured_rotation")), Pick::Key("store", "featured_rotation"));
    for bad in ["", "nodot", "Bad.key", "a.b.c", ".k", "ns."] {
        assert_eq!(pick(&params(bad)), Pick::None, "{bad:?}");
    }
}

#[test]
fn keys_body_draws_each_column_and_the_selection() {
    let page = KeyPage {
        rows: vec![
            row("game", "hardcore", "true", 4),
            row("game", "limit", "42", 7),
            KeyRow { by: None, ..row("store", "rot", "[1]", 9) },
        ],
        total: 12,
        first: Some("game.hardcore".into()),
    };
    let body = keys_body(&page, None, None, Some("game.limit"));
    assert_eq!(body.meta.as_deref(), Some("12"));
    assert!(body.foot.is_none());
    let t = table(&body);
    let labels: Vec<&str> = t.columns.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["Key", "Type", "Value", "Rev", "Updated"]);
    assert!(t.columns[2].wrap);
    assert_eq!(t.columns[3].align, Align::Right);
    assert!(t.columns.iter().all(|c| c.sorted.is_none() && !c.sortable));
    assert_eq!(t.search_placeholder.as_deref(), Some("Key prefix"));
    assert_eq!(t.filters.len(), 1);
    assert_eq!(t.filters[0].key, "type");
    let names: Vec<String> = ["bool", "int", "float", "string", "json"].map(String::from).into();
    assert_eq!(t.filters[0].kind, FilterKind::Select(names));
    assert!(t.page.next_cursor.is_none() && t.page.total.is_none());

    let highlighted: Vec<bool> = t.rows.iter().map(|r| r.highlight).collect();
    assert_eq!(highlighted, [false, true, false]);
    let flag = &t.rows[0].cells;
    assert_eq!((flag[0].kind, flag[0].value.as_str()), (CellKind::Mono, "game.hardcore"));
    assert_eq!((flag[1].kind, flag[1].value.as_str()), (CellKind::Ref, "bool"));
    assert_eq!((flag[2].kind, flag[2].value.as_str()), (CellKind::Toggle, "true"));
    assert_eq!((flag[3].kind, flag[3].value.as_str()), (CellKind::Number, "4"));
    assert_eq!((flag[4].kind, flag[4].value.as_str(), flag[4].row), (CellKind::Dim, "12:30", true));
    assert_eq!(flag[4].more.len(), 1);
    assert_eq!((flag[4].more[0].kind, flag[4].more[0].value.as_str()), (CellKind::User, "ola.w"));
    let rot = &t.rows[2].cells;
    assert_eq!((rot[4].more[0].kind, rot[4].more[0].value.as_str()), (CellKind::Muted, "—"));
    assert_eq!((rot[1].value.as_str(), rot[2].kind, rot[2].value.as_str()), ("json", CellKind::Mono, "[1]"));
    assert_eq!(link_params(&flag[0]), BTreeMap::from([("key".to_string(), "game.hardcore".to_string())]));
}

#[test]
fn key_link_keeps_the_search_and_the_type() {
    let page = KeyPage { rows: vec![row("game", "limit", "42", 7)], total: 1, first: Some("game.limit".into()) };
    let body = keys_body(&page, Some(ValueType::Int), Some("game."), None);
    let t = table(&body);
    assert!(!t.rows[0].highlight);
    let want = BTreeMap::from([
        ("key".to_string(), "game.limit".to_string()),
        ("q".to_string(), "game.".to_string()),
        ("type".to_string(), "int".to_string()),
    ]);
    assert_eq!(link_params(&t.rows[0].cells[0]), want);
}

#[test]
fn keys_body_filters_the_read_then_caps_the_draw() {
    let mut rows: Vec<KeyRow> = (0..150).map(|i| row("a", &format!("n{i:03}"), "1", 1)).collect();
    rows.extend((0..150).map(|i| row("b", &format!("s{i:03}"), "text", 1)));
    let page = KeyPage { rows, total: 300, first: Some("a.n000".into()) };

    let ints = keys_body(&page, Some(ValueType::Int), None, None);
    assert_eq!(table(&ints).rows.len(), MAX_ROWS);
    assert!(table(&ints).rows.iter().all(|r| r.cells[1].value == "int"));
    assert!(ints.foot.is_some());
    assert_eq!(ints.meta.as_deref(), Some("300"));
    // The strings are the last 150 rows: a cap before the filter would draw none of them.
    let strings = keys_body(&page, Some(ValueType::String), None, None);
    assert_eq!(table(&strings).rows.len(), MAX_ROWS);
    assert!(table(&strings).rows.iter().all(|r| r.cells[1].value == "string"));
    assert_eq!(table(&strings).rows[0].cells[0].value, "b.s000");
    assert!(strings.foot.is_some());

    let json = keys_body(&page, Some(ValueType::Json), None, None);
    assert!(table(&json).rows.is_empty());
    assert!(json.foot.is_none());
    assert_eq!(json.meta.as_deref(), Some("300"), "the meta counts every key, whatever the filter");

    let over: Vec<KeyRow> = (0..=MAX_READ).map(|i| row("c", &format!("k{i:04}"), "text", 1)).collect();
    let page = KeyPage { rows: over, total: 900, first: Some("c.k0000".into()) };
    let all = keys_body(&page, None, None, None);
    assert_eq!(table(&all).rows.len(), MAX_ROWS);
    assert!(all.foot.is_some());
    let none = keys_body(&page, Some(ValueType::Int), None, None);
    assert!(table(&none).rows.is_empty());
    assert!(none.foot.is_some(), "a filtered read that hit the cap says so");
}

#[test]
fn edit_body_serves_the_value_under_its_type() {
    let cases = [
        ("true", FieldKind::Bool),
        ("42", FieldKind::Int),
        ("2.5", FieldKind::Float),
        ("1e3", FieldKind::Float),
        ("arena", FieldKind::String),
        ("[\"a\", \"b\"]", FieldKind::Json),
        ("", FieldKind::String),
    ];
    for (value, kind) in cases {
        let body = edit_body(&row("game", "k", value, 7));
        assert_eq!(body.heading.as_deref(), Some("game.k"));
        assert_eq!(body.meta.as_deref(), Some("rev 7"));
        let f = form(&body);
        assert!(form_spec_errors(f).is_empty(), "{value:?}: {:?}", form_spec_errors(f));
        assert_eq!(f.fields.len(), 1);
        let field = &f.fields[0];
        assert_eq!((field.key.as_str(), field.label.as_str()), ("value", "Value"));
        assert_eq!(field.kind, kind, "{value:?}");
        assert_eq!(field.default.as_deref(), Some(value));
        // A blank post of a required field is refused, so only a string value may be saved blank.
        assert_eq!(field.required, kind != FieldKind::String, "{value:?}");
        assert!(field.mono && !field.readonly);
        assert_eq!(f.hidden, BTreeMap::from([("key".to_string(), "game.k".to_string())]));
        assert_eq!(f.revision, Some(7));
        assert_eq!(f.submit_label, "Save revision");
        assert_eq!(f.note.as_deref(), Some("Compare-and-swap: saves only if still rev 7."));
        assert!(f.title.is_none() && f.modes.is_empty());
        assert_eq!(f.reason, ReasonReq::Optional);
        assert_eq!(f.reason_label.as_deref(), Some("Change note"));
        assert_eq!(f.reason_hint.as_deref(), Some("Why? (recorded in the audit log)"));
    }
}

fn revision(revision: i64, value: Option<&str>, operation: &str, by: &str) -> Revision {
    Revision {
        revision,
        value: value.map(str::to_string),
        operation: operation.into(),
        by: by.into(),
        at: format!("at-{revision}"),
    }
}

#[test]
fn revisions_body_draws_each_stored_revision_newest_first() {
    let sel = Selected {
        row: row("game", "limit", "42", 30),
        revisions: vec![
            revision(30, Some("42"), "insert", "marta.k"),
            revision(21, None, "delete", "gamebackend"),
            revision(20, Some("7"), "update", "system"),
        ],
    };
    let body = revisions_body(&sel);
    let tl = timeline(&body);
    // The timeline draws actor, title, verb, summary, then badges (primitives.html.tmpl
    // `timeline`): with only a summary and one badge an entry reads "rev N by <writer>".
    assert!(tl.date_only && !tl.live && tl.cursor_before.is_none());
    let drawn: Vec<(&str, &str, Option<&str>, Option<Tone>)> =
        tl.entries.iter().map(|e| (e.at.as_str(), e.summary.as_str(), e.detail.as_deref(), e.tone)).collect();
    assert_eq!(
        drawn,
        [
            ("at-30", "rev 30 by", Some("42"), Some(Tone::Accent)),
            ("at-21", "rev 21 deleted by", None, None),
            ("at-20", "rev 20 by", Some("7"), None),
        ]
    );
    let writers: Vec<(CellKind, &str)> =
        tl.entries.iter().flat_map(|e| e.badges.iter().map(|b| (b.kind, b.value.as_str()))).collect();
    assert_eq!(writers, [(CellKind::User, "marta.k"), (CellKind::User, "gamebackend"), (CellKind::User, "system")]);
    for entry in &tl.entries {
        assert!(entry.verb.is_empty() && entry.actor.is_none() && entry.title.is_none() && entry.target.is_none());
        assert_eq!(entry.icon, "edit");
    }

    let none = revisions_body(&Selected { row: row("game", "limit", "42", 30), revisions: Vec::new() });
    assert!(timeline(&none).entries.is_empty());
}

// ---- Live Postgres --------------------------------------------------------------------

fn is_hh_mm(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 5 && b[2] == b':' && [0, 1, 3, 4].iter().all(|&i| b[i].is_ascii_digit())
}

fn is_mon_dd(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 6
        && b[0].is_ascii_uppercase()
        && b[1..3].iter().all(u8::is_ascii_lowercase)
        && b[3] == b' '
        && b[4..].iter().all(u8::is_ascii_digit)
}

#[tokio::test]
async fn updated_time_is_the_time_today_else_the_date() {
    let Some(pool) = test_pool().await else { return };
    let sql = |age: &str| format!("SELECT {} FROM (SELECT now() - interval '{age}' AS updated_at) v", store::UPDATED_EXPR);
    let (today,): (String,) = sqlx::query_as(&sql("0 seconds")).fetch_one(&pool).await.unwrap();
    assert!(is_hh_mm(&today), "{today:?}");
    let (older,): (String,) = sqlx::query_as(&sql("40 days")).fetch_one(&pool).await.unwrap();
    assert!(is_mon_dd(&older), "{older:?}");
}

#[tokio::test]
async fn panels_read_the_store() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Service::new(pool.clone());
    for (key, value) in [("ratio", "2.5"), ("flag", "true"), ("limit", "42"), ("rot", "[\"a\"]"), ("name", "arena")] {
        svc.set(&ns, key, value).await.unwrap();
    }
    let admin = ConfigAdmin::new(pool.clone());
    let prefix = format!("{ns}.");
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM config.settings").fetch_one(&pool).await.unwrap();
    let limit_key = format!("{ns}.limit");
    let (limit_rev, limit_at): (i64, String) = sqlx::query_as(
        "SELECT revision, to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
         FROM config.settings WHERE namespace = $1 AND key = 'limit'",
    )
    .bind(&ns)
    .fetch_one(&pool)
    .await
    .unwrap();

    let keys = admin.panel(query(KEYS, Some(&prefix), &[("key", &limit_key)])).await.unwrap();
    assert_eq!(keys.meta, Some(count.to_string()));
    let t = table(&keys);
    let drawn: Vec<(&str, &str)> = t.rows.iter().map(|r| (r.cells[0].value.as_str(), r.cells[1].value.as_str())).collect();
    let want: Vec<(String, &str)> = [("flag", "bool"), ("limit", "int"), ("name", "string"), ("ratio", "float"), ("rot", "json")]
        .into_iter()
        .map(|(k, t)| (format!("{ns}.{k}"), t))
        .collect();
    assert_eq!(drawn, want.iter().map(|(k, t)| (k.as_str(), *t)).collect::<Vec<_>>());
    let highlighted: Vec<&str> = t.rows.iter().filter(|r| r.highlight).map(|r| r.cells[0].value.as_str()).collect();
    assert_eq!(highlighted, [limit_key.as_str()]);
    assert_eq!(t.rows[1].cells[3].value, limit_rev.to_string());
    assert!(t.rows.iter().all(|r| is_hh_mm(&r.cells[4].value)));
    let writers: Vec<(CellKind, &str)> =
        t.rows.iter().map(|r| (r.cells[4].more[0].kind, r.cells[4].more[0].value.as_str())).collect();
    assert_eq!(writers, [(CellKind::User, "system"); 5]);

    let ints = admin.panel(query(KEYS, Some(&prefix), &[("type", "int")])).await.unwrap();
    let names: Vec<&str> = table(&ints).rows.iter().map(|r| r.cells[0].value.as_str()).collect();
    assert_eq!(names, [limit_key.as_str()]);
    assert_eq!(ints.meta, Some(count.to_string()));
    let bad = admin.panel(query(KEYS, None, &[("type", "text")])).await.unwrap_err();
    assert_eq!(bad.status, opsapi::Status::Invalid);

    let edit = admin.panel(query(EDIT, None, &[("key", &limit_key)])).await.unwrap();
    assert_eq!(edit.heading.as_deref(), Some(limit_key.as_str()));
    assert_eq!(edit.meta, Some(format!("rev {limit_rev}")));
    assert_eq!(form(&edit).fields[0].kind, FieldKind::Int);
    assert_eq!(form(&edit).revision, Some(limit_rev as u64));

    let revisions = admin.panel(query(REVISIONS, None, &[("key", &limit_key)])).await.unwrap();
    let entries = &timeline(&revisions).entries;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].at, limit_at);
    assert_eq!(entries[0].summary, format!("rev {limit_rev} by"));
    assert!(entries[0].verb.is_empty() && entries[0].actor.is_none());
    assert_eq!(entries[0].badges[0].value, "system");
    assert_eq!(entries[0].detail.as_deref(), Some("42"));

    for missing in [format!("{ns}.missing"), "Nope".to_string()] {
        for c in [EDIT, REVISIONS] {
            let body = admin.panel(query(c, None, &[("key", &missing)])).await.unwrap();
            assert!(is_empty(&body), "{c} for {missing:?}: {body:?}");
        }
    }
    cleanup(&pool, &ns).await;
}

#[tokio::test]
async fn no_key_param_selects_the_first_key_in_byte_order() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Service::new(pool.clone());
    for key in ["k", "ab", "a_z"] {
        svc.set(&ns, key, "v").await.unwrap();
    }
    let (first,): (String,) = sqlx::query_as(
        r#"SELECT namespace || '.' || key FROM config.settings ORDER BY namespace COLLATE "C", key COLLATE "C" LIMIT 1"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let admin = ConfigAdmin::new(pool.clone());
    let edit = admin.panel(query(EDIT, None, &[])).await.unwrap();
    assert_eq!(edit.heading.as_deref(), Some(first.as_str()));
    let keys = admin.panel(query(KEYS, None, &[])).await.unwrap();
    let highlighted: Vec<&str> =
        table(&keys).rows.iter().filter(|r| r.highlight).map(|r| r.cells[0].value.as_str()).collect();
    assert_eq!(highlighted, [first.as_str()]);
    // Byte order puts '_' (0x5F) before 'b'; a linguistic collation ignores '_' and sorts
    // "ab" before "a_z".
    let prefix = format!("{ns}.");
    let ours = admin.panel(query(KEYS, Some(&prefix), &[])).await.unwrap();
    let names: Vec<&str> = table(&ours).rows.iter().map(|r| r.cells[0].value.as_str()).collect();
    let want = ["a_z", "ab", "k"].map(|k| format!("{ns}.{k}"));
    assert_eq!(names, want.iter().map(String::as_str).collect::<Vec<_>>());
    cleanup(&pool, &ns).await;
}

#[tokio::test]
async fn the_type_filter_sees_past_the_drawn_rows() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Service::new(pool.clone());
    for i in 0..=MAX_ROWS {
        svc.set(&ns, &format!("s{i:03}"), "text").await.unwrap();
    }
    svc.set(&ns, "zz", "7").await.unwrap();
    let admin = ConfigAdmin::new(pool.clone());
    let prefix = format!("{ns}.");
    let int_key = format!("{ns}.zz");

    let all = admin.panel(query(KEYS, Some(&prefix), &[])).await.unwrap();
    assert_eq!(table(&all).rows.len(), MAX_ROWS);
    assert!(table(&all).rows.iter().all(|r| r.cells[0].value != int_key), "the int key sorts past the drawn rows");
    let ints = admin.panel(query(KEYS, Some(&prefix), &[("type", "int")])).await.unwrap();
    let names: Vec<&str> = table(&ints).rows.iter().map(|r| r.cells[0].value.as_str()).collect();
    assert_eq!(names, [int_key.as_str()], "the filter reads up to MAX_READ keys, not the drawn MAX_ROWS");
    cleanup(&pool, &ns).await;
}

#[test]
fn stored_value_normalises_and_keeps_the_type() {
    let ok = [
        (ValueType::Float, "2", "2.0"),
        (ValueType::Float, "2.5", "2.5"),
        (ValueType::Float, "1e3", "1000.0"),
        (ValueType::Int, "007", "7"),
        (ValueType::Bool, "false", "false"),
        (ValueType::Json, " [1, 2] ", "[1, 2]"),
        (ValueType::String, "arena", "arena"),
        (ValueType::String, "", ""),
        (ValueType::String, " 5 ", " 5 "),
    ];
    for (t, raw, want) in ok {
        assert_eq!(stored_value(t, raw).as_deref(), Ok(want), "{t:?} {raw:?}");
    }
    let refused = [
        (ValueType::Json, "5", "reads as int, not json"),
        (ValueType::String, "5", "reads as int, not string"),
        (ValueType::String, "true", "reads as bool, not string"),
        (ValueType::Bool, "yes", "must be true or false"),
        (ValueType::Float, "NaN", "must be a finite number"),
        (ValueType::Json, "{", "must be valid JSON"),
    ];
    for (t, raw, want) in refused {
        assert_eq!(stored_value(t, raw), Err(want.to_string()), "{t:?} {raw:?}");
    }
}

fn call(action: &str, input: &[(&str, &str)], expected_revision: Option<u64>) -> InvokeCall {
    InvokeCall {
        module: MODULE.into(),
        action: action.into(),
        entity: None,
        input: input.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        expected_revision,
        reason: None,
        operator: "tester".into(),
        item: None,
        request_id: None,
    }
}

fn field_errors(outcome: InvokeOutcome) -> BTreeMap<String, String> {
    match outcome {
        InvokeOutcome::FieldErrors(errors) => errors,
        other => panic!("field errors, got {other:?}"),
    }
}

#[test]
fn the_input_cap_is_the_first_check() {
    let cap = crate::conformance::CP_INVOKE_INPUT_VALUE_BYTES;
    assert_eq!(cap, controlplane::MAX_INPUT_VALUE_BYTES);
    assert!(!crate::conformance::conformance_cp_invoke_input_rejected(cap), "at the cap the save reaches the store");
    assert!(crate::conformance::conformance_cp_invoke_input_rejected(cap + 1));
}

/// The cap covers every field, the key too: one byte over it, a create is refused before the
/// key's own check, which at the cap answers a field error, and before the lazy pool connects.
#[tokio::test]
async fn the_input_cap_covers_the_key() {
    let pool = sqlx::postgres::PgPoolOptions::new().connect_lazy("postgres://x@127.0.0.1:1/x").unwrap();
    let admin = ConfigAdmin::new(pool);
    let cap = controlplane::MAX_INPUT_VALUE_BYTES;
    let create = |key: String| call(CREATE, &[("key", &key), ("type", "int"), ("value", "1")], None);
    let over = admin.invoke(create("a".repeat(cap + 1))).await.unwrap_err();
    assert_eq!(over.status, opsapi::Status::Invalid);
    let at = field_errors(admin.invoke(create("a".repeat(cap))).await.unwrap());
    assert_eq!(at.keys().collect::<Vec<_>>(), ["key"]);
}

/// Refusals that answer before any statement: the lazy pool never connects.
#[tokio::test]
async fn invoke_refuses_bad_input_before_the_store() {
    let pool = sqlx::postgres::PgPoolOptions::new().connect_lazy("postgres://x@127.0.0.1:1/x").unwrap();
    let admin = ConfigAdmin::new(pool);
    for bad in ["nodot", "a.b.c", "A.b", ".b", "a."] {
        let errors = field_errors(admin.invoke(call(SAVE, &[("key", bad), ("value", "1")], Some(1))).await.unwrap());
        assert_eq!(errors.keys().collect::<Vec<_>>(), ["key"], "{bad:?}");
    }
    let no_rev = admin.invoke(call(SAVE, &[("key", "a.b"), ("value", "1")], None)).await.unwrap_err();
    assert_eq!(no_rev.status, opsapi::Status::Invalid);
    let errors =
        field_errors(admin.invoke(call(CREATE, &[("key", "a"), ("type", "text"), ("value", "1")], None)).await.unwrap());
    assert_eq!(
        errors,
        BTreeMap::from([
            ("key".to_string(), "must be namespace.key, each of a-z, 0-9 and _".to_string()),
            ("type".to_string(), "is not one of the types".to_string()),
        ])
    );
    let errors =
        field_errors(admin.invoke(call(CREATE, &[("key", "a.b"), ("type", "json"), ("value", "5")], None)).await.unwrap());
    assert_eq!(errors, BTreeMap::from([("value".to_string(), "reads as int, not json".to_string())]));
    let unknown = admin.invoke(call("setting.delete", &[], None)).await.unwrap_err();
    assert_eq!(unknown.status, opsapi::Status::NotFound);
}

async fn stored(pool: &PgPool, ns: &str, key: &str) -> Option<(String, i64)> {
    sqlx::query_as("SELECT value, revision FROM config.settings WHERE namespace = $1 AND key = $2")
        .bind(ns)
        .bind(key)
        .fetch_optional(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn save_compares_and_swaps_on_the_revision() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Service::new(pool.clone());
    svc.set(&ns, "limit", "42").await.unwrap();
    let admin = ConfigAdmin::new(pool.clone());
    let id = format!("{ns}.limit");
    let (_, rev) = stored(&pool, &ns, "limit").await.unwrap();
    let events = changed_event_count(&pool, &ns).await;

    let saved = admin.invoke(call(SAVE, &[("key", &id), ("value", "43")], Some(rev as u64))).await.unwrap();
    let (value, now) = stored(&pool, &ns, "limit").await.unwrap();
    assert_eq!(value, "43");
    assert!(now > rev);
    assert_eq!(saved, InvokeOutcome::Ok { message: format!("Saved {id} at rev {now}"), reveal: None, redirect: None });
    assert_eq!(changed_event_count(&pool, &ns).await, events + 1, "the save goes through the trigger");

    let stale = admin.invoke(call(SAVE, &[("key", &id), ("value", "44")], Some(rev as u64))).await.unwrap();
    assert_eq!(stale, InvokeOutcome::Conflict { current_revision: now as u64 });
    assert_eq!(stored(&pool, &ns, "limit").await, Some(("43".to_string(), now)));

    let missing = format!("{ns}.missing");
    let gone = admin.invoke(call(SAVE, &[("key", &missing), ("value", "1")], Some(now as u64))).await.unwrap_err();
    assert_eq!(gone.status, opsapi::Status::Conflict);
    assert_eq!(stored(&pool, &ns, "missing").await, None);
    cleanup(&pool, &ns).await;
}

/// The write after the revision check, reached when another write lands between the two: the
/// answer is the key's revision now, or `Conflict` once the key is gone, and the row is left.
#[tokio::test]
async fn a_write_between_the_check_and_the_update_is_a_conflict() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Service::new(pool.clone());
    svc.set(&ns, "k", "1").await.unwrap();
    let (_, checked) = stored(&pool, &ns, "k").await.unwrap();
    svc.set(&ns, "k", "2").await.unwrap();
    let (_, now) = stored(&pool, &ns, "k").await.unwrap();
    assert!(now > checked);

    let raced = swap(&pool, &ns, "k", "3", checked, "tester").await.unwrap();
    assert_eq!(raced, InvokeOutcome::Conflict { current_revision: now as u64 });
    assert_eq!(stored(&pool, &ns, "k").await, Some(("2".to_string(), now)));

    let vanished = swap(&pool, &ns, "missing", "3", now, "tester").await.unwrap_err();
    assert_eq!(vanished.status, opsapi::Status::Conflict);
    assert_eq!(stored(&pool, &ns, "missing").await, None);
    cleanup(&pool, &ns).await;
}

/// The statement itself compares the revision: a stale write matches no row.
#[tokio::test]
async fn the_update_statement_matches_only_the_expected_revision() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    Service::new(pool.clone()).set(&ns, "k", "1").await.unwrap();
    let (_, rev) = stored(&pool, &ns, "k").await.unwrap();
    assert_eq!(store::update(&pool, &ns, "k", "2", rev - 1, "tester").await.unwrap(), None);
    assert_eq!(stored(&pool, &ns, "k").await, Some(("1".to_string(), rev)));
    let stamped = store::update(&pool, &ns, "k", "2", rev, "tester").await.unwrap();
    assert_eq!(stored(&pool, &ns, "k").await, Some(("2".to_string(), stamped.unwrap())));
    assert_eq!(store::update(&pool, &ns, "nope", "2", rev, "tester").await.unwrap(), None);
    cleanup(&pool, &ns).await;
}

#[tokio::test]
async fn save_keeps_the_type_and_normalises_the_value() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Service::new(pool.clone());
    svc.set(&ns, "rot", "[\"a\"]").await.unwrap();
    svc.set(&ns, "ratio", "2.5").await.unwrap();
    svc.set(&ns, "motd", "").await.unwrap();
    let admin = ConfigAdmin::new(pool.clone());

    let (rot, rot_rev) = stored(&pool, &ns, "rot").await.unwrap();
    let id = format!("{ns}.rot");
    let errors = field_errors(admin.invoke(call(SAVE, &[("key", &id), ("value", "5")], Some(rot_rev as u64))).await.unwrap());
    assert_eq!(errors, BTreeMap::from([("value".to_string(), "reads as int, not json".to_string())]));
    assert_eq!(stored(&pool, &ns, "rot").await, Some((rot, rot_rev)));

    // The portal posts a float as f64's Display text: 2.0 arrives as "2".
    let (_, ratio_rev) = stored(&pool, &ns, "ratio").await.unwrap();
    let id = format!("{ns}.ratio");
    let saved = admin.invoke(call(SAVE, &[("key", &id), ("value", "2")], Some(ratio_rev as u64))).await.unwrap();
    assert!(matches!(saved, InvokeOutcome::Ok { .. }), "{saved:?}");
    let (value, _) = stored(&pool, &ns, "ratio").await.unwrap();
    assert_eq!(value, "2.0");
    assert_eq!(ValueType::of(&value), ValueType::Float);

    let (_, motd_rev) = stored(&pool, &ns, "motd").await.unwrap();
    let id = format!("{ns}.motd");
    let edit = admin.panel(query(EDIT, None, &[("key", &id)])).await.unwrap();
    assert!(!form(&edit).fields[0].required, "a blank string value can be posted");
    let saved = admin.invoke(call(SAVE, &[("key", &id), ("value", "")], Some(motd_rev as u64))).await.unwrap();
    assert!(matches!(saved, InvokeOutcome::Ok { .. }), "{saved:?}");
    let (value, now) = stored(&pool, &ns, "motd").await.unwrap();
    assert_eq!(value, "");
    assert!(now > motd_rev);
    cleanup(&pool, &ns).await;
}

#[tokio::test]
async fn create_inserts_only_a_new_key() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let admin = ConfigAdmin::new(pool.clone());
    let id = format!("{ns}.fresh");

    let created = admin.invoke(call(CREATE, &[("key", &id), ("type", "float"), ("value", "3")], None)).await.unwrap();
    assert_eq!(created, InvokeOutcome::Ok { message: format!("Key {id} created"), reveal: None, redirect: None });
    let (value, rev) = stored(&pool, &ns, "fresh").await.unwrap();
    assert_eq!(value, "3.0");
    let events = changed_event_count(&pool, &ns).await;
    assert_eq!(events, 1, "the create goes through the trigger");

    let again = admin.invoke(call(CREATE, &[("key", &id), ("type", "float"), ("value", "9")], None)).await.unwrap();
    assert_eq!(again, InvokeOutcome::FieldErrors(BTreeMap::from([("key".to_string(), "already exists".to_string())])));
    assert_eq!(stored(&pool, &ns, "fresh").await, Some(("3.0".to_string(), rev)), "nothing is written");
    assert_eq!(changed_event_count(&pool, &ns).await, events);
    cleanup(&pool, &ns).await;
}


/// The `config.setting_history` rows of `(ns, key)`, oldest first: revision, value, operation,
/// writer.
async fn history(pool: &PgPool, ns: &str, key: &str) -> Vec<(i64, Option<String>, String, String)> {
    sqlx::query_as(
        "SELECT revision, value, operation, by FROM config.setting_history \
         WHERE namespace = $1 AND key = $2 ORDER BY revision",
    )
    .bind(ns)
    .bind(key)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Every write of a key through the module appends one history row in its own transaction:
/// after k writes the history holds k rows with strictly increasing revisions, the newest equal
/// to `config.settings`, written by the operator for an admin write and `system` for
/// `Service::set` and `set_tx`. A rolled-back `set_tx` leaves no row, and an admin write
/// that writes nothing (a stale save, a create of an existing key) appends none.
#[tokio::test]
async fn every_write_appends_one_history_row_with_its_writer() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Service::new(pool.clone());
    let admin = ConfigAdmin::new(pool.clone());
    let id = format!("{ns}.limit");

    let created = admin.invoke(call(CREATE, &[("key", &id), ("type", "int"), ("value", "1")], None)).await.unwrap();
    assert!(matches!(created, InvokeOutcome::Ok { .. }), "{created:?}");
    let (_, rev) = stored(&pool, &ns, "limit").await.unwrap();
    let saved = admin.invoke(call(SAVE, &[("key", &id), ("value", "2")], Some(rev as u64))).await.unwrap();
    assert!(matches!(saved, InvokeOutcome::Ok { .. }), "{saved:?}");
    let stale = admin.invoke(call(SAVE, &[("key", &id), ("value", "9")], Some(rev as u64))).await.unwrap();
    assert!(matches!(stale, InvokeOutcome::Conflict { .. }), "{stale:?}");
    let again = admin.invoke(call(CREATE, &[("key", &id), ("type", "int"), ("value", "9")], None)).await.unwrap();
    assert!(matches!(again, InvokeOutcome::FieldErrors(_)), "{again:?}");
    svc.set(&ns, "limit", "3").await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    svc.set_tx(&mut tx, &ns, "limit", "4").await.unwrap();
    tx.commit().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    svc.set_tx(&mut tx, &ns, "limit", "5").await.unwrap();
    tx.rollback().await.unwrap();

    let rows = history(&pool, &ns, "limit").await;
    let drawn: Vec<(Option<&str>, &str, &str)> =
        rows.iter().map(|(_, v, op, by)| (v.as_deref(), op.as_str(), by.as_str())).collect();
    assert_eq!(
        drawn,
        [
            (Some("1"), "insert", "tester"),
            (Some("2"), "update", "tester"),
            (Some("3"), "update", "system"),
            (Some("4"), "update", "system"),
        ]
    );
    assert!(rows.windows(2).all(|w| w[0].0 < w[1].0), "strictly increasing revisions: {rows:?}");
    assert_eq!(rows[0].0, rev, "the history row carries the revision the stamp trigger assigned");
    let (value, current) = stored(&pool, &ns, "limit").await.unwrap();
    assert_eq!((rows[3].0, rows[3].1.as_deref()), (current, Some(value.as_str())), "the newest is the stored row");

    let (role,): (String,) = sqlx::query_as("SELECT session_user::text").fetch_one(&pool).await.unwrap();
    sqlx::query("DELETE FROM config.settings WHERE namespace = $1 AND key = 'limit'")
        .bind(&ns)
        .execute(&pool)
        .await
        .unwrap();
    let rows = history(&pool, &ns, "limit").await;
    assert_eq!(rows.len(), 5);
    let (deleted_rev, value, op, by) = &rows[4];
    assert!(*deleted_rev > current, "a delete carries its own bump past the deleted row's revision");
    let drawn = (value.as_deref(), op.as_str(), by.as_str());
    assert_eq!(drawn, (None, "delete", role.as_str()), "an unset writer is the role");
    let (event_rev,): (i64,) = sqlx::query_as(
        "SELECT (payload->>'revision')::bigint FROM asyncevents.events \
         WHERE topic = 'config.changed' AND payload->>'namespace' = $1 AND payload->>'operation' = 'delete'",
    )
    .bind(&ns)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(*deleted_rev, event_rev, "the history and the event carry the same revision");
    cleanup(&pool, &ns).await;
}

/// The Revisions panel draws the newest `MAX_REVISIONS` history rows of the key, newest first,
/// and the Keys row's writer is the writer of the key's current revision.
#[tokio::test]
async fn revisions_panel_draws_the_newest_stored_revisions() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Service::new(pool.clone());
    let admin = ConfigAdmin::new(pool.clone());
    let id = format!("{ns}.limit");
    let writes = store::MAX_REVISIONS + 2;
    for i in 0..writes {
        svc.set(&ns, "limit", &i.to_string()).await.unwrap();
    }
    let (_, rev) = stored(&pool, &ns, "limit").await.unwrap();
    let saved = admin.invoke(call(SAVE, &[("key", &id), ("value", "99")], Some(rev as u64))).await.unwrap();
    assert!(matches!(saved, InvokeOutcome::Ok { .. }), "{saved:?}");

    let rows = history(&pool, &ns, "limit").await;
    assert_eq!(rows.len() as i64, store::MAX_REVISIONS, "the trigger trims the history to the panel's bound");
    let revisions = admin.panel(query(REVISIONS, None, &[("key", &id)])).await.unwrap();
    let entries = &timeline(&revisions).entries;
    let want: Vec<(String, String, Option<String>)> = rows
        .iter()
        .rev()
        .take(store::MAX_REVISIONS as usize)
        .map(|(r, v, _, by)| (format!("rev {r} by"), by.clone(), v.clone()))
        .collect();
    let drawn: Vec<(String, String, Option<String>)> =
        entries.iter().map(|e| (e.summary.clone(), e.badges[0].value.clone(), e.detail.clone())).collect();
    assert_eq!(drawn, want);
    assert_eq!((drawn[0].1.as_str(), drawn[0].2.as_deref()), ("tester", Some("99")));
    assert!(entries.iter().all(|e| e.at.ends_with('Z') && e.at.len() == 20), "RFC 3339 UTC");

    let keys = admin.panel(query(KEYS, Some(&id), &[])).await.unwrap();
    let writer = &table(&keys).rows[0].cells[4].more[0];
    assert_eq!((writer.kind, writer.value.as_str()), (CellKind::User, "tester"));
    cleanup(&pool, &ns).await;
}

/// The trigger keeps the newest `MAX_REVISIONS` history rows of a key and no more, a delete
/// entry counting like any other, and never touches another key's history; the panel still
/// draws at most that many.
#[tokio::test]
async fn history_keeps_only_the_newest_revisions_per_key() {
    let _guard = CONFIG_DB_TEST_GUARD.lock().await;
    let Some(pool) = test_pool().await else { return };
    let ns = unique_ns(&pool).await;
    let svc = Service::new(pool.clone());
    let admin = ConfigAdmin::new(pool.clone());
    let id = format!("{ns}.busy");
    let bound = store::MAX_REVISIONS as usize;
    svc.set(&ns, "quiet", "q").await.unwrap();
    let quiet_before = history(&pool, &ns, "quiet").await;
    for i in 0..(bound + 3) {
        svc.set(&ns, "busy", &i.to_string()).await.unwrap();
    }
    let rows = history(&pool, &ns, "busy").await;
    assert_eq!(rows.len(), bound);
    let values: Vec<String> = rows.iter().map(|r| r.1.clone().unwrap()).collect();
    let want: Vec<String> = (3..bound + 3).map(|i| i.to_string()).collect();
    assert_eq!(values, want, "the newest rows remain, oldest first");
    assert_eq!(history(&pool, &ns, "quiet").await, quiet_before, "another key's history is untouched");

    sqlx::query("DELETE FROM config.settings WHERE namespace = $1 AND key = 'busy'")
        .bind(&ns)
        .execute(&pool)
        .await
        .unwrap();
    let rows = history(&pool, &ns, "busy").await;
    assert_eq!(rows.len(), bound);
    assert_eq!(rows.last().unwrap().2, "delete", "a delete entry is trimmed like any other");
    assert_eq!(rows[0].1.as_deref(), Some("4"), "the delete pushed the oldest remaining row out");

    svc.set(&ns, "busy", "again").await.unwrap();
    let revisions = admin.panel(query(REVISIONS, None, &[("key", &id)])).await.unwrap();
    assert!(timeline(&revisions).entries.len() <= bound);
    cleanup(&pool, &ns).await;
}
