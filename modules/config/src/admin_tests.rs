//! The config provider: the manifest, the type rule, the Keys, Edit and Revisions bodies and
//! their FAKE writers and prior revisions; and, over a live Postgres, the store reads through
//! `Provider::panel` (prefix search, type filter, selection, the SQL-computed times).

use controlplane::{form_spec_errors, order, CellKind, DataTable, FormSpec, Layout, Point, Primitive, Role, Timeline};

use super::*;
use crate::tests::{cleanup, test_pool, unique_ns, CONFIG_DB_TEST_GUARD};
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

#[test]
fn manifest_declares_the_read_only_page() {
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
    assert!(page.head_actions.is_empty());
    let got: Vec<(&str, i32, &str)> =
        m.contributions.iter().map(|c| (c.id.as_str(), c.order, c.title.as_str())).collect();
    assert_eq!(got, vec![("keys", 10, "Keys"), ("edit", 20, "Edit"), ("revisions", 30, "Revisions")]);
    for c in &m.contributions {
        assert_eq!(c.point, Point::Page(PAGE.into()));
        assert_eq!(c.min_role, Role::Viewer);
        assert!(c.cache_ttl_s.is_none() && c.readonly.is_none());
    }
    assert!(m.actions.is_empty() && m.entity_kinds.is_empty() && m.search.is_empty());
    assert!(!m.writes);
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
        rows: vec![row("game", "hardcore", "true", 4), row("game", "limit", "42", 7), row("store", "rot", "[1]", 9)],
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
    assert_eq!((flag[4].more[0].kind, flag[4].more[0].value.as_str()), (CellKind::User, fake_by("game.hardcore")));
    let rot = &t.rows[2].cells;
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
        assert!(field.required && field.mono && !field.readonly);
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

fn selected(display_key: &str, value: &str, revision: i64) -> Selected {
    let (ns, key) = display_key.split_once('.').unwrap();
    Selected {
        row: row(ns, key, value, revision),
        at_days_back: (0..=store::MAX_DAYS_BACK).map(|d| format!("day-{d}")).collect(),
    }
}

#[test]
fn revisions_body_draws_the_current_then_fake_priors() {
    let body = revisions_body(&selected("game.limit", "42", 30));
    let tl = timeline(&body);
    // The timeline draws actor, title, verb, summary, then badges (primitives.html.tmpl
    // `timeline`): with only a summary and one badge an entry reads "rev N by <writer>".
    assert!(tl.date_only && !tl.live && tl.cursor_before.is_none());
    let current = &tl.entries[0];
    assert_eq!(current.at, "day-0");
    assert_eq!((current.verb.as_str(), current.summary.as_str()), ("", "rev 30 by"));
    assert!(current.actor.is_none() && current.title.is_none() && current.target.is_none());
    let writer: Vec<(CellKind, &str)> = current.badges.iter().map(|b| (b.kind, b.value.as_str())).collect();
    assert_eq!(writer, [(CellKind::User, fake_by("game.limit"))]);
    assert_eq!(current.detail.as_deref(), Some("42"));
    assert_eq!((current.icon.as_str(), current.tone), ("edit", Some(Tone::Accent)));

    let prior = fake_prior("game.limit", 30);
    assert_eq!(tl.entries.len(), 1 + prior.len());
    assert_eq!(prior.len(), 2);
    assert!(prior[0].0 < 30 && prior[1].0 < prior[0].0);
    assert!(prior[0].1 >= 1 && prior[1].1 > prior[0].1 && prior[1].1 <= store::MAX_DAYS_BACK as usize);
    for (entry, (revision, days)) in tl.entries[1..].iter().zip(&prior) {
        assert_eq!((entry.verb.as_str(), entry.summary.clone()), ("", format!("rev {revision} by")));
        assert!(entry.actor.is_none());
        assert_eq!(entry.at, format!("day-{days}"));
        assert_eq!(entry.detail.as_deref(), Some("42"));
        assert!(entry.tone.is_none());
        assert_eq!(entry.badges.len(), 1);
        assert_eq!(entry.badges[0].kind, CellKind::User);
        assert!(FAKE_LOGINS.contains(&entry.badges[0].value.as_str()));
    }
    assert_eq!(revisions_body(&selected("game.limit", "42", 30)), body, "stable across reloads");

    let first = revisions_body(&selected("game.limit", "42", 1));
    assert_eq!(timeline(&first).entries.len(), 1, "no prior revision below 1");
    let varied: std::collections::BTreeSet<Vec<(i64, usize)>> =
        (0..20).map(|i| fake_prior(&format!("ns.k{i}"), 100)).collect();
    assert!(varied.len() > 1);
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
    assert_eq!(entries[0].at, limit_at);
    assert_eq!(entries[0].summary, format!("rev {limit_rev} by"));
    assert!(entries[0].verb.is_empty() && entries[0].actor.is_none());
    assert_eq!(entries[0].detail.as_deref(), Some("42"));
    for entry in &entries[1..] {
        assert!(entry.at < limit_at, "a prior revision is dated before the current one: {}", entry.at);
    }

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
