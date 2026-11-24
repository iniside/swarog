//! The API keys provider: the manifest, the Keys and Roles bodies, the FAKE cells, the cursor,
//! the refusals decided before the store; and, over a live Postgres, the Keys pager, filters
//! and total and the Roles key counts through `Provider::panel`.

use std::collections::BTreeMap;
use std::sync::Arc;

use controlplane::{
    order, ActionPoint, Align, CellKind, ConfirmTone, DataTable, Emphasis, FieldKind, InvokeCall, InvokeOutcome, Layout,
    Link, PanelBody, PanelQuery, Point, PrepareCall, Primitive, Provider, ReasonReq, Role, SearchQuery, Then,
};
use opsapi::Status;
use sqlx::PgPool;

use super::*;
use crate::store::{KeyRow, RoleRow, Store};
use crate::store_tests::{cleanup, db_test_lock, test_pool, unique_name};

fn key(i: usize, revoked: bool) -> KeyRow {
    KeyRow {
        name: format!("key-{i:02}"),
        prefix: format!("ak_{i:04}"),
        role: "ops".to_string(),
        revision: 1,
        revoked,
        created: "2026-11-23".to_string(),
        created_us: 1_000_000 - i as i64,
        created_by: format!("op-{i:02}"),
    }
}

fn table(body: &PanelBody) -> &DataTable {
    match &body.primitive {
        Primitive::DataTable(t) => t,
        other => panic!("a DataTable, got {other:?}"),
    }
}

fn provider(pool: PgPool) -> ApiKeysAdmin {
    ApiKeysAdmin::new(Arc::new(Service { store: Store { pool } }))
}

/// A provider over a pool that cannot connect: every case it serves is decided before the store.
fn dead_provider() -> ApiKeysAdmin {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy("postgres://gamebackend:gamebackend@127.0.0.1:1/gamebackend?sslmode=disable")
        .unwrap();
    provider(pool)
}

fn query(contribution: &str, search: Option<&str>, cursor: Option<&str>, params: &[(&str, &str)]) -> PanelQuery {
    PanelQuery {
        module: "apikeys".into(),
        contribution: contribution.into(),
        entity: None,
        search: search.map(String::from),
        cursor: cursor.map(String::from),
        before: None,
        queue_state: None,
        range: None,
        params: params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        refs: Vec::new(),
    }
}

fn prepare_call(action: &str, key: Option<&str>) -> PrepareCall {
    PrepareCall {
        module: "apikeys".into(),
        action: action.into(),
        entity: key.map(|id| EntityRef { kind: KEY_KIND.into(), id: id.into() }),
        item: None,
        operator: "tester".into(),
    }
}

fn create_input(name: &str, role: &str, expires: Option<&str>) -> BTreeMap<String, String> {
    let mut input = BTreeMap::from([("name".to_string(), name.to_string()), ("role".to_string(), role.to_string())]);
    if let Some(expires) = expires {
        input.insert("expires".to_string(), expires.to_string());
    }
    input
}

/// A call bound to the key `name`.
fn key_call(action: &str, name: &str, input: BTreeMap<String, String>, revision: Option<i64>) -> InvokeCall {
    InvokeCall {
        entity: Some(EntityRef { kind: KEY_KIND.into(), id: name.into() }),
        expected_revision: revision.map(|r| r as u64),
        ..call(action, input)
    }
}

fn call(action: &str, input: BTreeMap<String, String>) -> InvokeCall {
    InvokeCall {
        module: "apikeys".into(),
        action: action.into(),
        entity: Some(EntityRef { kind: KEY_KIND.into(), id: "k".into() }),
        input,
        expected_revision: None,
        reason: None,
        operator: "tester".into(),
        item: None,
        request_id: None,
    }
}

#[test]
fn manifest_declares_the_page_its_two_panels_kinds_search_and_row_actions() {
    let m = manifest();
    assert_eq!(m.module, "apikeys");
    assert!(m.writes);
    assert_eq!(m.entries.len(), 1);
    let nav = &m.entries[0];
    assert_eq!((nav.section.as_str(), nav.id.as_str(), nav.label.as_str()), ("platform", "apikeys", "API keys"));
    assert_eq!((nav.icon.as_str(), nav.page.as_str()), ("key", "keys"));
    assert_eq!(nav.order, order::platform_nav::APIKEYS);
    assert!(nav.counter.is_none());

    let page = &m.pages[0];
    assert_eq!((page.id.as_str(), page.title.as_str()), ("keys", "API keys"));
    assert_eq!(page.subtitle, "Machine credentials for game servers, CI and integrations.");
    assert_eq!(page.head_actions, vec!["key.create".to_string()]);
    assert_eq!(page.layout, Layout::Stack(vec!["keys".into(), "roles".into()]));
    assert_eq!(page.params, vec!["status".to_string(), "role".to_string()]);

    let panels: Vec<_> = m.contributions.iter().map(|c| (c.id.as_str(), c.order, c.title.as_str())).collect();
    assert_eq!(panels, vec![("keys", 10, "Keys"), ("roles", 20, "Roles")]);
    for c in &m.contributions {
        assert_eq!(c.point, Point::Page("keys".into()));
        assert_eq!(c.min_role, Role::Viewer);
        assert_eq!(c.cache_ttl_s, None);
    }

    let kinds: Vec<_> = m.entity_kinds.iter().map(|k| (k.kind.as_str(), k.label.as_str(), k.icon.as_str())).collect();
    assert_eq!(kinds, vec![("apikey", "API key", "key"), ("apirole", "API role", "shield")]);
    let search: Vec<_> = m.search.iter().map(|s| (s.kind.as_str(), s.label.as_str(), s.icon.as_str())).collect();
    assert_eq!(search, vec![("apikey", "API keys", "key")]);
    assert_eq!(m.search[0].deadline_ms, controlplane::DEFAULT_SEARCH_DEADLINE_MS);

    let rotate = m.actions.iter().find(|a| a.id == "key.rotate").expect("key.rotate");
    assert_eq!(rotate.point, ActionPoint::RowAction("apikey".into()));
    assert_eq!((rotate.label.as_str(), rotate.icon.as_str(), rotate.order), ("Rotate secret…", "refresh", 10));
    assert!(!rotate.implemented);
    assert_eq!(rotate.min_role, Role::Operator);
    assert_eq!(rotate.then, Then::Invoke);


    let create = m.actions.iter().find(|a| a.id == "key.create").expect("key.create");
    assert_eq!(create.point, ActionPoint::PageHead("keys".into()));
    assert_eq!((create.label.as_str(), create.icon.as_str(), create.order), ("Create key", "plus", 10));
    assert_eq!(create.emphasis, Emphasis::Primary);
    assert!(create.prepare && create.implemented && !create.danger);
    assert!(matches!(create.then, Then::Form(_)));

    let change = m.actions.iter().find(|a| a.id == "key.change-role").expect("key.change-role");
    assert_eq!(change.point, ActionPoint::RowAction("apikey".into()));
    assert_eq!((change.label.as_str(), change.icon.as_str(), change.order), ("Change role…", "shield", 20));
    assert!(change.prepare && change.implemented && !change.danger);
    assert!(matches!(change.then, Then::Form(_)));

    let revoke = m.actions.iter().find(|a| a.id == "key.revoke").expect("key.revoke");
    assert_eq!(revoke.point, ActionPoint::RowAction("apikey".into()));
    assert_eq!((revoke.label.as_str(), revoke.icon.as_str(), revoke.order), ("Revoke…", "ban", 30));
    assert!(revoke.prepare && revoke.implemented && revoke.danger);
    assert!(matches!(revoke.then, Then::Confirm(_)));
    for a in [create, change, revoke] {
        assert_eq!(a.min_role, Role::Operator, "{}", a.id);
    }

    let role_create = m.actions.iter().find(|a| a.id == "role.create").expect("role.create");
    assert_eq!(role_create.point, ActionPoint::PanelTool("roles".into()));
    assert_eq!((role_create.label.as_str(), role_create.icon.as_str(), role_create.order), ("Role", "plus", 10));
    assert_eq!(role_create.emphasis, Emphasis::Default);
    assert!(role_create.prepare && role_create.implemented && role_create.when_empty && !role_create.danger);
    assert!(matches!(role_create.then, Then::Form(_)));
    let role_edit = m.actions.iter().find(|a| a.id == "role.edit").expect("role.edit");
    assert_eq!(role_edit.point, ActionPoint::RowAction("apirole".into()));
    assert_eq!((role_edit.label.as_str(), role_edit.icon.as_str(), role_edit.order), ("Edit policy…", "edit", 10));
    assert!(role_edit.prepare && role_edit.implemented && !role_edit.danger);
    assert!(matches!(role_edit.then, Then::Form(_)));
    let role_delete = m.actions.iter().find(|a| a.id == "role.delete").expect("role.delete");
    assert_eq!(role_delete.point, ActionPoint::RowAction("apirole".into()));
    assert_eq!((role_delete.label.as_str(), role_delete.icon.as_str(), role_delete.order), ("Delete role…", "trash", 20));
    assert!(role_delete.prepare && role_delete.implemented && role_delete.danger);
    assert!(matches!(role_delete.then, Then::Confirm(_)));
    for a in [role_create, role_edit, role_delete] {
        assert_eq!(a.min_role, Role::Operator, "{}", a.id);
        assert_eq!(a.emphasis, Emphasis::Default, "{}", a.id);
    }
    assert_eq!(m.actions.len(), 7);
}

fn role_row(name: &str, policy: &str) -> RoleRow {
    RoleRow { name: name.into(), policy: policy.into(), revision: 1, keys: 0 }
}

#[test]
fn the_create_form_offers_the_stored_roles_and_only_never_as_expiry() {
    let form = create_form(&[role_row("ci", "match.report, accounts.login"), role_row("ops", "full")]);
    assert_eq!(form.title.as_deref(), Some("Create API key"));
    assert_eq!(form.note.as_deref(), Some("The secret is shown once after creation."));
    assert_eq!(form.submit_label, "Create key");
    assert_eq!(form.revision, None);
    let keys: Vec<_> = form.fields.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, vec!["name", "role", "expires"]);
    let name = &form.fields[0];
    assert_eq!(name.kind, FieldKind::String);
    assert!(name.required && name.mono);
    assert_eq!(name.placeholder.as_deref(), Some("horde-agents-2"));
    assert_eq!(name.help.as_deref(), Some("Used in audit entries."));
    let role = &form.fields[1];
    assert_eq!(role.kind, FieldKind::Enum(vec!["ci".into(), "ops".into()]));
    assert_eq!(role.default.as_deref(), Some("ci"));
    assert_eq!(role.help.as_deref(), Some("match.report, accounts.login"));
    let expires = &form.fields[2];
    assert_eq!(expires.kind, FieldKind::Enum(vec!["Never".into()]));
    assert_eq!(expires.default.as_deref(), Some("Never"));
    assert!(controlplane::form_spec_errors(&form).is_empty());
}

#[test]
fn the_change_role_form_defaults_to_the_current_role_at_the_key_revision() {
    let k = KeyRow { revision: 7, ..key(1, false) };
    let form = change_role_form(&[role_row("ci", "full"), role_row("ops", "full")], Some(&k));
    assert_eq!(form.revision, Some(7));
    assert_eq!(form.submit_label, "Change role");
    assert_eq!(form.fields.len(), 1);
    assert_eq!(form.fields[0].key, "role");
    assert_eq!(form.fields[0].kind, FieldKind::Enum(vec!["ci".into(), "ops".into()]));
    assert_eq!(form.fields[0].default.as_deref(), Some("ops"));
    assert!(controlplane::form_spec_errors(&form).is_empty());
}

#[test]
fn the_revoke_confirm_names_the_key_its_impact_and_asks_for_its_name() {
    let k = key(3, false);
    let c = revoke_confirm_for(&k);
    assert_eq!(c.title, "Revoke key-03?");
    assert_eq!(c.body, "Requests signed with this key fail immediately on every process.");
    assert_eq!(c.tone, ConfirmTone::Danger);
    assert_eq!(c.reason, ReasonReq::Optional);
    assert_eq!(c.reason_hint.as_deref(), Some("e.g. leaked in CI log"));
    assert_eq!(c.type_to_confirm.as_deref(), Some("key-03"));
    assert_eq!(c.confirm_label, "Revoke key");
    let labels: Vec<_> = c.impact.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels, vec!["Key"]);
    let id = &c.impact[0].1;
    assert_eq!((id.kind, id.value.as_str(), id.row), (CellKind::Mono, "ak_0003…", true));
    assert_eq!(id.more.len(), 1);
    assert_eq!((id.more[0].kind, id.more[0].value.as_str()), (CellKind::Text, "· ops"));
}

#[test]
fn the_role_form_offers_full_the_catalog_and_in_use_methods_and_creates_by_name() {
    let form = role_form(&[role_row("ci", "match.report, zz.custom"), role_row("ops", "full")], None);
    let keys: Vec<_> = form.fields.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, vec!["name", "methods", "extra_methods"]);
    let FieldKind::Multi(options) = &form.fields[1].kind else { panic!("a Multi") };
    let mut want = vec!["full".to_string()];
    want.extend(opscatalog::OPERATIONS.iter().map(|op| op.method.to_string()));
    want.push("zz.custom".to_string());
    assert_eq!(options, &want, "full, the catalog, then the in-use methods it lacks, once each");
    assert_eq!(form.fields[1].label, "Allowed methods");
    assert!(form.fields[0].required && !form.fields[1].required && !form.fields[2].required);
    assert!(!form.fields[0].readonly && form.fields[0].default.is_none() && form.fields[1].default.is_none());
    assert_eq!(form.fields[2].label, "Additional methods");
    assert_eq!(form.fields[2].kind, FieldKind::String);
    assert_eq!((form.submit_label.as_str(), form.title.as_deref()), ("Create role", Some("Create role")));
    assert_eq!(form.revision, None);
}

#[test]
fn the_edit_form_pre_checks_the_stored_methods_at_the_role_revision() {
    let ci = RoleRow { name: "ci".into(), policy: "match.report, zz.custom".into(), revision: 7, keys: 2 };
    let form = role_form(std::slice::from_ref(&ci), Some(&ci));
    assert!(form.fields[0].readonly);
    assert_eq!(form.fields[0].default.as_deref(), Some("ci"));
    let picked = controlplane::decode_field(&form.fields[1].kind, form.fields[1].default.as_deref().unwrap());
    assert_eq!(picked, Ok(controlplane::FieldValue::Multi(vec!["match.report".into(), "zz.custom".into()])));
    assert_eq!(form.fields[2].default, None);
    assert_eq!(form.revision, Some(7));
    assert_eq!((form.submit_label.as_str(), form.title), ("Save policy", None));
    let full = role_row("ops", "full");
    let form = role_form(std::slice::from_ref(&full), Some(&full));
    assert_eq!(form.fields[1].default.as_deref(), Some(r#"["full"]"#));
}

fn policy_input(picked: &[&str], typed: Option<&str>) -> BTreeMap<String, String> {
    let mut input = BTreeMap::new();
    if !picked.is_empty() {
        input.insert("methods".to_string(), serde_json::to_string(picked).unwrap());
    }
    if let Some(typed) = typed {
        input.insert("extra_methods".to_string(), typed.to_string());
    }
    input
}

#[test]
fn a_role_policy_is_full_or_the_union_of_picked_and_typed_methods() {
    let ok = |picked: &[&str], typed| compose_policy(&policy_input(picked, typed)).unwrap();
    assert_eq!(ok(&["full", "match.report"], None), "full");
    assert_eq!(ok(&["match.report"], Some(" full ")), "full");
    assert_eq!(ok(&["match.report", "accounts.login"], Some("x.y, match.report,, x.y ")), "match.report,accounts.login,x.y");
    assert_eq!(ok(&[], Some("x.y")), "x.y");
    for (input, why) in [
        (policy_input(&[], None), "nothing"),
        (policy_input(&[], Some(" , ")), "only blanks"),
        (BTreeMap::from([("methods".to_string(), "match.report".to_string())]), "not a JSON array"),
        (policy_input(&[], Some(&"m".repeat(crate::store::MAX_POLICY_BYTES + 1))), "over the cap"),
    ] {
        assert!(compose_policy(&input).is_err(), "{why}");
    }
}

#[test]
fn the_delete_role_confirm_is_a_danger_with_no_reason_and_names_the_role() {
    let confirm = role_delete_confirm_for(&role_row("ci", "full"));
    assert_eq!(confirm.title, "Delete role ci?");
    assert_eq!(confirm.body, ROLE_DELETE_BODY);
    assert_eq!(confirm.tone, ConfirmTone::Danger);
    assert_eq!(confirm.reason, ReasonReq::None);
    assert_eq!(confirm.confirm_label, "Delete role");
    assert_eq!((confirm.type_to_confirm, confirm.impact.len()), (None, 0));
}

#[test]
fn a_key_row_draws_name_prefix_role_created_and_status() {
    let body = keys_body(vec![key(1, false), key(4, true)], vec!["ops".into()], Some(2), 1);
    let t = table(&body);
    let labels: Vec<_> = t.columns.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["Name", "Key", "Role", "Created", "Status"]);
    let row = &t.rows[0];
    assert_eq!(row.entity, Some(EntityRef { kind: "apikey".into(), id: "key-01".into() }));
    let c = &row.cells;
    assert_eq!((c[0].kind, c[0].value.as_str()), (CellKind::Strong, "key-01"));
    assert_eq!((c[1].kind, c[1].value.as_str()), (CellKind::MonoMuted, "ak_0001…"));
    assert_eq!((c[2].kind, c[2].value.as_str()), (CellKind::Ref, "ops"));
    assert_eq!((c[3].kind, c[3].value.as_str(), c[3].row), (CellKind::Dim, "2026-11-23", true));
    assert_eq!(c[3].more.len(), 1);
    assert_eq!((c[3].more[0].kind, c[3].more[0].value.as_str()), (CellKind::User, "op-01"));
    assert_eq!((c[4].kind, c[4].value.as_str(), c[4].tone), (CellKind::Badge, "active", None));
    assert_eq!(t.rows[1].cells[4].value, "revoked");
    assert_eq!(body.meta.as_deref(), Some("1 active"));
    assert_eq!(t.search_placeholder.as_deref(), Some("Name or key prefix"));
    assert_eq!(t.item_noun, Some(controlplane::Noun::new("key", "keys")));
    let filters: Vec<_> = t.filters.iter().map(|f| (f.key.as_str(), f.kind.clone())).collect();
    assert_eq!(
        filters,
        vec![
            ("status", controlplane::FilterKind::Select(vec!["active".into(), "revoked".into()])),
            ("role", controlplane::FilterKind::Select(vec!["ops".into()])),
        ]
    );
}

#[test]
fn a_page_of_25_has_no_cursor_and_a_26th_row_sets_it_to_the_25th() {
    let full: Vec<KeyRow> = (0..PAGE_SIZE).map(|i| key(i, false)).collect();
    let body = keys_body(full, Vec::new(), Some(25), 25);
    let t = table(&body);
    assert_eq!(t.rows.len(), 25);
    assert_eq!(t.page.next_cursor, None);
    assert_eq!((t.page.size, t.page.total), (25, Some(25)));

    let over: Vec<KeyRow> = (0..=PAGE_SIZE).map(|i| key(i, false)).collect();
    let body = keys_body(over, Vec::new(), None, 26);
    let t = table(&body);
    assert_eq!(t.rows.len(), 25);
    assert_eq!(t.page.next_cursor, Some(cursor_of(&key(24, false))));
    assert_eq!(t.page.total, None);
}

#[test]
fn an_empty_keys_table_is_still_the_table_with_a_zero_meta() {
    let body = keys_body(Vec::new(), Vec::new(), Some(0), 0);
    assert!(table(&body).rows.is_empty());
    assert_eq!(body.meta.as_deref(), Some("0 active"));
}

#[test]
fn the_cursor_round_trips_and_a_malformed_one_is_invalid() {
    let row = KeyRow { name: "ci.deploy".into(), ..key(1, false) };
    let raw = cursor_of(&row);
    assert_eq!(parse_cursor(&raw).unwrap(), (row.created_us, "ci.deploy"));
    let long = format!("1.{}", "n".repeat(MAX_NAME_BYTES + 1));
    let at_cap = format!("1.{}", "n".repeat(MAX_NAME_BYTES));
    assert!(parse_cursor(&at_cap).is_ok());
    let over_micros = format!("{}.n", (1i64 << 53) + 1);
    for bad in ["", "1", ".n", "1.", "x1.n", "-1.n", long.as_str(), over_micros.as_str()] {
        assert_eq!(parse_cursor(bad).unwrap_err().status, Status::Invalid, "{bad:?}");
    }
}

#[test]
fn roles_draw_one_tag_per_method_or_full_a_right_aligned_count_meta_and_foot() {
    let rows = vec![
        RoleRow { name: "ci".into(), policy: "leaderboard.topScores, match.report".into(), revision: 1, keys: 0 },
        RoleRow { name: "server".into(), policy: "full".into(), revision: 1, keys: 3 },
    ];
    let body = roles_body(&rows);
    let t = table(&body);
    let labels: Vec<_> = t.columns.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["Role", "Scopes", "Keys"]);
    assert!(t.columns[1].wrap);
    assert_eq!(t.columns[2].align, Align::Right);
    let ci = &t.rows[0];
    assert_eq!(ci.entity, Some(EntityRef { kind: "apirole".into(), id: "ci".into() }));
    assert_eq!((ci.cells[0].kind, ci.cells[0].value.as_str()), (CellKind::Strong, "ci"));
    let scopes = &ci.cells[1];
    assert_eq!((scopes.kind, scopes.value.as_str()), (CellKind::Ref, "leaderboard.topScores"));
    assert_eq!(scopes.more.iter().map(|c| (c.kind, c.value.as_str())).collect::<Vec<_>>(), vec![(CellKind::Ref, "match.report")]);
    assert_eq!((ci.cells[2].kind, ci.cells[2].value.as_str()), (CellKind::Number, "0"));
    let full = &t.rows[1].cells[1];
    assert_eq!((full.value.as_str(), full.more.len()), ("full", 0));
    assert_eq!(t.rows[1].cells[2].value, "3");
    assert_eq!((t.page.next_cursor.clone(), t.page.total), (None, None));
    assert_eq!(body.meta.as_deref(), Some("2"));
    assert_eq!(body.foot.as_deref(), Some("Policies list the op methods modules serve (e.g. leaderboard.topScores)."));
    assert_eq!(roles_body(&[]).meta.as_deref(), Some("0"));
}

#[test]
fn a_search_hit_is_the_name_the_prefix_and_role_and_the_status() {
    let hit = search_hit(&key(3, true));
    assert_eq!((hit.title.as_str(), hit.subtitle.as_str()), ("key-03", "ak_0003… · role ops"));
    assert_eq!(hit.badge.as_deref(), Some("revoked"));
    assert_eq!(hit.link, Link::Page { module: "apikeys".into(), page: "keys".into(), params: BTreeMap::new() });
}

#[test]
fn the_conformance_probe_refuses_one_byte_over_the_input_cap_and_no_less() {
    let cap = crate::conformance::CP_INVOKE_INPUT_VALUE_BYTES;
    assert!(!crate::conformance::conformance_cp_invoke_input_rejected(cap), "at the cap the create reaches the store");
    assert!(crate::conformance::conformance_cp_invoke_input_rejected(cap + 1));
}

#[tokio::test]
async fn refusals_are_decided_before_the_store() {
    let p = dead_provider();
    let e = p.invoke(call("key.rotate", BTreeMap::new())).await.unwrap_err();
    assert_eq!(e.status, Status::Forbidden);
    let e = p.invoke(call("key.nope", BTreeMap::new())).await.unwrap_err();
    assert_eq!(e.status, Status::NotFound);
    let e = p.invoke(InvokeCall { entity: None, ..call("key.revoke", BTreeMap::new()) }).await.unwrap_err();
    assert_eq!(e.status, Status::Invalid);
    assert!(e.msg.contains("needs an API key"), "{e:?}");
    let role_entity = Some(EntityRef { kind: ROLE_KIND.into(), id: "k".into() });
    let e = p.invoke(InvokeCall { entity: role_entity.clone(), ..call("key.change-role", BTreeMap::new()) }).await.unwrap_err();
    assert_eq!(e.status, Status::Invalid);
    assert!(e.msg.contains("needs an API key"), "{e:?}");
    let no_role = p.invoke(key_call("key.change-role", "k", BTreeMap::new(), Some(1))).await.unwrap();
    assert_eq!(no_role, InvokeOutcome::FieldErrors(BTreeMap::from([("role".into(), "required".into())])));
    let to_ops = BTreeMap::from([("role".to_string(), "ops".to_string())]);
    let e = p.invoke(key_call("key.change-role", "k", to_ops, None)).await.unwrap_err();
    assert_eq!(e.status, Status::Invalid, "no revision is refused before the key is read: {e:?}");
    assert!(e.msg.contains("changing a role needs the form's revision"), "{e:?}");
    let no_role = p.invoke(call("key.create", create_input("fine", "", Some("Never")))).await.unwrap();
    assert_eq!(no_role, InvokeOutcome::FieldErrors(BTreeMap::from([("role".into(), "required".into())])));

    let role_call = |action: &str, input| InvokeCall { entity: role_entity.clone(), ..call(action, input) };
    for action in ["role.edit", "role.delete"] {
        // No methods and a revision: only the entity check can answer before `compose_policy`.
        let bound_to_key = InvokeCall { expected_revision: Some(1), ..call(action, BTreeMap::new()) };
        let e = p.invoke(bound_to_key).await.unwrap_err();
        assert_eq!(e.status, Status::Invalid, "{action} bound to a key");
        assert!(e.msg.contains("needs an API role"), "{action} bound to a key: {e:?}");
        let e = p.prepare(prepare_call(action, Some("k"))).await.unwrap_err();
        assert_eq!(e.status, Status::Invalid, "{action} prepared for a key");
        assert!(e.msg.contains("needs an API role"), "{action} prepared for a key: {e:?}");
    }
    let e = p.invoke(role_call("role.edit", policy_input(&["full"], None))).await.unwrap_err();
    assert_eq!(e.status, Status::Invalid, "an edit with no revision: {e:?}");
    assert!(e.msg.contains("editing a policy needs the form's revision"), "{e:?}");
    match p.invoke(InvokeCall { expected_revision: Some(1), ..role_call("role.edit", BTreeMap::new()) }).await {
        Ok(InvokeOutcome::FieldErrors(errors)) => assert!(errors.contains_key("methods"), "{errors:?}"),
        other => panic!("an edit picking nothing: FieldErrors on methods, got {other:?}"),
    }
    let mut bad_name = policy_input(&["full"], None);
    bad_name.insert("name".into(), " padded".into());
    for (input, field) in [(bad_name, "name"), (BTreeMap::from([("name".to_string(), "ok".to_string())]), "methods")] {
        match p.invoke(call("role.create", input)).await {
            Ok(InvokeOutcome::FieldErrors(errors)) => assert!(errors.contains_key(field), "{errors:?}"),
            other => panic!("FieldErrors on {field}, got {other:?}"),
        }
    }
    let e = p.prepare(prepare_call("key.nope", None)).await.unwrap_err();
    assert_eq!(e.status, Status::NotFound);
    let e = p.prepare(prepare_call("key.revoke", None)).await.unwrap_err();
    assert_eq!(e.status, Status::Invalid);
    assert!(e.msg.contains("needs an API key"), "{e:?}");
    for (name, expires, field) in [
        ("", "Never", "name"),
        (" padded", "Never", "name"),
        (&*"n".repeat(MAX_NAME_BYTES + 1), "Never", "name"),
        ("fine", "30 days", "expires"),
    ] {
        let input = create_input(name, "ops", Some(expires));
        match p.invoke(call("key.create", input)).await {
            Ok(InvokeOutcome::FieldErrors(errors)) => assert!(errors.contains_key(field), "{name:?}: {errors:?}"),
            other => panic!("{name:?} {expires:?}: FieldErrors on {field}, got {other:?}"),
        }
    }
    match p.invoke(call("key.create", create_input("fine", "ops", None))).await {
        Ok(InvokeOutcome::FieldErrors(errors)) => assert!(errors.contains_key("expires")),
        other => panic!("no expiry: FieldErrors on expires, got {other:?}"),
    }

    let big = BTreeMap::from([("x".to_string(), "v".repeat(controlplane::MAX_INPUT_VALUE_BYTES + 1))]);
    let e = p.invoke(call("key.rotate", big)).await.unwrap_err();
    assert_eq!(e.status, Status::Invalid);
    assert!(e.msg.contains("an input value is longer than"), "{e:?}");

    let e = p.panel(query("nope", None, None, &[])).await.unwrap_err();
    assert_eq!(e.status, Status::NotFound);
    let long = "n".repeat(MAX_NEEDLE_BYTES + 1);
    let e = p.panel(query("keys", Some(&long), None, &[])).await.unwrap_err();
    assert_eq!(e.status, Status::Invalid);
    assert!(e.msg.contains("the search is longer than"), "{e:?}");
    let e = p.panel(query("keys", None, None, &[("status", "lost")])).await.unwrap_err();
    assert_eq!(e.status, Status::Invalid);
    assert!(e.msg.contains("unknown status"), "{e:?}");

    for (kind, q) in [("apirole", "x"), ("apikey", "  "), ("apikey", long.as_str())] {
        let req = SearchQuery { module: "apikeys".into(), kind: kind.into(), q: q.into(), limit: 10 };
        assert!(p.search(req).await.unwrap().is_empty(), "{kind} {q:?}");
    }
}

#[tokio::test]
async fn keys_page_by_25_narrow_by_filters_and_drop_the_total_and_roles_count_keys() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let base = unique_name(&pool).await;
    let store = Store { pool: pool.clone() };
    let (role_a, role_b) = (format!("{base}-a"), format!("{base}-b"));
    store.create_role(&role_a, "full").await.unwrap();
    store.create_role(&role_b, "match.report").await.unwrap();
    for i in 0..PAGE_SIZE {
        store.create_key(&format!("{base}-k{i:02}"), &role_a, "tester").await.unwrap();
    }
    let revoked = format!("{base}-r");
    store.create_key(&revoked, &role_b, "tester").await.unwrap();
    store.revoke_key(&revoked).await.unwrap();
    let p = provider(pool.clone());

    let first = p.panel(query("keys", Some(&base), None, &[])).await.unwrap();
    let t = table(&first);
    assert_eq!(t.rows.len(), 25);
    assert_eq!(t.page.total, None, "a search narrows, so no total");
    let cursor = t.page.next_cursor.clone().expect("a 26th key exists");
    let second = p.panel(query("keys", Some(&base), Some(&cursor), &[])).await.unwrap();
    let t2 = table(&second);
    assert_eq!(t2.rows.len(), 1);
    assert_eq!(t2.page.next_cursor, None);
    let mut names: Vec<_> = t.rows.iter().chain(&t2.rows).map(|r| r.cells[0].value.clone()).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 26, "every key once");

    let only_revoked = p.panel(query("keys", Some(&base), None, &[("status", "revoked")])).await.unwrap();
    let rows: Vec<_> = table(&only_revoked).rows.iter().map(|r| r.cells[0].value.clone()).collect();
    assert_eq!(rows, vec![revoked.clone()]);
    let by_role = p.panel(query("keys", None, None, &[("role", role_b.as_str())])).await.unwrap();
    let t = table(&by_role);
    assert_eq!(t.rows.iter().map(|r| r.cells[0].value.clone()).collect::<Vec<_>>(), vec![revoked.clone()]);
    assert_eq!(t.page.total, None, "a filter narrows, so no total");
    let gone = p.panel(query("keys", None, None, &[("role", &format!("{base}-none"))])).await.unwrap();
    let t = table(&gone);
    assert!(t.rows.is_empty(), "a role that no longer exists matches no keys");
    assert_eq!(t.page.total, None, "a filter narrows, so no total");
    let long = "r".repeat(MAX_NAME_BYTES + 1);
    let e = p.panel(query("keys", None, None, &[("role", long.as_str())])).await.unwrap_err();
    assert_eq!(e.status, Status::Invalid);

    let all = p.panel(query("keys", None, None, &[])).await.unwrap();
    let (total, active) = store.key_counts().await.unwrap();
    assert_eq!(table(&all).page.total, Some(total as u64));
    assert_eq!(all.meta, Some(format!("{active} active")));
    let filters = &table(&all).filters;
    let controlplane::FilterKind::Select(roles) = &filters[1].kind else { panic!("role select") };
    assert!(roles.contains(&role_a) && roles.contains(&role_b));

    let roles = p.panel(query("roles", None, None, &[])).await.unwrap();
    let counts: Vec<_> = table(&roles)
        .rows
        .iter()
        .filter(|r| r.cells[0].value.starts_with(&base))
        .map(|r| (r.cells[0].value.clone(), r.cells[2].value.clone()))
        .collect();
    assert_eq!(counts, vec![(role_a.clone(), "25".to_string()), (role_b.clone(), "1".to_string())]);

    let hits = p
        .search(SearchQuery { module: "apikeys".into(), kind: "apikey".into(), q: revoked.clone(), limit: 50 })
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].badge.as_deref(), Some("revoked"));
    let capped = p
        .search(SearchQuery { module: "apikeys".into(), kind: "apikey".into(), q: base.clone(), limit: 50 })
        .await
        .unwrap();
    assert_eq!(capped.len(), 25);

    cleanup(&pool, &base).await;
}

#[tokio::test]
async fn create_change_role_and_revoke_a_key_through_the_provider() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let base = unique_name(&pool).await;
    let store = Store { pool: pool.clone() };
    let (role_a, role_b) = (format!("{base}-a"), format!("{base}-b"));
    store.create_role(&role_a, "full").await.unwrap();
    store.create_role(&role_b, "match.report").await.unwrap();
    let p = provider(pool.clone());
    let name = format!("{base}-k");

    let Then::Form(form) = p.prepare(prepare_call("key.create", None)).await.unwrap() else { panic!("a form") };
    let FieldKind::Enum(roles) = &form.fields[1].kind else { panic!("a role enum") };
    assert!(roles.contains(&role_a) && roles.contains(&role_b));

    let created = p.invoke(call("key.create", create_input(&name, &role_a, Some("Never")))).await.unwrap();
    let InvokeOutcome::Ok { message, reveal: Some(reveal), redirect: None } = created else { panic!("{created:?}") };
    assert_eq!(message, "");
    assert_eq!(reveal.label, format!("Key created — {name}"));
    assert!(reveal.value.starts_with("ak_"));
    assert_eq!(reveal.hint, None);
    assert_eq!(
        reveal.extra.as_deref(),
        Some(&*format!("Send it as the X-Api-Key header (HTTP) or the api_key field (socket). Role {role_a}."))
    );
    assert_eq!(store.lookup(&reveal.value).await.unwrap().unwrap().name, name);
    assert_eq!(store.key(&name).await.unwrap().unwrap().created_by, "tester", "the creator is the calling operator");

    let again = p.invoke(call("key.create", create_input(&name, &role_b, Some("Never")))).await.unwrap();
    let InvokeOutcome::FieldErrors(errors) = again else { panic!("{again:?}") };
    assert_eq!(errors.get("name").map(String::as_str), Some("a key with this name exists"));
    let other = format!("{base}-k2");
    let e = p.invoke(call("key.create", create_input(&other, &format!("{base}-none"), Some("Never")))).await.unwrap_err();
    assert_eq!(e.status, Status::Conflict);
    assert!(store.key(&other).await.unwrap().is_none(), "no key written");

    let Then::Form(form) = p.prepare(prepare_call("key.change-role", Some(&name))).await.unwrap() else {
        panic!("a form")
    };
    let rev = store.key(&name).await.unwrap().unwrap().revision;
    assert_eq!(form.revision, Some(rev as u64));
    assert_eq!(form.fields[0].default.as_deref(), Some(role_a.as_str()));
    let to_b = || BTreeMap::from([("role".to_string(), role_b.clone())]);
    let to_a = || BTreeMap::from([("role".to_string(), role_a.clone())]);
    let gone_role = BTreeMap::from([("role".to_string(), format!("{base}-none"))]);
    let e = p.invoke(key_call("key.change-role", &name, gone_role, Some(rev))).await.unwrap_err();
    assert_eq!(e.status, Status::Conflict, "a role deleted after the form opened: {e:?}");
    let changed = p.invoke(key_call("key.change-role", &name, to_b(), Some(rev))).await.unwrap();
    assert!(matches!(changed, InvokeOutcome::Ok { .. }), "{changed:?}");
    let now = store.key(&name).await.unwrap().unwrap();
    assert_eq!(now.role, role_b);
    let stale = p.invoke(key_call("key.change-role", &name, to_a(), Some(rev))).await.unwrap();
    assert_eq!(stale, InvokeOutcome::Conflict { current_revision: now.revision as u64 });
    let same = p.invoke(key_call("key.change-role", &name, to_b(), Some(now.revision))).await.unwrap();
    assert_eq!(same, InvokeOutcome::NoOp { message: format!("Key {name} already has role {role_b}") });
    let missing = format!("{base}-missing");
    let e = p.invoke(key_call("key.change-role", &missing, to_a(), Some(1))).await.unwrap_err();
    assert_eq!(e.status, Status::NotFound);
    let e = p.prepare(prepare_call("key.change-role", Some(&missing))).await.unwrap_err();
    assert_eq!(e.status, Status::NotFound);

    let Then::Confirm(confirm) = p.prepare(prepare_call("key.revoke", Some(&name))).await.unwrap() else {
        panic!("a confirm")
    };
    assert_eq!(confirm.title, format!("Revoke {name}?"));
    assert_eq!(confirm.type_to_confirm.as_deref(), Some(name.as_str()));
    let revoked = p.invoke(key_call("key.revoke", &name, BTreeMap::new(), None)).await.unwrap();
    assert_eq!(revoked, InvokeOutcome::Ok { message: "Key revoked".into(), reveal: None, redirect: None });
    assert!(store.lookup(&reveal.value).await.unwrap().is_none(), "a revoked key no longer authenticates");
    let repeat = p.invoke(key_call("key.revoke", &name, BTreeMap::new(), None)).await.unwrap();
    assert_eq!(repeat, InvokeOutcome::NoOp { message: format!("Key {name} is already revoked") });
    let e = p.invoke(key_call("key.revoke", &missing, BTreeMap::new(), None)).await.unwrap_err();
    assert_eq!(e.status, Status::NotFound);
    let e = p.prepare(prepare_call("key.revoke", Some(&missing))).await.unwrap_err();
    assert_eq!(e.status, Status::NotFound);

    let dead = store.key(&name).await.unwrap().unwrap();
    let e = p.prepare(prepare_call("key.change-role", Some(&name))).await.unwrap_err();
    assert_eq!((e.status, e.msg.as_str()), (Status::Conflict, &*format!("key {name} is revoked")));
    let e = p.invoke(key_call("key.change-role", &name, to_a(), Some(dead.revision))).await.unwrap_err();
    assert_eq!((e.status, e.msg.as_str()), (Status::Conflict, &*format!("key {name} is revoked")));
    let e = p.invoke(key_call("key.change-role", &name, to_b(), Some(dead.revision))).await.unwrap_err();
    assert_eq!((e.status, e.msg.as_str()), (Status::Conflict, &*format!("key {name} is revoked")), "not a same-role no-op");
    let still = store.key(&name).await.unwrap().unwrap();
    assert_eq!((still.role, still.revision), (role_b.clone(), dead.revision), "a revoked key is never re-roled");

    cleanup(&pool, &base).await;
}

fn role_prepare(action: &str, role: &str) -> PrepareCall {
    PrepareCall { entity: Some(EntityRef { kind: ROLE_KIND.into(), id: role.into() }), ..prepare_call(action, None) }
}

/// A call bound to the role `name`.
fn role_call(action: &str, name: &str, input: BTreeMap<String, String>, revision: Option<i64>) -> InvokeCall {
    InvokeCall {
        entity: Some(EntityRef { kind: ROLE_KIND.into(), id: name.into() }),
        ..key_call(action, name, input, revision)
    }
}

#[tokio::test]
async fn create_edit_and_delete_roles_through_the_provider() {
    let _guard = db_test_lock().await;
    let Some(pool) = test_pool().await else { return };
    let base = unique_name(&pool).await;
    let store = Store { pool: pool.clone() };
    let p = provider(pool.clone());
    let custom = format!("{base}.custom");
    let in_use = format!("{base}-in-use");
    store.create_role(&in_use, &format!("match.report,{custom}")).await.unwrap();

    let Then::Form(form) = p.prepare(prepare_call("role.create", None)).await.unwrap() else { panic!("a form") };
    let FieldKind::Multi(options) = &form.fields[1].kind else { panic!("a Multi") };
    assert_eq!(options[0], "full");
    assert!(options.contains(&"match.report".to_string()) && options.contains(&custom), "{options:?}");

    let name = format!("{base}-r");
    let mut input = policy_input(&["match.report"], Some(&custom));
    input.insert("name".into(), name.clone());
    let created = p.invoke(call("role.create", input.clone())).await.unwrap();
    assert_eq!(created, InvokeOutcome::Ok { message: "Role created".into(), reveal: None, redirect: None });
    let role = store.role(&name).await.unwrap().expect("the role is stored");
    assert_eq!(role.policy, format!("match.report,{custom}"));
    let again = p.invoke(call("role.create", input)).await.unwrap();
    assert_eq!(again, InvokeOutcome::FieldErrors(BTreeMap::from([("name".into(), "a role with this name exists".into())])));

    let Then::Form(form) = p.prepare(role_prepare("role.edit", &name)).await.unwrap() else { panic!("a form") };
    assert_eq!(form.revision, Some(role.revision as u64));
    assert_eq!((form.fields[0].readonly, form.fields[0].default.as_deref()), (true, Some(name.as_str())));
    let picked = controlplane::decode_field(&form.fields[1].kind, form.fields[1].default.as_deref().unwrap());
    assert_eq!(picked, Ok(controlplane::FieldValue::Multi(vec!["match.report".into(), custom.clone()])));

    let same = p.invoke(role_call("role.edit", &name, policy_input(&[&custom, "match.report"], None), Some(role.revision)));
    assert_eq!(same.await.unwrap(), InvokeOutcome::NoOp { message: format!("Role {name} already has this policy") });
    assert_eq!(store.role(&name).await.unwrap().unwrap().revision, role.revision, "a NoOp writes nothing");
    let to_full = || policy_input(&["full"], None);
    let saved = p.invoke(role_call("role.edit", &name, to_full(), Some(role.revision))).await.unwrap();
    assert_eq!(saved, InvokeOutcome::Ok { message: "Policy saved".into(), reveal: None, redirect: None });
    let now = store.role(&name).await.unwrap().unwrap();
    assert_eq!(now.policy, "full");
    assert!(now.revision > role.revision);
    let to_report = || policy_input(&["match.report"], None);
    let stale = p.invoke(role_call("role.edit", &name, to_report(), Some(role.revision))).await.unwrap();
    assert_eq!(stale, InvokeOutcome::Conflict { current_revision: now.revision as u64 });
    assert_eq!(store.role(&name).await.unwrap().unwrap().policy, "full", "a stale form writes nothing");
    let missing = format!("{base}-missing");
    let e = p.invoke(role_call("role.edit", &missing, to_report(), Some(1))).await.unwrap_err();
    assert_eq!(e.status, Status::NotFound);
    let e = p.prepare(role_prepare("role.edit", &missing)).await.unwrap_err();
    assert_eq!(e.status, Status::NotFound);

    // A role may hold only typed methods: the Multi is optional (core/admin rejects a blank
    // required field before invoke) and `compose_policy` alone asks for full or a method.
    let typed_only = format!("{base}-typed");
    let Then::Form(form) = p.prepare(prepare_call("role.create", None)).await.unwrap() else { panic!("a form") };
    assert!(!form.fields[1].required, "the create form's Allowed methods is optional");
    let mut input = policy_input(&[], Some(&custom));
    input.insert("name".into(), typed_only.clone());
    let created = p.invoke(call("role.create", input)).await.unwrap();
    assert_eq!(created, InvokeOutcome::Ok { message: "Role created".into(), reveal: None, redirect: None });
    let typed = store.role(&typed_only).await.unwrap().expect("the typed-only role is stored");
    assert_eq!(typed.policy, custom);
    let Then::Form(form) = p.prepare(role_prepare("role.edit", &typed_only)).await.unwrap() else { panic!("a form") };
    assert!(!form.fields[1].required, "the edit form's Allowed methods is optional");
    let edit_typed = policy_input(&[], Some(&format!("{custom},x.y")));
    let saved = p.invoke(role_call("role.edit", &typed_only, edit_typed, Some(typed.revision))).await.unwrap();
    assert_eq!(saved, InvokeOutcome::Ok { message: "Policy saved".into(), reveal: None, redirect: None });
    let typed = store.role(&typed_only).await.unwrap().unwrap();
    assert_eq!(typed.policy, format!("{custom},x.y"));
    let nothing = p.invoke(role_call("role.edit", &typed_only, BTreeMap::new(), Some(typed.revision))).await.unwrap();
    assert_eq!(nothing, InvokeOutcome::FieldErrors(BTreeMap::from([("methods".into(), "pick full or at least one method".into())])));
    let mut nothing = BTreeMap::new();
    nothing.insert("name".to_string(), format!("{base}-empty"));
    let refused = p.invoke(call("role.create", nothing)).await.unwrap();
    assert_eq!(refused, InvokeOutcome::FieldErrors(BTreeMap::from([("methods".into(), "pick full or at least one method".into())])));

    let Then::Confirm(confirm) = p.prepare(role_prepare("role.delete", &name)).await.unwrap() else {
        panic!("a confirm")
    };
    assert_eq!(confirm.title, format!("Delete role {name}?"));
    let e = p.prepare(role_prepare("role.delete", &missing)).await.unwrap_err();
    assert_eq!(e.status, Status::NotFound);

    let key = format!("{base}-k");
    store.create_key(&key, &name, "tester").await.unwrap();
    assert_eq!(store.revoke_key(&key).await.unwrap(), Revoke::Revoked);
    let e = p.invoke(role_call("role.delete", &name, BTreeMap::new(), None)).await.unwrap_err();
    assert_eq!(e.status, Status::Conflict, "a revoked key still holds its role: {e:?}");
    assert_eq!(e.msg, format!("role {name} is still used by 1 key(s), revoked keys included"));
    assert!(store.role(&name).await.unwrap().is_some(), "a refused delete keeps the role");

    sqlx::query("DELETE FROM apikeys.keys WHERE name = $1").bind(&key).execute(&pool).await.unwrap();
    let deleted = p.invoke(role_call("role.delete", &name, BTreeMap::new(), None)).await.unwrap();
    assert_eq!(deleted, InvokeOutcome::Ok { message: "Role deleted".into(), reveal: None, redirect: None });
    assert!(store.role(&name).await.unwrap().is_none());
    let repeat = p.invoke(role_call("role.delete", &name, BTreeMap::new(), None)).await.unwrap();
    assert_eq!(repeat, InvokeOutcome::NoOp { message: format!("Role {name} does not exist") });

    cleanup(&pool, &base).await;
}
