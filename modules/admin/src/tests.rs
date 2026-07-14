//! Admin portal tests. The pure helpers (`slugify`, `resolve_items`, `build_groups`,
//! templates) run with no DB, no network (LOCAL renders + REMOTE fetches are plain
//! closures). The session-auth matrix — login/lockout/CSRF/logout/cookie flags plus
//! the durable `admin.action` emits — targets the local Postgres (the test DB) and
//! SKIPs cleanly when it is unreachable, accounts-harness style.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use futures::future::BoxFuture;
use lifecycle::Context;
use tower::ServiceExt as _; // for `oneshot`

use super::*;

const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

// ---- shared helpers -----------------------------------------------------------

/// Builds an [`AdminState`] over a fresh `Slots`, so a test can contribute items and
/// drive `resolve_items`/the router against them. The pool is LAZY — the pure tests
/// never connect; the live tests pass a connected pool via [`wired`].
fn state_from(ctx: &Context) -> AdminState {
    AdminState {
        env: template_env().unwrap(),
        slots: ctx.slots().clone(),
        pool: sqlx::postgres::PgPool::connect_lazy(DEFAULT_DSN).unwrap(),
        bus: ctx.bus().clone(),
        open: true, // pure tests exercise helpers, not the session gate
        cookie_secure: true,
        trusted: Vec::new(),
        login_slots: Arc::new(tokio::sync::Semaphore::new(32)),
        argon_permits: Arc::new(tokio::sync::Semaphore::new(2)),
        login_limiter: httpmw::IpLimiter::new(5.0, 20),
        login_attempt_gc_requests: AtomicU64::new(0),
        verifier: Arc::new(ArgonVerifier),
    }
}

fn login_reaper_is_owned(admin: &Admin) -> bool {
    admin
        .login_reaper
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_some()
}

#[tokio::test]
async fn login_reaper_stops_and_restarts_without_clock_coordination() {
    let admin = Admin::new();
    assert!(
        admin
            .login_limiter
            .set(httpmw::IpLimiter::new(5.0, 20))
            .is_ok()
    );
    let ctx = Context::new();

    admin.start_login_reaper().unwrap();
    assert!(login_reaper_is_owned(&admin));
    admin.stop(&ctx).await.unwrap();
    assert!(!login_reaper_is_owned(&admin));

    admin.start_login_reaper().unwrap();
    assert!(login_reaper_is_owned(&admin));
    admin.stop(&ctx).await.unwrap();
    assert!(!login_reaper_is_owned(&admin));
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
        remote_submit: None,
    }
}

/// A REMOTE item that is BOTH readable (fetch yields `data`, whose `content.form` the
/// admin renders as an editable form) AND writable over the edge: its `remote_submit`
/// records every call into `calls` and answers `result` (an `Ok(SubmitOutcome)` or a
/// raw `opsapi::Error` — a `NotFound` stands in for a peer that never registered
/// `admin.adminSubmit`, a `Conflict` for a CAS miss). Lets the item_post remote branch
/// be driven with zero network.
fn remote_writable_item(
    id: &str,
    data: adminapi::ItemData,
    result: Result<adminapi::SubmitOutcome, opsapi::Error>,
    calls: Arc<Mutex<Vec<adminapi::Params>>>,
) -> adminapi::Item {
    let fetch_data = data.clone();
    let fetch: adminapi::RemoteFetchFn = Arc::new(move |_p: adminapi::Params| {
        let out: Result<adminapi::ItemData, adminapi::ItemError> = Ok(fetch_data.clone());
        Box::pin(async move { out }) as BoxFuture<'static, _>
    });
    let submit: adminapi::RemoteSubmitFn = Arc::new(move |params: adminapi::Params| {
        let calls = calls.clone();
        let result = result.clone();
        Box::pin(async move {
            calls.lock().unwrap().push(params);
            result
        }) as BoxFuture<'static, Result<adminapi::SubmitOutcome, opsapi::Error>>
    });
    adminapi::Item {
        id: id.into(),
        section: String::new(),
        label: String::new(),
        render: None,
        remote_fetch: Some(fetch),
        remote_submit: Some(submit),
    }
}

/// An [`adminapi::ItemData`] whose content carries an editable one-field form (so the
/// admin renders a POSTable form for the remote item). `form.submit` is `None` across
/// the wire; the admin dispatches the POST via the item's `remote_submit`.
fn remote_item_data_with_form(id: &str, label: &str) -> adminapi::ItemData {
    adminapi::ItemData {
        id: id.into(),
        section: "S".into(),
        label: label.into(),
        content: adminapi::Content {
            kpis: Vec::new(),
            table: None,
            form: Some(adminapi::Form {
                action: String::new(),
                fields: vec![adminapi::Field {
                    name: "knob".into(),
                    label: "Knob".into(),
                    value: String::new(),
                    ..Default::default()
                }],
                hidden: Vec::new(),
                submit: None,
            }),
        },
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

// ---- template render smoke ----------------------------------------------------

#[tokio::test]
async fn template_renders_kpis_table_csrf_and_escapes() {
    let ctx = Context::new();
    let st = state_from(&ctx);
    let data = PageData {
        crumb: "Game Content".into(),
        title: "Characters".into(),
        env: "Local".into(),
        user: UserView::new("Ops"),
        csrf: "csrf-tok-123".into(),
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
            form: Some(adminapi::Form {
                action: "/admin/characters".into(),
                fields: vec![adminapi::Field {
                    name: "note".into(),
                    label: "Note".into(),
                    value: String::new(),
                    ..Default::default()
                }],
                hidden: Vec::new(),
                submit: None,
            }),
            reveal: Vec::new(),
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
    // The session CSRF token is injected as a hidden input on the edit form AND the
    // logout form.
    assert!(html.contains(r#"name="_csrf" value="csrf-tok-123""#), "csrf hidden input");
    assert!(html.contains(r#"action="/admin/logout""#), "logout form rendered");
    // Player-supplied text is auto-escaped (the `.html` template name), matching Go's
    // html/template — no raw <script> reaches the output.
    assert!(html.contains("&lt;script&gt;Aria"));
    assert!(!html.contains("<script>Aria"));
}

#[tokio::test]
async fn template_renders_empty_shell_without_csrf() {
    let ctx = Context::new();
    let st = state_from(&ctx);
    let data = PageData {
        crumb: "Admin".into(),
        title: "Admin".into(),
        env: "Local".into(),
        user: UserView::new(""),
        csrf: String::new(),
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
    // No session → no CSRF input, no logout form (ADMIN_OPEN mode).
    assert!(!html.contains("_csrf"));
    assert!(!html.contains(r#"action="/admin/logout""#));
}

/// The typed-field render: a `Select` becomes `<select>`/`<option>` with the
/// preselected option marked, a `CheckboxGroup` becomes one checkbox per option (all
/// sharing the field name) with the pre-checked flag, a plain `Text` stays an input,
/// and a `SubmitOutcome` reveal renders its one-time value inline.
#[tokio::test]
async fn template_renders_select_checkboxgroup_and_reveal() {
    let ctx = Context::new();
    let st = state_from(&ctx);
    let data = PageData {
        crumb: "Access".into(),
        title: "API Keys".into(),
        env: "Local".into(),
        user: UserView::new("Ops"),
        csrf: "tok".into(),
        groups: Vec::new(),
        page: Some(PageView {
            title: "API Keys".into(),
            err: String::new(),
            kpis: Vec::new(),
            table: None,
            form: Some(adminapi::Form {
                action: "/admin/api-keys".into(),
                fields: vec![
                    adminapi::Field {
                        name: "role".into(),
                        label: "Role".into(),
                        value: "client".into(),
                        kind: adminapi::FieldKind::Select,
                        options: vec![
                            adminapi::FieldOption {
                                value: "client".into(),
                                label: "Client".into(),
                                checked: true,
                            },
                            adminapi::FieldOption {
                                value: "server".into(),
                                label: "Server".into(),
                                checked: false,
                            },
                        ],
                    },
                    adminapi::Field {
                        name: "methods".into(),
                        label: "Methods".into(),
                        value: String::new(),
                        kind: adminapi::FieldKind::CheckboxGroup,
                        options: vec![
                            adminapi::FieldOption {
                                value: "leaderboard.topScores".into(),
                                label: "leaderboard.topScores".into(),
                                checked: true,
                            },
                            adminapi::FieldOption {
                                value: "match.report".into(),
                                label: "match.report".into(),
                                checked: false,
                            },
                        ],
                    },
                    adminapi::Field {
                        name: "note".into(),
                        label: "Note".into(),
                        value: "hi".into(),
                        ..Default::default()
                    },
                ],
                hidden: Vec::new(),
                submit: None,
            }),
            reveal: vec![adminapi::RevealItem {
                label: "secret".into(),
                value: "ak_shown_once".into(),
            }],
        }),
    };
    let html = st.env.get_template("admin.html").unwrap().render(&data).unwrap();

    // Select: <select> with the preselected option marked, the other plain.
    assert!(html.contains(r#"<select name="role">"#), "select element: {html}");
    assert!(
        html.contains(r#"<option value="client" selected>Client</option>"#),
        "preselected option marked"
    );
    assert!(
        html.contains(r#"<option value="server">Server</option>"#),
        "unselected option unmarked"
    );
    // CheckboxGroup: one checkbox per option, shared name, checked flag on the first only.
    assert!(
        html.contains(r#"<input type="checkbox" name="methods" value="leaderboard.topScores" checked>"#),
        "checked box"
    );
    assert!(
        html.contains(r#"<input type="checkbox" name="methods" value="match.report">"#),
        "unchecked box"
    );
    // Text stays a plain input.
    assert!(html.contains(r#"<input type="text" name="note" value="hi">"#), "text input");
    // Show-once reveal panel.
    assert!(html.contains("Generated — shown once"), "reveal panel");
    assert!(html.contains(r#"value="ak_shown_once""#), "reveal value shown inline");
}

// ---- env-knob parsing (dev knobs, explicit-only conventions) ------------------

/// Serializes the env-mutating tests below — the knobs are process-global.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Runs `f` with the given env var set (or cleared), restoring the prior value.
fn with_env(key: &str, val: Option<&str>, f: impl FnOnce()) {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let prev = std::env::var(key).ok();
    match val {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
    f();
    match prev {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
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
        with_env("ADMIN_OPEN", val, || {
            assert_eq!(admin_open_explicitly_on(), want, "ADMIN_OPEN={val:?}");
        });
    }
}

/// `ADMIN_COOKIE_SECURE` is fail-closed: Secure stays ON unless EXPLICITLY falsy.
#[test]
fn cookie_secure_knob_parsing() {
    for (val, want) in [
        (None, true),
        (Some("1"), true),
        (Some("yes"), true),
        (Some(""), true),
        (Some("0"), false),
        (Some("false"), false),
        (Some("OFF"), false),
    ] {
        with_env("ADMIN_COOKIE_SECURE", val, || {
            assert_eq!(cookie_secure_on(), want, "ADMIN_COOKIE_SECURE={val:?}");
        });
    }
}

#[test]
fn backoff_is_exponential_and_capped() {
    assert_eq!(backoff_secs(5), 32);
    assert_eq!(backoff_secs(6), 64);
    assert_eq!(backoff_secs(9), 512);
    assert_eq!(backoff_secs(10), 900);
    assert_eq!(backoff_secs(100), 900);
    assert_eq!(backoff_secs(-1), 900, "nonsense input stays at the cap, never panics");
}

/// The consistency table (C18): `normalize_username` and the login path's own
/// `!username.is_empty() && username_within_cap(...)` check on the TRIMMED value
/// must agree on every case, or a CLI-created account and the login handler could
/// disagree about which usernames are usable (the zombie-account defect).
#[test]
fn normalize_username_agrees_with_login_validity_check() {
    let padded_128 = format!("  {}  ", "a".repeat(128));
    let cases: &[(&str, &str)] = &[
        ("empty", ""),
        ("whitespace-only", "   "),
        ("exactly-128-bytes", &"a".repeat(128)),
        ("129-bytes", &"a".repeat(129)),
        ("padded-under-cap", "  alice  "),
        ("padded-exactly-128-after-trim", &padded_128),
    ];
    for (label, input) in cases {
        let normalize_ok = normalize_username(input).is_ok();
        // Mirrors the OLD inline check `login_submit` used to run directly (now
        // routed through `normalize_username` too) — trim, then the same cap fn.
        let trimmed = input.trim();
        let login_ok = !trimmed.is_empty() && username_within_cap(trimmed);
        assert_eq!(
            normalize_ok, login_ok,
            "{label}: normalize_username()={normalize_ok} login-style check={login_ok} for {input:?}"
        );
    }
}

#[test]
fn normalize_username_trims_and_rejects_empty_or_over_cap() {
    assert_eq!(normalize_username("  alice  ").unwrap(), "alice");
    assert_eq!(normalize_username("bob").unwrap(), "bob");
    assert!(normalize_username("").is_err());
    assert!(normalize_username("   ").is_err());
    assert!(normalize_username(&"a".repeat(129)).is_err());
    assert!(normalize_username(&"a".repeat(128)).is_ok(), "exactly at the cap is accepted");
}

#[test]
fn password_roundtrip() {
    let h = hash_password("hunter2").unwrap();
    assert!(verify_password(&h, "hunter2"), "correct password rejected");
    assert!(!verify_password(&h, "wrong"), "wrong password accepted");
    assert!(!verify_password("not-a-hash", "hunter2"), "garbage hash accepted");
}

// ============================================================================
// Live-Postgres integration: the session-auth matrix. One schema migration per
// test binary; each test uses unique usernames + a unique peer IP so parallel
// tests never share a lockout subject.
// ============================================================================

/// Opens the local Postgres; returns `None` (printing a skip line) when unreachable,
/// so the suite RUNS but SKIPs cleanly with no DB.
async fn test_pool() -> Option<PgPool> {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(Duration::from_secs(3), PgPool::connect(&dsn)).await {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — admin DB tests skipped");
            return None;
        }
    };
    Some(pool)
}

/// Migrates BOTH the asyncevents plane and the admin schema EXACTLY ONCE per test
/// binary — concurrent idempotent DDL across parallel tests can deadlock on catalog
/// locks.
static SCHEMA_READY: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

async fn ensure_schema(pool: &PgPool) {
    SCHEMA_READY
        .get_or_init(|| async {
            let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
            asyncevents::Plane::new(pool.clone(), dsn)
                .unwrap()
                .migrate()
                .await
                .unwrap();
            let ctx = Context::with_db(pool.clone());
            let admin = Admin::new();
            admin.register(&ctx).unwrap();
            admin.migrate(&ctx).await.unwrap();
        })
        .await;
}

/// A wired live state: real pool, real durable transport (`emit_tx` appends to
/// `asyncevents.events`), fresh `Slots` for per-test item contributions.
async fn wired(pool: &PgPool, open: bool, cookie_secure: bool) -> (Context, Arc<AdminState>) {
    wired_with_verifier(pool, open, cookie_secure, Arc::new(ArgonVerifier)).await
}

async fn wired_with_verifier(
    pool: &PgPool,
    open: bool,
    cookie_secure: bool,
    verifier: Arc<dyn PasswordVerifier>,
) -> (Context, Arc<AdminState>) {
    ensure_schema(pool).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    let st = Arc::new(AdminState {
        env: template_env().unwrap(),
        slots: ctx.slots().clone(),
        pool: pool.clone(),
        bus: ctx.bus().clone(),
        open,
        cookie_secure,
        trusted: Vec::new(),
        login_slots: Arc::new(tokio::sync::Semaphore::new(32)),
        argon_permits: Arc::new(tokio::sync::Semaphore::new(2)),
        login_limiter: httpmw::IpLimiter::new(1_000.0, 1_000),
        login_attempt_gc_requests: AtomicU64::new(0),
        verifier,
    });
    (ctx, st)
}

#[derive(Default)]
struct RecordingVerifier {
    calls: Mutex<Vec<(String, String)>>,
}

impl PasswordVerifier for RecordingVerifier {
    fn verify(&self, encoded: &str, password: &str) -> bool {
        self.calls.lock().unwrap().push((encoded.to_string(), password.to_string()));
        false
    }
}

/// Per-test unique suffix (parallel-safe, plus wall-clock so reruns never collide).
static UNIQ: AtomicU32 = AtomicU32::new(0);
fn uniq(prefix: &str) -> String {
    let n = UNIQ.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    format!("{prefix}-{t}-{n}")
}

async fn create_user(pool: &PgPool, username: &str, pass: &str) {
    let hash = hash_password(pass).unwrap();
    sqlx::query("INSERT INTO admin.users (username, pass_hash) VALUES ($1, $2)")
        .bind(username)
        .bind(&hash)
        .execute(pool)
        .await
        .unwrap();
}

/// Test teardown: the user row (sessions CASCADE), its attempt rows, and its
/// durable events.
async fn cleanup_user(pool: &PgPool, username: &str) {
    let _ = sqlx::query("DELETE FROM admin.users WHERE username = $1")
        .bind(username)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM admin.login_attempts WHERE subject = $1")
        .bind(format!("user:{username}"))
        .execute(pool)
        .await;
    let _ = asyncevents::testing::cleanup_events(pool, "actor", username).await;
}

async fn cleanup_ip(pool: &PgPool, ip: &str) {
    let _ = sqlx::query("DELETE FROM admin.login_attempts WHERE subject = $1")
        .bind(format!("ip:{ip}"))
        .execute(pool)
        .await;
}

/// Counts `admin.action` rows by one payload key + the action value. Direct plane
/// SQL is sanctioned in test files (archcheck rule 14b exempts tests).
async fn action_rows(pool: &PgPool, key: &str, value: &str, action: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM asyncevents.events
         WHERE topic = 'admin.action' AND payload->>$1 = $2 AND payload->>'action' = $3",
    )
    .bind(key)
    .bind(value)
    .bind(action)
    .fetch_one(pool)
    .await
    .unwrap();
    n
}

async fn action_detail(pool: &PgPool, target: &str, action: &str) -> Option<String> {
    sqlx::query_scalar(
        "SELECT payload->>'detail' FROM asyncevents.events
         WHERE topic = 'admin.action' AND payload->>'target' = $1
           AND payload->>'action' = $2
         LIMIT 1",
    )
    .bind(target)
    .bind(action)
    .fetch_optional(pool)
    .await
    .unwrap()
}

/// Builds a form request with the ConnectInfo extension `app::run` injects in
/// production (`into_make_service_with_connect_info`).
fn form_req(method: &str, uri: &str, peer: &str, cookie: Option<&str>, body: Option<String>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(c) = cookie {
        b = b.header("cookie", c);
    }
    let mut req = match body {
        Some(body) => b
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let addr: SocketAddr = peer.parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    req
}

async fn send(st: &Arc<AdminState>, req: Request<Body>) -> Response {
    router(st.clone()).oneshot(req).await.unwrap()
}

async fn body_string(resp: Response) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    String::from_utf8_lossy(&bytes).to_string()
}

fn set_cookie(resp: &Response) -> String {
    resp.headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// Extracts the session token from a minting Set-Cookie value.
fn token_of(set_cookie: &str) -> String {
    set_cookie
        .strip_prefix("admin_session=")
        .and_then(|rest| rest.split(';').next())
        .unwrap_or_default()
        .to_string()
}

fn location_of(resp: &Response) -> String {
    resp.headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

async fn post_login(st: &Arc<AdminState>, peer: &str, user: &str, pass: &str) -> Response {
    send(
        st,
        form_req(
            "POST",
            "/admin/login",
            peer,
            None,
            Some(format!("username={user}&password={pass}")),
        ),
    )
    .await
}

async fn csrf_of(pool: &PgPool, token: &str) -> String {
    let (csrf,): (String,) =
        sqlx::query_as("SELECT csrf_token FROM admin.sessions WHERE token = $1")
            .bind(token)
            .fetch_one(pool)
            .await
            .unwrap();
    csrf
}

async fn attempts_row(pool: &PgPool, subject: &str) -> Option<(i32, bool)> {
    sqlx::query_as(
        "SELECT fails, locked_until IS NOT NULL FROM admin.login_attempts WHERE subject = $1",
    )
    .bind(subject)
    .fetch_optional(pool)
    .await
    .unwrap()
}

// ---- the matrix -----------------------------------------------------------------

/// Login success: 303 → /admin, the cookie carries every flag (Secure included by
/// default), the session row exists, and the durable `login-succeeded` row landed.
#[tokio::test(flavor = "multi_thread")]
async fn login_success_mints_session_cookie_and_emits() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let user = uniq("t-ok");
    create_user(&pool, &user, "right").await;

    let resp = post_login(&st, "203.0.113.10:9999", &user, "right").await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(location_of(&resp), "/admin");
    let sc = set_cookie(&resp);
    for flag in ["HttpOnly", "SameSite=Strict", "Path=/admin", "Max-Age=43200", "Secure"] {
        assert!(sc.contains(flag), "Set-Cookie missing {flag}: {sc}");
    }
    let token = token_of(&sc);
    assert_eq!(token.len(), 43, "32B base64url token");

    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM admin.sessions WHERE token = $1 AND username = $2 AND expires_at > now()",
    )
    .bind(&token)
    .bind(&user)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 1, "session row minted");
    assert_eq!(action_rows(&pool, "actor", &user, "login-succeeded").await, 1);

    cleanup_user(&pool, &user).await;
    cleanup_ip(&pool, "203.0.113.10").await;
}

/// `ADMIN_COOKIE_SECURE=0` variant: everything identical except no `Secure` flag.
#[tokio::test(flavor = "multi_thread")]
async fn login_cookie_without_secure_when_opted_out() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, false).await;
    let user = uniq("t-insec");
    create_user(&pool, &user, "right").await;

    let resp = post_login(&st, "203.0.113.11:9999", &user, "right").await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let sc = set_cookie(&resp);
    assert!(!sc.contains("Secure"), "Secure must be absent when opted out: {sc}");
    for flag in ["HttpOnly", "SameSite=Strict", "Path=/admin", "Max-Age=43200"] {
        assert!(sc.contains(flag), "Set-Cookie missing {flag}: {sc}");
    }

    cleanup_user(&pool, &user).await;
    cleanup_ip(&pool, "203.0.113.11").await;
}

/// No username oracle: wrong password, unknown user, and a LOCKED user answer 401
/// with BYTE-IDENTICAL bodies.
#[tokio::test(flavor = "multi_thread")]
async fn failed_logins_have_identical_generic_401_bodies() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let user = uniq("t-oracle");
    create_user(&pool, &user, "right").await;
    // A pre-locked user (row planted directly).
    let locked_user = uniq("t-locked");
    create_user(&pool, &locked_user, "right").await;
    sqlx::query(
        "INSERT INTO admin.login_attempts (subject, fails, locked_until)
         VALUES ($1, 5, now() + interval '10 minutes')",
    )
    .bind(format!("user:{locked_user}"))
    .execute(&pool)
    .await
    .unwrap();

    let wrong = post_login(&st, "203.0.113.12:9999", &user, "nope").await;
    let unknown = post_login(&st, "203.0.113.12:9999", &uniq("t-ghost"), "nope").await;
    let locked = post_login(&st, "203.0.113.12:9999", &locked_user, "right").await;

    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(locked.status(), StatusCode::UNAUTHORIZED);
    let (a, b, c) = (
        body_string(wrong).await,
        body_string(unknown).await,
        body_string(locked).await,
    );
    assert_eq!(a, b, "wrong-pass and unknown-user bodies must be identical");
    assert_eq!(a, c, "locked body must be identical too (no lock oracle)");
    assert!(a.contains(GENERIC_LOGIN_ERROR));

    cleanup_user(&pool, &user).await;
    cleanup_user(&pool, &locked_user).await;
    cleanup_ip(&pool, "203.0.113.12").await;
}

/// The user row locks at 5 consecutive fails (emitting `login-locked`), the correct
/// password STILL answers the generic 401 while locked (and does not increment),
/// and after the lock window expires the correct password logs in and resets the
/// attempt rows.
#[tokio::test(flavor = "multi_thread")]
async fn user_locks_at_five_and_unlocks_after_window() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let user = uniq("t-lock5");
    let subject = format!("user:{user}");
    create_user(&pool, &user, "right").await;

    for i in 0..5 {
        let resp = post_login(&st, "203.0.113.13:9999", &user, "wrong").await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "fail #{i}");
    }
    assert_eq!(attempts_row(&pool, &subject).await, Some((5, true)), "locked at 5");
    assert_eq!(action_rows(&pool, "actor", &user, "login-locked").await, 1);

    // Correct password while locked: same generic 401, no further increment.
    let resp = post_login(&st, "203.0.113.13:9999", &user, "right").await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(attempts_row(&pool, &subject).await, Some((5, true)), "no increment while locked");

    // Expire the lock window → the correct password succeeds and resets the rows.
    sqlx::query(
        "UPDATE admin.login_attempts SET locked_until = now() - interval '1 second' WHERE subject = $1",
    )
    .bind(&subject)
    .execute(&pool)
    .await
    .unwrap();
    let resp = post_login(&st, "203.0.113.13:9999", &user, "right").await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER, "unlocked after the window");
    assert_eq!(attempts_row(&pool, &subject).await, None, "attempt rows reset on success");

    cleanup_user(&pool, &user).await;
    cleanup_ip(&pool, "203.0.113.13").await;
}

/// The IP row accumulates across usernames but does NOT lock below its 20
/// threshold — a shared office IP isn't bricked by one folk's typos [R2].
#[tokio::test(flavor = "multi_thread")]
async fn ip_row_accumulates_but_does_not_lock_below_twenty() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let ip = "203.0.113.14";
    let peer = format!("{ip}:9999");
    let ghosts: Vec<String> = (0..3).map(|_| uniq("t-ipghost")).collect();

    for g in &ghosts {
        let resp = post_login(&st, &peer, g, "wrong").await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        attempts_row(&pool, &format!("ip:{ip}")).await,
        Some((3, false)),
        "ip row counts but is not locked below 20"
    );

    // The same IP can still log a real user in.
    let user = uniq("t-ipok");
    create_user(&pool, &user, "right").await;
    let resp = post_login(&st, &peer, &user, "right").await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);

    cleanup_user(&pool, &user).await;
    for g in &ghosts {
        cleanup_user(&pool, g).await;
    }
    cleanup_ip(&pool, ip).await;
}

/// An expired session is a miss: page GETs bounce to the login form, POSTs 401.
#[tokio::test(flavor = "multi_thread")]
async fn expired_session_redirects_get_and_401s_post() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let user = uniq("t-expiry");
    create_user(&pool, &user, "right").await;

    let resp = post_login(&st, "203.0.113.15:9999", &user, "right").await;
    let token = token_of(&set_cookie(&resp));
    let cookie = format!("admin_session={token}");

    // Live session works.
    let ok = send(&st, form_req("GET", "/admin", "203.0.113.15:9999", Some(&cookie), None)).await;
    assert_eq!(ok.status(), StatusCode::OK, "empty slot renders the shell");

    sqlx::query("UPDATE admin.sessions SET expires_at = now() - interval '1 second' WHERE token = $1")
        .bind(&token)
        .execute(&pool)
        .await
        .unwrap();

    let get = send(&st, form_req("GET", "/admin", "203.0.113.15:9999", Some(&cookie), None)).await;
    assert_eq!(get.status(), StatusCode::SEE_OTHER);
    assert_eq!(location_of(&get), "/admin/login");
    let post = send(
        &st,
        form_req("POST", "/admin/whatever", "203.0.113.15:9999", Some(&cookie), Some(String::new())),
    )
    .await;
    assert_eq!(post.status(), StatusCode::UNAUTHORIZED);

    cleanup_user(&pool, &user).await;
    cleanup_ip(&pool, "203.0.113.15").await;
}

/// An unauthenticated GET bounces to `/admin/login`; the login page itself serves
/// 200 with the form (the split-proof `[AD1]` shape).
#[tokio::test(flavor = "multi_thread")]
async fn unauthenticated_get_redirects_to_login() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;

    let get = send(&st, form_req("GET", "/admin", "203.0.113.16:9999", None, None)).await;
    assert_eq!(get.status(), StatusCode::SEE_OTHER);
    assert_eq!(location_of(&get), "/admin/login");

    let login = send(&st, form_req("GET", "/admin/login", "203.0.113.16:9999", None, None)).await;
    assert_eq!(login.status(), StatusCode::OK);
    let html = body_string(login).await;
    assert!(html.contains(r#"action="/admin/login""#));
    // Security headers ride the admin router.
    // (Checked on a fresh response since `html` consumed the first.)
    let login2 = send(&st, form_req("GET", "/admin/login", "203.0.113.16:9999", None, None)).await;
    assert!(login2.headers().contains_key(header::CONTENT_SECURITY_POLICY));
    assert_eq!(login2.headers()[header::X_FRAME_OPTIONS], "DENY");
    assert_eq!(login2.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(login2.headers()[header::REFERRER_POLICY], "no-referrer");
}

/// CSRF is checked BEFORE the local/remote editability decision: a REMOTE item
/// without `_csrf` is 403 (not 405); with a valid token the same POST reaches the
/// editability check and 405s. A LOCAL form with a bad token never runs `submit`;
/// with the right token it submits, emits `form-submit`, and 303s.
#[tokio::test(flavor = "multi_thread")]
async fn csrf_rejects_before_editability_and_gates_local_submit() {
    let Some(pool) = test_pool().await else { return };
    let (ctx, st) = wired(&pool, false, true).await;
    let user = uniq("t-csrf");
    create_user(&pool, &user, "right").await;

    // A remote (read-only by construction) item + a local editable form.
    let remote_label = uniq("Remote Panel");
    let remote_slug = slugify(&remote_label);
    ctx.contribute(
        adminapi::SLOT,
        remote_item(
            "remote",
            Ok(adminapi::ItemData {
                id: "remote".into(),
                section: "S".into(),
                label: remote_label.clone(),
                content: adminapi::Content::default(),
            }),
            None,
        ),
    );
    let submitted: Arc<Mutex<Vec<adminapi::Params>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = submitted.clone();
    let local_label = uniq("Local Form");
    let local_slug = slugify(&local_label);
    let submit: adminapi::SubmitFn = Arc::new(move |values: adminapi::Params| {
        let sink = sink.clone();
        Box::pin(async move {
            sink.lock().unwrap().push(values);
            Ok(adminapi::SubmitOutcome::default())
        }) as BoxFuture<'static, Result<adminapi::SubmitOutcome, adminapi::SubmitError>>
    });
    ctx.contribute(
        adminapi::SLOT,
        adminapi::Item::local(
            "localform",
            "S",
            &local_label,
            Arc::new(move |_p: &adminapi::Params| {
                Ok(adminapi::Content {
                    kpis: Vec::new(),
                    table: None,
                    form: Some(adminapi::Form {
                        action: String::new(),
                        fields: vec![adminapi::Field {
                            name: "knob".into(),
                            label: "Knob".into(),
                            value: String::new(),
                            ..Default::default()
                        }],
                        hidden: vec![adminapi::HiddenField {
                            name: "_expected_revision".into(),
                            value: "1".into(),
                        }],
                        submit: Some(submit.clone()),
                    }),
                })
            }),
        ),
    );

    let resp = post_login(&st, "203.0.113.17:9999", &user, "right").await;
    let token = token_of(&set_cookie(&resp));
    let cookie = format!("admin_session={token}");
    let csrf = csrf_of(&pool, &token).await;

    // Remote item, no _csrf → 403 (CSRF first), with _csrf → 405 (editability).
    let no_csrf = send(
        &st,
        form_req("POST", &format!("/admin/{remote_slug}"), "203.0.113.17:9999", Some(&cookie), Some(String::new())),
    )
    .await;
    assert_eq!(no_csrf.status(), StatusCode::FORBIDDEN, "CSRF must reject before editability");
    let with_csrf = send(
        &st,
        form_req(
            "POST",
            &format!("/admin/{remote_slug}"),
            "203.0.113.17:9999",
            Some(&cookie),
            Some(format!("_csrf={csrf}")),
        ),
    )
    .await;
    assert_eq!(with_csrf.status(), StatusCode::METHOD_NOT_ALLOWED, "remote stays read-only");

    // Local form, wrong _csrf → 403, submit never ran.
    let bad = send(
        &st,
        form_req(
            "POST",
            &format!("/admin/{local_slug}"),
            "203.0.113.17:9999",
            Some(&cookie),
            Some("knob=v&_csrf=wrong".to_string()),
        ),
    )
    .await;
    assert_eq!(bad.status(), StatusCode::FORBIDDEN);
    assert!(submitted.lock().unwrap().is_empty(), "submit must not run on a CSRF miss");

    // Local form, right _csrf → 303, submit ran, durable form-submit landed.
    let good = send(
        &st,
        form_req(
            "POST",
            &format!("/admin/{local_slug}"),
            "203.0.113.17:9999",
            Some(&cookie),
            Some(format!("knob=v&_expected_revision=1&_csrf={csrf}")),
        ),
    )
    .await;
    assert_eq!(good.status(), StatusCode::SEE_OTHER);
    assert_eq!(location_of(&good), format!("/admin/{local_slug}"));
    {
        let calls = submitted.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].get("knob").map(String::as_str), Some("v"));
        assert_eq!(
            calls[0].get("_expected_revision").map(String::as_str),
            Some("1")
        );
        assert!(!calls[0].contains_key("_csrf"), "_csrf never reaches the module");
    }
    assert_eq!(action_rows(&pool, "target", &local_slug, "form-submit").await, 1);
    assert_eq!(
        action_detail(&pool, &local_slug, "form-submit").await.as_deref(),
        Some("knob"),
        "hidden concurrency evidence must never enter the field-name audit trail"
    );

    cleanup_user(&pool, &user).await;
    cleanup_ip(&pool, "203.0.113.17").await;
    let _ = sqlx::query(
        "DELETE FROM asyncevents.events WHERE topic = 'admin.action' AND payload->>'target' = $1",
    )
    .bind(&local_slug)
    .execute(&pool)
    .await;
}

/// The REMOTE-write failing branch: a peer that never registered `admin.adminSubmit`
/// answers `NotFound`, so a POST with a valid CSRF token dispatches to the edge, gets
/// NotFound, and degrades to 405 (read-only). A POST WITHOUT `_csrf` is rejected 403
/// BEFORE `remote_submit` is ever dialed — proving the ordering contract AND that a bad
/// token never reaches the edge (calls stays empty).
#[tokio::test(flavor = "multi_thread")]
async fn remote_submit_not_found_is_405_and_csrf_gates_before_edge() {
    let Some(pool) = test_pool().await else { return };
    let (ctx, st) = wired(&pool, false, true).await;
    let user = uniq("t-remote-write");
    let peer = "203.0.113.28:9999";
    create_user(&pool, &user, "right").await;

    let calls: Arc<Mutex<Vec<adminapi::Params>>> = Arc::new(Mutex::new(Vec::new()));
    let label = uniq("Remote Writable");
    let slug = slugify(&label);
    ctx.contribute(
        adminapi::SLOT,
        remote_writable_item(
            "apikeys",
            remote_item_data_with_form("apikeys", &label),
            Err(opsapi::Error::not_found("edge: unknown method admin.adminSubmit")),
            calls.clone(),
        ),
    );

    let login = post_login(&st, peer, &user, "right").await;
    let token = token_of(&set_cookie(&login));
    let cookie = format!("admin_session={token}");
    let csrf = csrf_of(&pool, &token).await;

    // No _csrf → 403 BEFORE any edge dispatch (calls must stay empty).
    let no_csrf = send(
        &st,
        form_req("POST", &format!("/admin/{slug}"), peer, Some(&cookie), Some("knob=v".to_string())),
    )
    .await;
    assert_eq!(no_csrf.status(), StatusCode::FORBIDDEN, "CSRF must reject before the edge");
    assert!(
        calls.lock().unwrap().is_empty(),
        "remote_submit must not be dialed on a CSRF miss"
    );

    // Valid _csrf → the edge is dialed once and NotFound degrades to 405 read-only.
    let with_csrf = send(
        &st,
        form_req(
            "POST",
            &format!("/admin/{slug}"),
            peer,
            Some(&cookie),
            Some(format!("knob=v&_csrf={csrf}")),
        ),
    )
    .await;
    assert_eq!(
        with_csrf.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "a NotFound peer degrades the item to read-only"
    );
    assert_eq!(calls.lock().unwrap().len(), 1, "the edge was dialed exactly once");

    cleanup_user(&pool, &user).await;
    cleanup_ip(&pool, "203.0.113.28").await;
}

/// The REMOTE-write success branch: a CheckboxGroup posts its shared name once per
/// checked option and the admin comma-joins them into ONE param before dispatching; a
/// `SubmitOutcome` carrying a reveal renders INLINE (200, never a 303 that would drop
/// the one-time value); `_csrf` never reaches the module.
#[tokio::test(flavor = "multi_thread")]
async fn remote_submit_joins_checkboxes_and_renders_reveal_inline() {
    let Some(pool) = test_pool().await else { return };
    let (ctx, st) = wired(&pool, false, true).await;
    let user = uniq("t-remote-ok");
    let peer = "203.0.113.29:9999";
    create_user(&pool, &user, "right").await;

    let calls: Arc<Mutex<Vec<adminapi::Params>>> = Arc::new(Mutex::new(Vec::new()));
    let label = uniq("Remote Reveal");
    let slug = slugify(&label);
    let data = adminapi::ItemData {
        id: "apikeys".into(),
        section: "S".into(),
        label: label.clone(),
        content: adminapi::Content {
            kpis: Vec::new(),
            table: None,
            form: Some(adminapi::Form {
                action: String::new(),
                fields: vec![adminapi::Field {
                    name: "methods".into(),
                    label: "Methods".into(),
                    value: String::new(),
                    kind: adminapi::FieldKind::CheckboxGroup,
                    options: vec![
                        adminapi::FieldOption {
                            value: "a".into(),
                            label: "A".into(),
                            checked: false,
                        },
                        adminapi::FieldOption {
                            value: "b".into(),
                            label: "B".into(),
                            checked: false,
                        },
                    ],
                }],
                hidden: Vec::new(),
                submit: None,
            }),
        },
    };
    ctx.contribute(
        adminapi::SLOT,
        remote_writable_item(
            "apikeys",
            data,
            Ok(adminapi::SubmitOutcome {
                reveal: vec![adminapi::RevealItem {
                    label: "secret".into(),
                    value: "ak_once".into(),
                }],
            }),
            calls.clone(),
        ),
    );

    let login = post_login(&st, peer, &user, "right").await;
    let token = token_of(&set_cookie(&login));
    let cookie = format!("admin_session={token}");
    let csrf = csrf_of(&pool, &token).await;

    let resp = send(
        &st,
        form_req(
            "POST",
            &format!("/admin/{slug}"),
            peer,
            Some(&cookie),
            Some(format!("methods=a&methods=b&_csrf={csrf}")),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "a reveal renders inline, never redirects");
    let html = body_string(resp).await;
    assert!(html.contains("Generated — shown once"), "reveal panel rendered");
    assert!(html.contains(r#"value="ak_once""#), "one-time value shown inline");

    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].get("methods").map(String::as_str),
            Some("a,b"),
            "the checkbox group's repeated posts are comma-joined"
        );
        assert!(!calls[0].contains_key("_csrf"), "_csrf never reaches the module");
    }

    cleanup_user(&pool, &user).await;
    cleanup_ip(&pool, "203.0.113.29").await;
}

/// The browser's original hidden expected-state value survives the POST-time
/// re-render even when that render no longer declares the same dynamic row. The
/// owning submit sees the stale token, rejects before mutation, and the portal maps
/// that typed result to a stable 409 without appending `admin.action`.
#[tokio::test(flavor = "multi_thread")]
async fn stale_hidden_evidence_round_trips_to_conflict_without_audit() {
    let Some(pool) = test_pool().await else { return };
    let (ctx, st) = wired(&pool, false, true).await;
    let user = uniq("t-stale-form");
    let peer = "203.0.113.27:9999";
    let peer_ip = "203.0.113.27";
    create_user(&pool, &user, "right").await;

    let authority = Arc::new(AtomicU64::new(1));
    let applied = Arc::new(AtomicU64::new(0));
    let submitted: Arc<Mutex<Vec<adminapi::Params>>> = Arc::new(Mutex::new(Vec::new()));

    let submit_authority = authority.clone();
    let submit_applied = applied.clone();
    let submit_values = submitted.clone();
    let submit: adminapi::SubmitFn = Arc::new(move |values: adminapi::Params| {
        let authority = submit_authority.clone();
        let applied = submit_applied.clone();
        let submitted = submit_values.clone();
        Box::pin(async move {
            let expected = values
                .get("_expected_deleted_row")
                .and_then(|value| value.parse::<u64>().ok());
            submitted.lock().unwrap().push(values);
            if expected != Some(authority.load(Ordering::SeqCst)) {
                return Err(adminapi::SubmitError::Conflict);
            }
            applied.fetch_add(1, Ordering::SeqCst);
            Ok(adminapi::SubmitOutcome::default())
        }) as BoxFuture<'static, Result<adminapi::SubmitOutcome, adminapi::SubmitError>>
    });

    let render_authority = authority.clone();
    let label = uniq("Stale Form");
    let slug = slugify(&label);
    ctx.contribute(
        adminapi::SLOT,
        adminapi::Item::local(
            "stale-form",
            "S",
            &label,
            Arc::new(move |_p: &adminapi::Params| {
                let revision = render_authority.load(Ordering::SeqCst);
                let hidden_name = if revision == 1 {
                    "_expected_deleted_row"
                } else {
                    "_expected_current_row"
                };
                Ok(adminapi::Content {
                    kpis: Vec::new(),
                    table: None,
                    form: Some(adminapi::Form {
                        action: String::new(),
                        fields: vec![adminapi::Field {
                            name: "knob".into(),
                            label: "Knob".into(),
                            value: "old".into(),
                            ..Default::default()
                        }],
                        hidden: vec![adminapi::HiddenField {
                            name: hidden_name.into(),
                            value: revision.to_string(),
                        }],
                        submit: Some(submit.clone()),
                    }),
                })
            }),
        ),
    );

    let login = post_login(&st, peer, &user, "right").await;
    let token = token_of(&set_cookie(&login));
    let cookie = format!("admin_session={token}");
    let csrf = csrf_of(&pool, &token).await;

    let get = send(
        &st,
        form_req(
            "GET",
            &format!("/admin/{slug}"),
            peer,
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(get.status(), StatusCode::OK);
    let html = body_string(get).await;
    assert!(
        html.contains(r#"type="hidden" name="_expected_deleted_row" value="1""#),
        "GET must carry the original authority evidence: {html}"
    );

    // Simulate a concurrent authoritative change that also removes the original
    // row from the POST-time declarative form.
    authority.store(2, Ordering::SeqCst);
    let post = send(
        &st,
        form_req(
            "POST",
            &format!("/admin/{slug}"),
            peer,
            Some(&cookie),
            Some(format!(
                "knob=new&_expected_deleted_row=1&undeclared=ignored&_csrf={csrf}"
            )),
        ),
    )
    .await;
    assert_eq!(post.status(), StatusCode::CONFLICT);
    let html = body_string(post).await;
    assert!(html.contains(STALE_FORM_ERROR), "stable conflict message missing: {html}");
    assert_eq!(applied.load(Ordering::SeqCst), 0, "conflict must not mutate authority");

    {
        let calls = submitted.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].get("_expected_deleted_row").map(String::as_str),
            Some("1"),
            "the original dynamic token must survive the fresh POST render"
        );
        assert_eq!(calls[0].get("knob").map(String::as_str), Some("new"));
        assert!(!calls[0].contains_key("undeclared"), "visible values stay allowlisted");
        assert!(!calls[0].contains_key("_csrf"), "CSRF never reaches the module");
    }

    assert_eq!(action_rows(&pool, "target", &slug, "form-submit").await, 0);
    cleanup_user(&pool, &user).await;
    cleanup_ip(&pool, peer_ip).await;
}

/// `ADMIN_OPEN=1` bypasses sessions AND CSRF: pages render with no cookie, and a
/// local form posts without `_csrf` (actor recorded as `local-admin`).
#[tokio::test(flavor = "multi_thread")]
async fn admin_open_bypasses_sessions_and_csrf() {
    let Some(pool) = test_pool().await else { return };
    let (ctx, st) = wired(&pool, true, true).await;

    let get = send(&st, form_req("GET", "/admin", "203.0.113.18:9999", None, None)).await;
    assert_eq!(get.status(), StatusCode::OK, "open portal renders without a session");

    let submitted: Arc<Mutex<Vec<adminapi::Params>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = submitted.clone();
    let label = uniq("Open Form");
    let slug = slugify(&label);
    let submit: adminapi::SubmitFn = Arc::new(move |values: adminapi::Params| {
        let sink = sink.clone();
        Box::pin(async move {
            sink.lock().unwrap().push(values);
            Ok(adminapi::SubmitOutcome::default())
        }) as BoxFuture<'static, Result<adminapi::SubmitOutcome, adminapi::SubmitError>>
    });
    ctx.contribute(
        adminapi::SLOT,
        adminapi::Item::local(
            "openform",
            "S",
            &label,
            Arc::new(move |_p: &adminapi::Params| {
                Ok(adminapi::Content {
                    kpis: Vec::new(),
                    table: None,
                    form: Some(adminapi::Form {
                        action: String::new(),
                        fields: vec![adminapi::Field {
                            name: "knob".into(),
                            label: "Knob".into(),
                            value: String::new(),
                            ..Default::default()
                        }],
                        hidden: Vec::new(),
                        submit: Some(submit.clone()),
                    }),
                })
            }),
        ),
    );

    let post = send(
        &st,
        form_req("POST", &format!("/admin/{slug}"), "203.0.113.18:9999", None, Some("knob=v".to_string())),
    )
    .await;
    assert_eq!(post.status(), StatusCode::SEE_OTHER, "no session, no _csrf — open mode submits");
    assert_eq!(submitted.lock().unwrap().len(), 1);
    assert_eq!(action_rows(&pool, "target", &slug, "form-submit").await, 1);

    let _ = sqlx::query(
        "DELETE FROM asyncevents.events WHERE topic = 'admin.action' AND payload->>'target' = $1",
    )
    .bind(&slug)
    .execute(&pool)
    .await;
}

/// Logout: CSRF-gated, deletes the session row, clears the cookie, emits the
/// durable `logout`.
#[tokio::test(flavor = "multi_thread")]
async fn logout_deletes_session_clears_cookie_and_emits() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let user = uniq("t-logout");
    create_user(&pool, &user, "right").await;

    let resp = post_login(&st, "203.0.113.19:9999", &user, "right").await;
    let token = token_of(&set_cookie(&resp));
    let cookie = format!("admin_session={token}");
    let csrf = csrf_of(&pool, &token).await;

    // Without _csrf → 403, session intact.
    let bad = send(
        &st,
        form_req("POST", "/admin/logout", "203.0.113.19:9999", Some(&cookie), Some(String::new())),
    )
    .await;
    assert_eq!(bad.status(), StatusCode::FORBIDDEN);

    let good = send(
        &st,
        form_req(
            "POST",
            "/admin/logout",
            "203.0.113.19:9999",
            Some(&cookie),
            Some(format!("_csrf={csrf}")),
        ),
    )
    .await;
    assert_eq!(good.status(), StatusCode::SEE_OTHER);
    assert_eq!(location_of(&good), "/admin/login");
    let sc = set_cookie(&good);
    assert!(sc.contains("Max-Age=0"), "clearing cookie: {sc}");

    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM admin.sessions WHERE token = $1")
        .bind(&token)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0, "session row deleted");
    assert_eq!(action_rows(&pool, "actor", &user, "logout").await, 1);

    cleanup_user(&pool, &user).await;
    cleanup_ip(&pool, "203.0.113.19").await;
}

/// Zero admin users is a WARNED boot, never a failure: the full lifecycle prefix
/// (`register` → `migrate` → `start`) succeeds against a live DB regardless of the
/// user count — the old `ADMIN_USER` fail-closed env gate is gone.
#[tokio::test(flavor = "multi_thread")]
async fn zero_user_boot_is_allowed() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;

    let ctx = Context::with_db(pool.clone());
    let admin = Admin::new();
    admin.register(&ctx).unwrap();
    {
        // `init` reads process env synchronously; participate in the env-test lock so
        // this lifecycle test cannot observe another test's temporary dev knob.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        admin.init(&ctx).unwrap();
    }
    assert!(!login_reaper_is_owned(&admin), "init must wire without spawning background work");
    admin.migrate(&ctx).await.unwrap();
    admin.start(&ctx).await.expect("start must succeed with zero users (warn only)");
    assert!(login_reaper_is_owned(&admin), "a successful start must own the limiter reaper");
    admin.stop(&ctx).await.unwrap();
    assert!(!login_reaper_is_owned(&admin), "stop must await and release the limiter reaper");
}

#[tokio::test(flavor = "multi_thread")]
async fn migrate_is_idempotent_including_attempt_retention_index() {
    let Some(pool) = test_pool().await else { return };
    let ctx = Context::with_db(pool);
    let admin = Admin::new();
    admin.register(&ctx).unwrap();
    admin.migrate(&ctx).await.unwrap();
    admin.migrate(&ctx).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_and_invalid_usernames_never_create_user_attempt_rows() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let ghost = uniq("t-no-ghost-row");
    let ip = "203.0.113.81";

    let unknown = post_login(&st, &format!("{ip}:9999"), &ghost, "wrong").await;
    assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
    let invalid_name = "x".repeat(129);
    let invalid = post_login(&st, &format!("{ip}:9999"), &invalid_name, "wrong").await;
    assert_eq!(invalid.status(), StatusCode::UNAUTHORIZED);

    assert_eq!(attempts_row(&pool, &format!("user:{ghost}")).await, None);
    assert_eq!(attempts_row(&pool, &format!("user:{invalid_name}")).await, None);
    assert_eq!(attempts_row(&pool, &format!("ip:{ip}")).await, Some((2, false)));
    cleanup_ip(&pool, ip).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn login_ip_limiter_returns_exact_retry_after() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    let st = Arc::new(AdminState {
        env: template_env().unwrap(),
        slots: ctx.slots().clone(),
        pool,
        bus: ctx.bus().clone(),
        open: false,
        cookie_secure: true,
        trusted: Vec::new(),
        login_slots: Arc::new(tokio::sync::Semaphore::new(32)),
        argon_permits: Arc::new(tokio::sync::Semaphore::new(2)),
        login_limiter: httpmw::IpLimiter::new(0.0, 1),
        login_attempt_gc_requests: AtomicU64::new(0),
        verifier: Arc::new(ArgonVerifier),
    });
    let peer = "203.0.113.82:9999";
    assert_eq!(post_login(&st, peer, "ghost", "wrong").await.status(), StatusCode::UNAUTHORIZED);
    let denied = post_login(&st, peer, "ghost", "wrong").await;
    assert_eq!(denied.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(denied.headers()[header::RETRY_AFTER], "1");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_failures_stop_exactly_at_user_threshold_and_emit_once() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let user = uniq("t-parallel-lock");
    let ip = "203.0.113.83";
    create_user(&pool, &user, "right").await;

    let calls = (0..12).map(|i| {
        let st = st.clone();
        let user = user.clone();
        async move {
            post_login(&st, &format!("{ip}:{}", 9000 + i), &user, "wrong").await.status()
        }
    });
    let statuses = futures::future::join_all(calls).await;
    assert!(statuses.iter().all(|status| *status == StatusCode::UNAUTHORIZED));
    assert_eq!(
        attempts_row(&pool, &format!("user:{user}")).await,
        Some((USER_LOCK_THRESHOLD, true))
    );
    assert_eq!(action_rows(&pool, "actor", &user, "login-locked").await, 1);

    cleanup_user(&pool, &user).await;
    cleanup_ip(&pool, ip).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn verifier_runs_exactly_once_for_every_denial_shape() {
    let Some(pool) = test_pool().await else { return };
    let verifier = Arc::new(RecordingVerifier::default());
    let (_ctx, st) = wired_with_verifier(&pool, false, true, verifier.clone()).await;
    let known = uniq("t-verify-known");
    let user_locked = uniq("t-verify-userlocked");
    let ip_locked_user = uniq("t-verify-iplocked");
    create_user(&pool, &known, "right").await;
    create_user(&pool, &user_locked, "right").await;
    create_user(&pool, &ip_locked_user, "right").await;
    sqlx::query("INSERT INTO admin.login_attempts(subject,fails,locked_until) VALUES ($1,5,now()+interval '10 minutes')")
        .bind(format!("user:{user_locked}"))
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO admin.login_attempts(subject,fails,locked_until) VALUES ($1,20,now()+interval '10 minutes')")
        .bind("ip:203.0.113.94")
        .execute(&pool).await.unwrap();

    let invalid_username = "x".repeat(129);
    let cases = [
        ("203.0.113.90:1", known.as_str(), "known-secret"),
        ("203.0.113.91:1", "unknown-structural", "unknown-secret"),
        ("203.0.113.92:1", user_locked.as_str(), "locked-secret"),
        ("203.0.113.94:1", ip_locked_user.as_str(), "ip-locked-secret"),
        ("203.0.113.95:1", invalid_username.as_str(), "invalid-secret"),
    ];
    for (peer, user, pass) in cases {
        assert_eq!(post_login(&st, peer, user, pass).await.status(), StatusCode::UNAUTHORIZED);
    }
    {
        let calls = verifier.calls.lock().unwrap();
        assert_eq!(calls.len(), 5, "one verifier call per admitted request");
        assert_eq!(calls[0].1, "known-secret");
        for (_, candidate) in &calls[1..] {
            assert_eq!(candidate, "admin-invalid-credentials");
        }
    }

    for user in [&known, &user_locked, &ip_locked_user] { cleanup_user(&pool, user).await; }
    for ip in ["203.0.113.90", "203.0.113.91", "203.0.113.92", "203.0.113.94", "203.0.113.95"] {
        cleanup_ip(&pool, ip).await;
    }
}

struct SlowVerifier;
impl PasswordVerifier for SlowVerifier {
    fn verify(&self, _encoded: &str, _password: &str) -> bool {
        std::thread::sleep(Duration::from_millis(150));
        false
    }
}

#[tokio::test(flavor = "current_thread")]
async fn blocking_password_verify_does_not_stall_runtime_heartbeat() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired_with_verifier(&pool, false, true, Arc::new(SlowVerifier)).await;
    let login = post_login(&st, "203.0.113.96:1", "unknown-heartbeat", "wrong");
    let heartbeat = async {
        tokio::time::sleep(Duration::from_millis(30)).await;
        1
    };
    let (response, beat) = tokio::join!(login, heartbeat);
    assert_eq!(beat, 1);
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    cleanup_ip(&pool, "203.0.113.96").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crossed_user_and_ip_lock_orders_do_not_deadlock() {
    let Some(pool) = test_pool().await else { return };
    let verifier = Arc::new(RecordingVerifier::default());
    let (_ctx, st) = wired_with_verifier(&pool, false, true, verifier).await;
    let shared_user = uniq("t-two-ip");
    let user_a = uniq("t-one-ip-a");
    let user_b = uniq("t-one-ip-b");
    for user in [&shared_user, &user_a, &user_b] { create_user(&pool, user, "right").await; }

    let calls = vec![
        post_login(&st, "203.0.113.101:1", &shared_user, "wrong"),
        post_login(&st, "203.0.113.102:1", &shared_user, "wrong"),
        post_login(&st, "203.0.113.103:1", &user_a, "wrong"),
        post_login(&st, "203.0.113.103:2", &user_b, "wrong"),
    ];
    let responses = tokio::time::timeout(Duration::from_secs(3), futures::future::join_all(calls))
        .await.expect("sorted advisory locks must not deadlock");
    assert!(responses.iter().all(|r| r.status() == StatusCode::UNAUTHORIZED));

    for user in [&shared_user, &user_a, &user_b] { cleanup_user(&pool, user).await; }
    for ip in ["203.0.113.101", "203.0.113.102", "203.0.113.103"] { cleanup_ip(&pool, ip).await; }
}

#[tokio::test(flavor = "multi_thread")]
async fn login_attempt_gc_removes_only_stale_unlocked_rows() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, st) = wired(&pool, false, true).await;
    let stale = format!("ip:198.51.100.{}", UNIQ.fetch_add(1, Ordering::Relaxed) % 200 + 1);
    let locked = format!("user:{}", uniq("t-gc-locked"));
    let fresh = format!("user:{}", uniq("t-gc-fresh"));
    sqlx::query("INSERT INTO admin.login_attempts(subject,fails,updated_at) VALUES ($1,1,now()-interval '25 hours'),($2,5,now()-interval '25 hours'),($3,1,now())")
        .bind(&stale).bind(&locked).bind(&fresh).execute(&pool).await.unwrap();
    sqlx::query("UPDATE admin.login_attempts SET locked_until=now()+interval '1 hour' WHERE subject=$1")
        .bind(&locked).execute(&pool).await.unwrap();
    st.cleanup_login_attempts().await;
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM admin.login_attempts WHERE subject=ANY($1)")
        .bind([&locked, &fresh]).fetch_one(&pool).await.unwrap();
    assert_eq!(count, 2);
    assert_eq!(attempts_row(&pool, &stale).await, None);
    sqlx::query("DELETE FROM admin.login_attempts WHERE subject=ANY($1)")
        .bind([stale, locked, fresh]).execute(&pool).await.unwrap();
}

/// A verifier that reports when `verify` has started and then blocks until the test
/// releases it — lets the test freeze a login mid-Argon2 deterministically.
struct GatedVerifier {
    started: std::sync::mpsc::Sender<()>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl PasswordVerifier for GatedVerifier {
    fn verify(&self, _encoded: &str, _password: &str) -> bool {
        self.started.send(()).expect("test alive");
        let _ = self.release.lock().unwrap().recv();
        false
    }
}

/// The RAM-cap regression: `spawn_blocking` is NOT cancelled when its JoinHandle
/// drops, so if the argon permit lived in the handler's async frame a client
/// disconnect would release it while the detached 64 MiB hash keeps running. The
/// permit must be owned by the blocking closure — released only AFTER the hash
/// completes, even when the caller future is dropped mid-verify.
#[tokio::test(flavor = "multi_thread")]
async fn argon_permit_survives_login_cancellation_until_hash_completes() {
    let Some(pool) = test_pool().await else { return };
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let verifier = Arc::new(GatedVerifier {
        started: started_tx,
        release: Mutex::new(release_rx),
    });
    let (_ctx, st) = wired_with_verifier(&pool, false, true, verifier).await;
    assert_eq!(st.argon_permits.available_permits(), 2);

    let task_st = st.clone();
    let login = tokio::spawn(async move {
        post_login(&task_st, "203.0.113.97:1", "unknown-cancelled", "wrong").await
    });
    // Wait until the login is provably inside the blocking verify.
    tokio::task::spawn_blocking(move || started_rx.recv().expect("verify started"))
        .await
        .unwrap();
    assert_eq!(st.argon_permits.available_permits(), 1);

    // Simulate the client disconnect: abort drops the handler future at its
    // `.await` on the spawn_blocking JoinHandle; the blocking hash keeps running.
    login.abort();
    let err = login.await.expect_err("login task was aborted mid-verify");
    assert!(err.is_cancelled());
    assert_eq!(
        st.argon_permits.available_permits(),
        1,
        "cancelling the request must NOT release the argon permit while the hash still runs"
    );

    // Let the hash finish; only then may the permit return.
    release_tx.send(()).expect("verifier still blocked");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while st.argon_permits.available_permits() != 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "permit was not released after the blocking verify completed"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    cleanup_ip(&pool, "203.0.113.97").await;
}
