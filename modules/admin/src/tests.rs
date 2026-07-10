//! White-box unit tests for the admin portal's pure helpers (ports of Go's
//! `admin_test.go` + `admin_fanout_test.go`): `slugify`, `resolve_items` (slug dedup,
//! local vs remote dispatch, absent-skip, error-card), and `build_groups` (first-seen
//! section order + active marking). No DB, no network — LOCAL renders and REMOTE
//! fetches are plain in-process closures.

use std::sync::Arc;

use futures::future::BoxFuture;
use lifecycle::Context;

use super::*;

// ---- helpers ----------------------------------------------------------------

/// Builds an [`AdminState`] over a fresh `Slots`, so a test can contribute items and
/// drive `resolve_items` against them.
fn state_from(ctx: &Context) -> AdminState {
    let mut env = minijinja::Environment::new();
    env.add_template("admin.html", TEMPLATE).unwrap();
    AdminState {
        env,
        slots: ctx.slots().clone(),
        auth_user: String::new(),
        auth_pass: String::new(),
        user: UserView::new(""),
    }
}

/// A LOCAL item contributing an empty render closure (Go's `Render` stand-in).
fn local_item(id: &str, section: &str, label: &str) -> adminapi::Item {
    adminapi::Item::local(
        id,
        section,
        label,
        Arc::new(|_p: &adminapi::Params| Ok(adminapi::Content::default())),
    )
}

/// A REMOTE item whose fetch yields the given data/err (the in-process stand-in for
/// the generated adminData edge client).
fn remote_item(
    id: &str,
    result: Result<adminapi::ItemData, ()>,
    err_kind: Option<bool>, // Some(true)=Absent, Some(false)=Other, None=Ok
) -> adminapi::Item {
    let fetch: adminapi::RemoteFetchFn = Arc::new(move |_p: adminapi::Params| {
        let out: Result<adminapi::ItemData, adminapi::ItemError> = match (&result, err_kind) {
            (Ok(d), None) => Ok(d.clone()),
            (_, Some(true)) => Err(adminapi::ItemError::Absent),
            (_, Some(false)) => Err(adminapi::ItemError::Other(anyhow::anyhow!("boom"))),
            _ => Ok(adminapi::ItemData::default()),
        };
        Box::pin(async move { out }) as BoxFuture<'static, _>
    });
    adminapi::Item {
        id: id.into(),
        section: String::new(),
        label: String::new(),
        render: None,
        remote_fetch: Some(fetch),
    }
}

// ---- slugify ----------------------------------------------------------------

#[test]
fn slugify_cases() {
    let cases = [
        ("Game Content", "game-content"),
        ("Players", "players"),
        ("  ", ""),
        ("A/B & C", "ab--c"),
        ("!@#$%^&*()", ""),
        ("_leading_", "leading"),
        ("hello-world", "hello-world"),
        ("Hello_World", "hello-world"),
        ("Zone42", "zone42"),
    ];
    for (input, want) in cases {
        assert_eq!(slugify(input), want, "slugify({input:?})");
    }
}

// ---- resolve_items: slug dedupe ---------------------------------------------

#[tokio::test]
async fn items_slug_dedupe() {
    let ctx = Context::new();
    ctx.contribute(adminapi::SLOT, local_item("a", "S", "Players"));
    ctx.contribute(adminapi::SLOT, local_item("b", "S", "Players"));
    ctx.contribute(adminapi::SLOT, local_item("c", "S", "!@#")); // empty slug → "item"
    ctx.contribute(adminapi::SLOT, local_item("d", "S", "Leaderboard"));
    let st = state_from(&ctx);

    let items = resolve_items(&st, &adminapi::Params::new()).await;
    let slugs: Vec<&str> = items.iter().map(|r| r.slug.as_str()).collect();
    assert_eq!(slugs, ["players", "players-2", "item", "leaderboard"]);
}

/// A wrong-typed contribution under `adminapi::SLOT` is a wiring bug: since the
/// homogeneous-slot contract landed in `contrib`, `contributions::<Item>()` is
/// loud about downcast misses — `debug_assert!`-panic in debug/test builds
/// (log + skip in release), instead of the old silent skip.
#[cfg(debug_assertions)]
#[tokio::test]
async fn items_mismatched_contributions_are_loud_in_debug() {
    let ctx = Context::new();
    ctx.contribute(adminapi::SLOT, "not an item".to_string());
    ctx.contribute(adminapi::SLOT, 42u32);
    ctx.contribute(adminapi::SLOT, local_item("v", "S", "Valid"));
    let st = state_from(&ctx);

    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        st.slots.contributions::<adminapi::Item>(adminapi::SLOT)
    }));
    assert!(r.is_err(), "mismatched contributions must panic under debug_assertions");

    // Matching types still round-trip: a homogeneous slot resolves normally.
    let ctx = Context::new();
    ctx.contribute(adminapi::SLOT, local_item("v", "S", "Valid"));
    let st = state_from(&ctx);
    let items = resolve_items(&st, &adminapi::Params::new()).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].slug, "valid");
}

#[tokio::test]
async fn items_empty_slot() {
    let ctx = Context::new();
    let st = state_from(&ctx);
    assert!(resolve_items(&st, &adminapi::Params::new()).await.is_empty());
}

// ---- resolve_items: local vs remote fan-out ---------------------------------

#[tokio::test]
async fn remote_success_carries_peer_metadata() {
    let ctx = Context::new();
    ctx.contribute(
        adminapi::SLOT,
        remote_item(
            "characters",
            Ok(adminapi::ItemData {
                id: "characters".into(),
                section: "Game Content".into(),
                label: "Characters".into(),
                content: adminapi::Content {
                    kpis: vec![adminapi::Kpi {
                        label: "Characters".into(),
                        value: "7".into(),
                        sub: String::new(),
                    }],
                    ..Default::default()
                },
            }),
            None,
        ),
    );
    let st = state_from(&ctx);

    let items = resolve_items(&st, &adminapi::Params::new()).await;
    assert_eq!(items.len(), 1);
    let it = &items[0];
    assert_eq!(it.section, "Game Content");
    assert_eq!(it.label, "Characters");
    assert_eq!(it.slug, "characters");
    assert!(it.item.render.is_none(), "remote item has no render");
    match &it.remote {
        Some(RemoteResult::Ok(c)) => assert_eq!(c.kpis[0].value, "7"),
        _ => panic!("expected a successful remote result"),
    }
}

#[tokio::test]
async fn remote_absent_is_skipped() {
    let ctx = Context::new();
    ctx.contribute(adminapi::SLOT, remote_item("ghost", Err(()), Some(true)));
    ctx.contribute(adminapi::SLOT, local_item("inv", "Game Content", "Inventory"));
    let st = state_from(&ctx);

    let items = resolve_items(&st, &adminapi::Params::new()).await;
    assert_eq!(items.len(), 1, "absent item dropped");
    assert_eq!(items[0].label, "Inventory");
}

#[tokio::test]
async fn remote_error_keeps_error_card() {
    let ctx = Context::new();
    ctx.contribute(adminapi::SLOT, remote_item("characters", Err(()), Some(false)));
    let st = state_from(&ctx);

    let items = resolve_items(&st, &adminapi::Params::new()).await;
    assert_eq!(items.len(), 1);
    let it = &items[0];
    // Label/Section fall back to the ID when the fetch failed.
    assert_eq!(it.label, "characters");
    assert_eq!(it.section, "characters");
    assert!(matches!(it.remote, Some(RemoteResult::Err(_))));
}

#[tokio::test]
async fn local_and_remote_dispatch_together() {
    let ctx = Context::new();
    ctx.contribute(adminapi::SLOT, local_item("inv", "Game Content", "Inventory"));
    ctx.contribute(
        adminapi::SLOT,
        remote_item(
            "characters",
            Ok(adminapi::ItemData {
                id: "characters".into(),
                section: "Game Content".into(),
                label: "Characters".into(),
                content: adminapi::Content::default(),
            }),
            None,
        ),
    );
    let st = state_from(&ctx);

    let items = resolve_items(&st, &adminapi::Params::new()).await;
    assert_eq!(items.len(), 2);
    assert!(items[0].item.render.is_some() && items[0].remote.is_none());
    assert!(items[1].item.render.is_none() && items[1].remote.is_some());
}

// ---- build_groups -----------------------------------------------------------

fn resolved(section: &str, label: &str, slug: &str) -> Resolved {
    Resolved {
        section: section.into(),
        label: label.into(),
        slug: slug.into(),
        item: local_item("x", section, label),
        remote: None,
    }
}

#[test]
fn groups_first_seen_order_and_active() {
    let items = [
        resolved("B", "X", "x"),
        resolved("A", "Y", "y"),
        resolved("B", "Z", "z"),
    ];
    let groups = build_groups(&items, "y");
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].section, "B"); // first-seen, not alphabetical
    assert_eq!(groups[0].items.len(), 2);
    assert_eq!(groups[0].items[0].slug, "x");
    assert!(!groups[0].items[0].active);
    assert_eq!(groups[1].section, "A");
    assert!(groups[1].items[0].active); // "y" is active
}

#[test]
fn groups_no_active_when_slug_empty() {
    let items = [resolved("S", "Alpha", "alpha"), resolved("S", "Beta", "beta")];
    let groups = build_groups(&items, "");
    for g in &groups {
        for ni in &g.items {
            assert!(!ni.active);
        }
    }
}

#[test]
fn groups_empty_input() {
    assert!(build_groups(&[], "anything").is_empty());
}

// ---- template render smoke --------------------------------------------------

#[test]
fn template_renders_kpis_table_and_escapes() {
    let ctx = Context::new();
    let st = state_from(&ctx);
    let data = PageData {
        crumb: "Game Content".into(),
        title: "Characters".into(),
        env: "Local".into(),
        user: UserView::new("Ops"),
        groups: vec![NavGroup {
            section: "Game Content".into(),
            items: vec![NavItem {
                label: "Characters".into(),
                slug: "characters".into(),
                active: true,
            }],
        }],
        page: Some(PageView {
            title: "Characters".into(),
            err: String::new(),
            kpis: vec![adminapi::Kpi {
                label: "Characters".into(),
                value: "3".into(),
                sub: String::new(),
            }],
            table: Some(adminapi::Table {
                columns: vec!["NAME".into()],
                rows: vec![
                    vec![adminapi::Cell::text("<script>Aria")],
                    vec![adminapi::Cell {
                        text: "mage".into(),
                        badge: "blue".into(),
                        ..Default::default()
                    }],
                ],
            }),
            form: None,
        }),
    };
    let html = st
        .env
        .get_template("admin.html")
        .unwrap()
        .render(&data)
        .unwrap();

    assert!(html.contains("nav-item active"), "active nav marked");
    assert!(html.contains(r#"href="/admin/characters""#));
    assert!(html.contains("kpi-value"));
    assert!(html.contains(r#"badge blue"#), "badge cell rendered");
    // Player-supplied text is auto-escaped (the `.html` template name), matching Go's
    // html/template — no raw <script> reaches the output.
    assert!(html.contains("&lt;script&gt;Aria"));
    assert!(!html.contains("<script>Aria"));
}

#[test]
fn template_renders_empty_shell() {
    let ctx = Context::new();
    let st = state_from(&ctx);
    let data = PageData {
        crumb: "Admin".into(),
        title: "Admin".into(),
        env: "Local".into(),
        user: UserView::new(""),
        groups: Vec::new(),
        page: None,
    };
    let html = st
        .env
        .get_template("admin.html")
        .unwrap()
        .render(&data)
        .unwrap();
    assert!(html.contains("No sections contributed yet."));
    assert!(html.contains("Local Admin"));
}

// ---- fail-closed startup gate (Admin::init) ---------------------------------

/// Serializes the two env-mutating tests below — `ADMIN_USER`/`ADMIN_OPEN` are
/// process-global, so they must not race each other (or observe each other's writes).
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Runs `f` with `ADMIN_USER`/`ADMIN_PASS`/`ADMIN_OPEN` cleared, then explicitly set
/// to the given values, restoring every prior value afterwards. Serialized so the two
/// gate tests never interleave their env writes.
fn with_admin_env(user: Option<&str>, open: Option<&str>, f: impl FnOnce()) {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let prev: Vec<(&str, Option<String>)> = ["ADMIN_USER", "ADMIN_PASS", "ADMIN_OPEN"]
        .iter()
        .map(|k| (*k, std::env::var(k).ok()))
        .collect();
    std::env::remove_var("ADMIN_USER");
    std::env::remove_var("ADMIN_PASS");
    std::env::remove_var("ADMIN_OPEN");
    if let Some(u) = user {
        std::env::set_var("ADMIN_USER", u);
    }
    if let Some(o) = open {
        std::env::set_var("ADMIN_OPEN", o);
    }
    f();
    for (k, v) in prev {
        match v {
            Some(v) => std::env::set_var(k, v),
            None => std::env::remove_var(k),
        }
    }
}

/// Empty `ADMIN_USER` with `ADMIN_OPEN` unset FAILS startup — the fail-closed default.
#[test]
fn init_bails_when_creds_missing_and_not_explicitly_open() {
    with_admin_env(None, None, || {
        let ctx = Context::new();
        let err = Admin::new()
            .init(&ctx)
            .expect_err("empty ADMIN_USER without ADMIN_OPEN must fail startup");
        assert!(
            err.to_string().contains("ADMIN_OPEN"),
            "bail message should point at the ADMIN_OPEN escape: {err}"
        );
    });
}

/// Empty `ADMIN_USER` with `ADMIN_OPEN=1` boots an open portal (deliberate local escape).
#[test]
fn init_ok_when_explicitly_open() {
    with_admin_env(None, Some("1"), || {
        let ctx = Context::new();
        Admin::new()
            .init(&ctx)
            .expect("ADMIN_OPEN=1 must permit a deliberately open portal");
    });
}

/// `admin_open_explicitly_on` matches the apikeys/gateway truthy set, case-insensitively.
#[test]
fn admin_open_truthy_parsing() {
    for (val, want) in [
        (Some("1"), true),
        (Some("true"), true),
        (Some("ON"), true),
        (Some("TrUe"), true),
        (Some("0"), false),
        (Some("no"), false),
        (Some(""), false),
        (None, false),
    ] {
        with_admin_env(None, val, || {
            assert_eq!(admin_open_explicitly_on(), want, "ADMIN_OPEN={val:?}");
        });
    }
}
