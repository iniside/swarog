//! `admin` — the GameOps admin PORTAL module (port of Go's `modules/admin`). It owns
//! the LOOK (the embedded dark theme + the sidebar/header shell) and composes a
//! navigable model from the items modules CONTRIBUTE to [`adminapi::SLOT`]: items are
//! grouped by [`adminapi::Item::section`] into the sidebar, and each opens its own
//! page (`GET /admin/{slug}`). A module appears here without the admin being edited —
//! it reads CONTRIBUTIONS, never a module's implementation or another schema.
//!
//! Two item kinds, resolved by [`resolve_items`]:
//!   - **LOCAL** (`render` set) — the module's in-process closure, called lazily at
//!     page render, carrying the request's query params so a `Render` can switch on a
//!     drill-down key (`?owner=…`).
//!   - **REMOTE** (`remote_fetch` set) — fetched now over the QUIC edge (in a split
//!     process each provider stub contributes one). Its Section/Label/Content come
//!     from the peer's [`adminapi::ItemData`]; [`adminapi::ItemError::Absent`] drops
//!     the item silently, any other failure keeps it as an error card (a down peer
//!     never blanks `/admin`).
//!
//! Routes (mounted via `ctx.mount`): `GET /admin/theme.css` (ungated), `GET /admin`
//! (redirect to the first item), `GET /admin/{slug}`, `POST /admin/{slug}` (LOCAL
//! form submit only; 405 for remote/non-form). Basic-auth gate `ADMIN_USER`/
//! `ADMIN_PASS` — required by default: an empty `ADMIN_USER` FAILS STARTUP unless
//! `ADMIN_OPEN=1` is explicitly set (a deliberately open local portal, loud warn).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Form, Router};
use base64::Engine as _;
use contrib::Slots;
use lifecycle::{Context, Module};
use serde::Serialize;

/// The admin page template (Go's `admin.html.tmpl`, adapted to minijinja: `range`→
/// `for`, `with .Page`→`if page`, and the two `define "cell"` blocks→macros). Named
/// with a `.html` suffix so minijinja auto-escapes value interpolations (matching
/// Go's `html/template` contextual escaping of player-supplied text in tables).
const TEMPLATE: &str = include_str!("admin.html.tmpl");

/// The embedded dark GameOps theme (copied verbatim from Go's `theme.css`). Served
/// ungated at `/admin/theme.css`.
const THEME_CSS: &str = include_str!("theme.css");

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

/// The admin portal module. Holds nothing until `init`; the per-request state (the
/// compiled template env, the slot registry the sidebar is composed from, and the
/// Basic-auth creds) lives in the [`AdminState`] captured by the mounted router.
#[derive(Default)]
pub struct Admin;

impl Admin {
    pub fn new() -> Self {
        Admin
    }
}

#[async_trait::async_trait]
impl Module for Admin {
    fn name(&self) -> &str {
        "admin"
    }

    /// Compiles the template once, reads the Basic-auth creds, and mounts the four
    /// `/admin` routes on the shared router. No I/O. The route table reads
    /// contributions lazily on each request, so a module contributing after the
    /// admin's `init` still appears (both run before the server accepts requests).
    ///
    /// Fail-closed: an empty `ADMIN_USER` is a startup failure unless `ADMIN_OPEN=1`
    /// is explicitly set (a deliberately open local portal — loud warn), mirroring the
    /// apikeys / gateway explicit-opt-in convention.
    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let mut env = minijinja::Environment::new();
        env.add_template("admin.html", TEMPLATE)
            .map_err(|e| anyhow::anyhow!("admin: template compile: {e}"))?;

        let auth_user = std::env::var("ADMIN_USER").unwrap_or_default();
        let auth_pass = std::env::var("ADMIN_PASS").unwrap_or_default();
        if auth_user.is_empty() {
            if !admin_open_explicitly_on() {
                anyhow::bail!(
                    "admin: set ADMIN_USER/ADMIN_PASS or ADMIN_OPEN=1 for a deliberately open local portal"
                );
            }
            tracing::warn!(
                "admin portal is UNAUTHENTICATED (ADMIN_OPEN=1) — no Basic-auth gate; intended for local use only"
            );
        }

        let user = UserView::new(&auth_user);
        let state = Arc::new(AdminState {
            env,
            slots: ctx.slots().clone(),
            auth_user,
            auth_pass,
            user,
        });

        ctx.mount(router(state));
        Ok(())
    }
}

/// Per-request admin state captured by the router closures (the analogue of Go's
/// `admin.Module` fields). `slots` is read on each request so newly-contributed
/// items appear without a restart.
struct AdminState {
    env: minijinja::Environment<'static>,
    slots: Arc<Slots>,
    auth_user: String,
    auth_pass: String,
    user: UserView,
}

/// Builds the `/admin` router. `theme.css` is ungated (a stylesheet leaks nothing);
/// the three page routes are gated per request by [`AdminState::check_auth`]. The
/// static `/admin/theme.css` is registered before the `/admin/:slug` param route so
/// matchit prefers it (static wins over a param at the same position).
fn router(state: Arc<AdminState>) -> Router {
    Router::new()
        .route("/admin/theme.css", get(theme_css))
        .route("/admin", get(index))
        .route("/admin/:slug", get(item).post(item_post))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /admin/theme.css` — the embedded stylesheet, ungated.
async fn theme_css() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        THEME_CSS,
    )
        .into_response()
}

/// `GET /admin` — redirect to the first resolved item's page, or render an empty
/// shell when nothing is contributed. 302 (Go's `StatusFound`).
async fn index(
    State(st): State<Arc<AdminState>>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Some(resp) = st.check_auth(&headers) {
        return resp;
    }
    let items = resolve_items(&st, &params).await;
    if items.is_empty() {
        return render_page(
            &st,
            PageData {
                crumb: "Admin".into(),
                title: "Admin".into(),
                env: "Local".into(),
                user: st.user.clone(),
                groups: Vec::new(),
                page: None,
            },
        );
    }
    let loc = format!("/admin/{}", items[0].slug);
    (
        StatusCode::FOUND,
        [(header::LOCATION, HeaderValue::from_str(&loc).unwrap())],
    )
        .into_response()
}

/// `GET /admin/{slug}` — render one item's page. A LOCAL item's `render` is called
/// here (lazily, with the query params); a REMOTE item's content was already fetched
/// in [`resolve_items`].
async fn item(
    State(st): State<Arc<AdminState>>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Some(resp) = st.check_auth(&headers) {
        return resp;
    }
    let items = resolve_items(&st, &params).await;
    let Some(cur) = items.iter().find(|r| r.slug == slug) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };

    let page = page_view(cur, &params, &slug);
    let groups = build_groups(&items, &slug);
    render_page(
        &st,
        PageData {
            crumb: cur.section.clone(),
            title: cur.label.clone(),
            env: "Local".into(),
            user: st.user.clone(),
            groups,
            page: Some(page),
        },
    )
}

/// `POST /admin/{slug}` — apply a LOCAL item's editable form. Resolves the item,
/// reaches its `Form` via the (idempotent) render closure, invokes `submit`, and on
/// success redirects (303) back to the GET. Remote and non-form items are 405.
async fn item_post(
    State(st): State<Arc<AdminState>>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
    Form(body): Form<HashMap<String, String>>,
) -> Response {
    if let Some(resp) = st.check_auth(&headers) {
        return resp;
    }
    let items = resolve_items(&st, &params).await;
    let Some(cur) = items.iter().find(|r| r.slug == slug) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };

    // Only LOCAL items with a render closure can be edited.
    let Some(render) = cur.item.render.clone() else {
        return (StatusCode::METHOD_NOT_ALLOWED, "not editable").into_response();
    };

    let content = match render(&params) {
        Ok(c) => c,
        Err(e) => return render_error(&st, cur, &slug, &items, format!("failed to load: {e}")),
    };
    let Some(form) = content.form else {
        return (StatusCode::METHOD_NOT_ALLOWED, "not editable").into_response();
    };
    let Some(submit) = form.submit.clone() else {
        return (StatusCode::METHOD_NOT_ALLOWED, "not editable").into_response();
    };

    // Collect exactly the declared fields (mirrors Go's `r.PostFormValue(f.Name)`).
    let mut values = adminapi::Params::new();
    for f in &form.fields {
        values.insert(f.name.clone(), body.get(&f.name).cloned().unwrap_or_default());
    }

    match submit(values).await {
        Ok(()) => {
            let loc = format!("/admin/{slug}");
            (
                StatusCode::SEE_OTHER,
                [(header::LOCATION, HeaderValue::from_str(&loc).unwrap())],
            )
                .into_response()
        }
        Err(e) => render_error(&st, cur, &slug, &items, format!("save failed: {e}")),
    }
}

/// Re-renders the current page with an error card (the POST failure path).
fn render_error(
    st: &AdminState,
    cur: &Resolved,
    slug: &str,
    items: &[Resolved],
    msg: String,
) -> Response {
    let groups = build_groups(items, slug);
    render_page(
        st,
        PageData {
            crumb: cur.section.clone(),
            title: cur.label.clone(),
            env: "Local".into(),
            user: st.user.clone(),
            groups,
            page: Some(PageView {
                title: cur.label.clone(),
                err: msg,
                kpis: Vec::new(),
                table: None,
                form: None,
            }),
        },
    )
}

// ---------------------------------------------------------------------------
// Item resolution (the fan-out) + pure view helpers
// ---------------------------------------------------------------------------

/// A remote item's fetched outcome: the content, or the transport error string that
/// becomes an "unavailable" error card.
enum RemoteResult {
    Ok(adminapi::Content),
    Err(String),
}

/// One resolved sidebar entry ready to render (Go's `resolvedItem`). `item` carries
/// the original contribution (its `render`/`submit` closures for the LOCAL path);
/// `remote` is `Some` for a REMOTE item (already fetched).
struct Resolved {
    section: String,
    label: String,
    slug: String,
    item: adminapi::Item,
    remote: Option<RemoteResult>,
}

/// Resolves the contributed admin items into ordered [`Resolved`] entries with unique
/// slugs (first-seen order; collisions get `-2`, `-3`, …; empty→`item`). A LOCAL item
/// keeps its `render` closure; a REMOTE item is fetched now over the edge — an
/// [`adminapi::ItemError::Absent`] drops it silently, any other error keeps it as an
/// error card (Label falls back to ID). Fetching per request is fine: `/admin` is
/// low-traffic.
async fn resolve_items(st: &AdminState, params: &adminapi::Params) -> Vec<Resolved> {
    let items: Vec<adminapi::Item> = st.slots.contributions(adminapi::SLOT);
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<Resolved> = Vec::new();

    for it in items {
        let (section, label, remote) = if let Some(fetch) = it.remote_fetch.clone() {
            match fetch(params.clone()).await {
                Err(adminapi::ItemError::Absent) => continue, // no admin surface → skip
                Err(e) => (it.id.clone(), it.id.clone(), Some(RemoteResult::Err(format!("{e}")))),
                Ok(data) => (data.section, data.label, Some(RemoteResult::Ok(data.content))),
            }
        } else {
            (it.section.clone(), it.label.clone(), None)
        };

        let mut base = slugify(&label);
        if base.is_empty() {
            base = "item".into();
        }
        let mut slug = base.clone();
        let mut n = 2;
        while seen.contains(&slug) {
            slug = format!("{base}-{n}");
            n += 1;
        }
        seen.insert(slug.clone());

        out.push(Resolved {
            section,
            label,
            slug,
            item: it,
            remote,
        });
    }
    out
}

/// Builds the [`PageView`] for one resolved item: the remote content (or its fetch
/// error), else the LOCAL render closure called with the request's query params.
fn page_view(cur: &Resolved, params: &adminapi::Params, slug: &str) -> PageView {
    match &cur.remote {
        Some(RemoteResult::Err(msg)) => PageView {
            title: cur.label.clone(),
            err: format!("unavailable: {msg}"),
            kpis: Vec::new(),
            table: None,
            form: None,
        },
        // A remote item's form arrives read-only (its `submit` cannot marshal), so
        // remote pages render KPIs + table only (Go dropped the remote form too).
        Some(RemoteResult::Ok(content)) => PageView {
            title: cur.label.clone(),
            err: String::new(),
            kpis: content.kpis.clone(),
            table: content.table.clone(),
            form: None,
        },
        None => match &cur.item.render {
            Some(render) => match render(params) {
                Ok(content) => {
                    let form = content.form.map(|mut f| {
                        f.action = format!("/admin/{slug}");
                        f
                    });
                    PageView {
                        title: cur.label.clone(),
                        err: String::new(),
                        kpis: content.kpis,
                        table: content.table,
                        form,
                    }
                }
                Err(e) => PageView {
                    title: cur.label.clone(),
                    err: format!("failed to load: {e}"),
                    kpis: Vec::new(),
                    table: None,
                    form: None,
                },
            },
            // Neither a closure nor a remote result (a metadata-only local item).
            None => PageView {
                title: cur.label.clone(),
                err: String::new(),
                kpis: Vec::new(),
                table: None,
                form: None,
            },
        },
    }
}

/// Groups items by section preserving first-seen section order, marking the item
/// whose slug matches `active` (Go's `buildGroups`).
fn build_groups(items: &[Resolved], active: &str) -> Vec<NavGroup> {
    let mut groups: Vec<NavGroup> = Vec::new();
    let mut idx: HashMap<String, usize> = HashMap::new();
    for it in items {
        let i = match idx.get(&it.section) {
            Some(&i) => i,
            None => {
                let i = groups.len();
                idx.insert(it.section.clone(), i);
                groups.push(NavGroup {
                    section: it.section.clone(),
                    items: Vec::new(),
                });
                i
            }
        };
        groups[i].items.push(NavItem {
            label: it.label.clone(),
            slug: it.slug.clone(),
            active: it.slug == active,
        });
    }
    groups
}

/// Lowercases `s`, keeps `[a-z0-9]`, maps space/`-`/`_`→`-`, drops other runes, and
/// trims leading/trailing `-` (Go's `slugify`, byte-for-byte on the ASCII cases).
fn slugify(s: &str) -> String {
    let mut b = String::new();
    for r in s.to_lowercase().chars() {
        if r.is_ascii_lowercase() || r.is_ascii_digit() {
            b.push(r);
        } else if r == ' ' || r == '-' || r == '_' {
            b.push('-');
        }
    }
    b.trim_matches('-').to_string()
}

// ---------------------------------------------------------------------------
// Rendering + auth
// ---------------------------------------------------------------------------

/// Renders the template with `data` into an HTML response; a template error becomes a
/// 500 (should never happen — the template is compile-time embedded).
fn render_page(st: &AdminState, data: PageData) -> Response {
    match st.env.get_template("admin.html").and_then(|t| t.render(&data)) {
        Ok(html) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            html,
        )
            .into_response(),
        Err(e) => {
            tracing::error!(err = %e, "admin render failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "render failed").into_response()
        }
    }
}

impl AdminState {
    /// Applies HTTP Basic auth when `ADMIN_USER` is configured; otherwise open (only
    /// reachable under the explicit `ADMIN_OPEN=1` escape) for local use. Returns
    /// `Some(401 response)` to write on a
    /// missing/mismatched credential, `None` when the request may proceed.
    fn check_auth(&self, headers: &HeaderMap) -> Option<Response> {
        if self.auth_user.is_empty() {
            return None;
        }
        let ok = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Basic "))
            .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|creds| {
                creds.split_once(':').map(|(u, p)| {
                    ct_eq(u.as_bytes(), self.auth_user.as_bytes())
                        && ct_eq(p.as_bytes(), self.auth_pass.as_bytes())
                })
            })
            .unwrap_or(false);
        if ok {
            None
        } else {
            let mut resp = (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
            resp.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Basic realm=\"admin\""),
            );
            Some(resp)
        }
    }
}

/// Length-checked constant-time byte compare (Go's `subtle.ConstantTimeCompare`):
/// differing lengths are unequal, equal lengths compared without an early exit.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// `true` only when `ADMIN_OPEN` is EXPLICITLY set truthy (`1`/`true`/`on`,
/// case-insensitive). Unset is `false` — an unauthenticated admin portal is a
/// trust decision, so this follows the explicit-only convention (apikeys'
/// `dev_seed_explicitly_on`, gateway's `dev_auth_explicitly_on`), NOT a default-open.
fn admin_open_explicitly_on() -> bool {
    matches!(
        std::env::var("ADMIN_OPEN"),
        Ok(v) if v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
    )
}

// ---------------------------------------------------------------------------
// Template view models (serde → minijinja)
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
struct UserView {
    name: String,
    initials: String,
}

impl UserView {
    /// The footer/avatar identity: the configured admin user's name + up-to-2-char
    /// initials, else the "Local Admin"/"LA" default when unauthenticated (Go's
    /// `newUser`).
    fn new(name: &str) -> UserView {
        if name.is_empty() {
            return UserView {
                name: "Local Admin".into(),
                initials: "LA".into(),
            };
        }
        let mut ini = name.to_uppercase();
        if ini.chars().count() > 2 {
            ini = ini.chars().take(2).collect();
        }
        UserView {
            name: name.to_string(),
            initials: ini,
        }
    }
}

#[derive(Serialize)]
struct NavItem {
    label: String,
    slug: String,
    active: bool,
}

#[derive(Serialize)]
struct NavGroup {
    section: String,
    items: Vec<NavItem>,
}

#[derive(Serialize)]
struct PageView {
    title: String,
    err: String,
    kpis: Vec<adminapi::Kpi>,
    table: Option<adminapi::Table>,
    form: Option<adminapi::Form>,
}

#[derive(Serialize)]
struct PageData {
    crumb: String,
    title: String,
    env: String,
    user: UserView,
    groups: Vec<NavGroup>,
    page: Option<PageView>,
}

// ============================================================================
// Tests. The pure helpers (slugify, build_groups, resolve_items) are exercised
// in-crate against a real `Slots` populated through a `lifecycle::Context` — no DB,
// no network (LOCAL renders + REMOTE fetches use plain closures).
// ============================================================================
#[cfg(test)]
mod tests;
