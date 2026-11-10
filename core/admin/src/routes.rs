//! The control-plane portal routes. Each handler resolves a typed view model through the
//! engine and renders it as an interim plain-text status listing.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use controlplane::{
    ActionDecl, ActionPoint, ContributionDecl, EntityKindDecl, EntityRef, InvokeOutcome, Layout, PageDecl,
    PanelRequest, Point,
};
use serde::Serialize;

use crate::engine::actions::{self, BuildError, Flash, Invoked, Refusal};
use crate::engine::manifests::{NavItem, Snapshot};
use crate::engine::panels::{self, PanelView};
use crate::engine::roles::{self, Operator};
use crate::engine::search::{self, SearchGroup};
use crate::engine::Engine;
use crate::{see_other, AdminState};

#[derive(Clone)]
pub(crate) struct Portal {
    pub(crate) st: Arc<AdminState>,
    pub(crate) engine: Arc<Engine>,
}

pub(crate) fn router(portal: Portal) -> Router {
    Router::new()
        .route("/admin", get(dashboard))
        .route("/admin/p/:module/:page", get(page))
        .route("/admin/e/:kind/:id", get(entity))
        .route("/admin/_panel/:module/:contribution", get(panel))
        .route("/admin/_menu/:kind/:id", get(menu))
        .route("/admin/_search", get(search_route))
        .route("/admin/_action/:module/:action", get(action_modal).post(action_submit))
        .layer(middleware::from_fn(crate::security_headers))
        .with_state(portal)
}

// ---------------------------------------------------------------------------
// View models
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub(crate) struct OperatorView {
    pub(crate) login: String,
    pub(crate) role: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ShellView {
    pub(crate) operator: OperatorView,
    pub(crate) csrf: String,
    pub(crate) sections: Vec<controlplane::NavSection>,
    pub(crate) nav: Vec<NavItem>,
    pub(crate) flash: Option<Flash>,
}

/// A panel the view loads from `url`; `denied` panels are never fetched.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct SlotView {
    pub(crate) module: String,
    pub(crate) contribution: String,
    pub(crate) title: String,
    pub(crate) icon: String,
    pub(crate) url: String,
    pub(crate) denied: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ActionItem {
    pub(crate) module: String,
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) icon: String,
    pub(crate) danger: bool,
    pub(crate) url: String,
    pub(crate) enabled: bool,
    pub(crate) hint: Option<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DashboardView {
    pub(crate) shell: ShellView,
    pub(crate) tiles: Vec<SlotView>,
    pub(crate) attention: Vec<SlotView>,
    pub(crate) panels: Vec<SlotView>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PageView {
    pub(crate) shell: ShellView,
    pub(crate) module: String,
    pub(crate) page: PageDecl,
    pub(crate) head_actions: Vec<ActionItem>,
    /// Layout columns in declaration order, each a list of panels.
    pub(crate) columns: Vec<Vec<SlotView>>,
    /// Layout ids naming no contribution of this module.
    pub(crate) missing: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TabView {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) url: String,
    pub(crate) counts: Vec<SlotView>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct EntityView {
    pub(crate) shell: ShellView,
    pub(crate) kind: EntityKindDecl,
    pub(crate) entity: EntityRef,
    pub(crate) headers: Vec<SlotView>,
    pub(crate) summaries: Vec<SlotView>,
    pub(crate) header_actions: Vec<ActionItem>,
    pub(crate) tabs: Vec<TabView>,
    pub(crate) selected_tab: Option<String>,
    pub(crate) tab_panels: Vec<SlotView>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct MenuView {
    pub(crate) entity: EntityRef,
    pub(crate) actions: Vec<ActionItem>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SearchView {
    pub(crate) q: String,
    pub(crate) kind: String,
    pub(crate) groups: Vec<SearchGroup>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ActionModalView {
    pub(crate) module: String,
    pub(crate) action: ActionDecl,
    pub(crate) entity: Option<EntityRef>,
    pub(crate) csrf: String,
    pub(crate) post_url: String,
}

/// A non-redirect POST result: the 422 form errors, a 409, or the error toast.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ActionResultView {
    pub(crate) module: String,
    pub(crate) action: String,
    pub(crate) field_errors: BTreeMap<String, String>,
    pub(crate) current_revision: Option<u64>,
    pub(crate) error: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared resolution
// ---------------------------------------------------------------------------

struct Ctx {
    csrf: String,
    operator: Operator,
}

async fn authorize(p: &Portal, jar: &CookieJar, is_post: bool) -> Result<Ctx, Response> {
    let authed = p.st.gate(jar, is_post).await?;
    let operator = roles::resolve(&p.st, &authed).await?;
    Ok(Ctx { csrf: authed.csrf, operator })
}

async fn shell(p: &Portal, ctx: &Ctx, snapshot: &Snapshot, query: &HashMap<String, String>) -> ShellView {
    let flash = match query.get("reveal") {
        Some(token) => actions::take_flash(&p.st.pool, token).await,
        None => None,
    };
    ShellView {
        operator: OperatorView { login: ctx.operator.login.clone(), role: crate::role_name(ctx.operator.role) },
        csrf: ctx.csrf.clone(),
        sections: snapshot.sections.clone(),
        nav: snapshot.nav.clone(),
        flash,
    }
}

fn slot(operator: &Operator, module: &str, decl: &ContributionDecl, entity: Option<&EntityRef>) -> SlotView {
    let mut url = format!("/admin/_panel/{}/{}", actions::encode(module), actions::encode(&decl.id));
    if let Some(e) = entity {
        url.push_str(&format!("?entity={}", actions::encode(&format!("{}:{}", e.kind, e.id))));
    }
    SlotView {
        module: module.to_string(),
        contribution: decl.id.clone(),
        title: decl.title.clone(),
        icon: decl.icon.clone(),
        url,
        denied: !roles::allows(operator, decl.min_role),
    }
}

fn action_item(operator: &Operator, module: &str, decl: &ActionDecl, entity: Option<&EntityRef>) -> ActionItem {
    let mut url = format!("/admin/_action/{}/{}", actions::encode(module), actions::encode(&decl.id));
    if let Some(e) = entity {
        url.push_str(&format!("?entity={}", actions::encode(&format!("{}:{}", e.kind, e.id))));
    }
    let (enabled, hint) = match actions::admit(operator, decl) {
        Ok(()) => (true, None),
        Err(Refusal::Denied) => (false, Some("Not permitted for your role")),
        Err(Refusal::NotImplemented) => (false, Some("Not implemented yet")),
    };
    ActionItem {
        module: module.to_string(),
        id: decl.id.clone(),
        label: decl.label.clone(),
        icon: decl.icon.clone(),
        danger: decl.danger,
        url,
        enabled,
        hint,
    }
}

/// Every `(module, decl)` across all manifests matching `want`, sorted by `(order, module, id)`.
fn contributions<'a>(
    snapshot: &'a Snapshot,
    want: impl Fn(&Point) -> bool,
) -> Vec<(&'a str, &'a ContributionDecl)> {
    let mut found: Vec<(&str, &ContributionDecl)> = snapshot
        .modules
        .iter()
        .flat_map(|(m, r)| r.manifest.contributions.iter().map(move |c| (m.as_str(), c)))
        .filter(|(_, c)| want(&c.point))
        .collect();
    found.sort_by(|a, b| (a.1.order, a.0, &a.1.id).cmp(&(b.1.order, b.0, &b.1.id)));
    found
}

fn action_items(
    snapshot: &Snapshot,
    operator: &Operator,
    entity: Option<&EntityRef>,
    want: impl Fn(&ActionPoint) -> bool,
) -> Vec<ActionItem> {
    let mut found: Vec<(&str, &ActionDecl)> = snapshot
        .modules
        .iter()
        .flat_map(|(m, r)| r.manifest.actions.iter().map(move |a| (m.as_str(), a)))
        .filter(|(_, a)| want(&a.point))
        .collect();
    found.sort_by(|a, b| (a.1.order, a.0, &a.1.id).cmp(&(b.1.order, b.0, &b.1.id)));
    found.into_iter().map(|(m, a)| action_item(operator, m, a, entity)).collect()
}

// ---------------------------------------------------------------------------
// Interim rendering (plain text; the view models are the stable surface)
// ---------------------------------------------------------------------------

fn text(status: StatusCode, body: String) -> Response {
    (status, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
}

fn render_shell(out: &mut String, title: &str, shell: &ShellView) {
    let _ = writeln!(out, "{title}: working");
    let _ = writeln!(out, "operator: {} ({})", shell.operator.login, shell.operator.role);
    for item in &shell.nav {
        let state = if item.source_ok { "ok" } else { "not working" };
        let _ = writeln!(out, "nav {}/{}/{}: {state}", item.section, item.module, item.id);
    }
    if let Some(flash) = &shell.flash {
        let _ = writeln!(out, "flash: {}", flash.message);
        if let Some(reveal) = &flash.reveal {
            let _ = writeln!(out, "reveal {}: {}", reveal.label, reveal.value);
        }
    }
}

fn render_slots(out: &mut String, slots: &[SlotView]) {
    for s in slots {
        if s.denied {
            let _ = writeln!(out, "{}/{}: denied", s.module, s.contribution);
        } else {
            let _ = writeln!(out, "{}/{}: deferred {}", s.module, s.contribution, s.url);
        }
    }
}

fn render_actions(out: &mut String, items: &[ActionItem]) {
    for a in items {
        let state = a.hint.unwrap_or("enabled");
        let _ = writeln!(out, "action {}/{}: {state}", a.module, a.id);
    }
}

fn not_working(status: StatusCode, what: &str) -> Response {
    text(status, format!("{what}: not working\n"))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /admin`.
async fn dashboard(
    State(p): State<Portal>,
    jar: CookieJar,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let ctx = match authorize(&p, &jar, false).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let snapshot = p.engine.snapshot();
    let pick = |point: Point| {
        contributions(&snapshot, |pt| *pt == point)
            .into_iter()
            .map(|(m, c)| slot(&ctx.operator, m, c, None))
            .collect::<Vec<_>>()
    };
    let view = DashboardView {
        shell: shell(&p, &ctx, &snapshot, &query).await,
        tiles: pick(Point::DashboardTile),
        attention: pick(Point::DashboardAttention),
        panels: pick(Point::DashboardPanel),
    };
    let mut out = String::new();
    render_shell(&mut out, "dashboard", &view.shell);
    render_slots(&mut out, &view.tiles);
    render_slots(&mut out, &view.attention);
    render_slots(&mut out, &view.panels);
    text(StatusCode::OK, out)
}

/// `GET /admin/p/{module}/{page}`.
async fn page(
    State(p): State<Portal>,
    jar: CookieJar,
    Path((module, page_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let ctx = match authorize(&p, &jar, false).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let snapshot = p.engine.snapshot();
    let Some((manifest, _)) = snapshot.route(&module) else {
        return not_working(StatusCode::NOT_FOUND, &module);
    };
    let Some(decl) = manifest.pages.iter().find(|pg| pg.id == page_id) else {
        return not_working(StatusCode::NOT_FOUND, &format!("{module}/{page_id}"));
    };
    let ids: Vec<&Vec<String>> = match &decl.layout {
        Layout::Stack(a) | Layout::Tiles(a) => vec![a],
        Layout::Grid21(a, b) | Layout::Grid2(a, b) => vec![a, b],
    };
    let mut missing = Vec::new();
    let columns: Vec<Vec<SlotView>> = ids
        .into_iter()
        .map(|column| {
            column
                .iter()
                .filter_map(|id| {
                    let found = manifest.contributions.iter().find(|c| &c.id == id);
                    if found.is_none() {
                        missing.push(id.clone());
                    }
                    found.map(|c| slot(&ctx.operator, &module, c, None))
                })
                .collect()
        })
        .collect();
    let head_actions = decl
        .head_actions
        .iter()
        .filter_map(|id| manifest.actions.iter().find(|a| &a.id == id))
        .map(|a| action_item(&ctx.operator, &module, a, None))
        .collect();
    let view = PageView {
        shell: shell(&p, &ctx, &snapshot, &query).await,
        module: module.clone(),
        page: decl.clone(),
        head_actions,
        columns,
        missing,
    };
    let mut out = String::new();
    render_shell(&mut out, &format!("{}/{}", view.module, view.page.id), &view.shell);
    render_actions(&mut out, &view.head_actions);
    for column in &view.columns {
        render_slots(&mut out, column);
    }
    for id in &view.missing {
        let _ = writeln!(out, "{}/{id}: not working (no such contribution)", view.module);
    }
    text(StatusCode::OK, out)
}

/// `GET /admin/e/{kind}/{id}?tab=`.
async fn entity(
    State(p): State<Portal>,
    jar: CookieJar,
    Path((kind, id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let ctx = match authorize(&p, &jar, false).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let snapshot = p.engine.snapshot();
    let Some(kind_decl) = snapshot
        .modules
        .values()
        .flat_map(|r| r.manifest.entity_kinds.iter())
        .find(|k| k.kind == kind)
        .cloned()
    else {
        return not_working(StatusCode::NOT_FOUND, &format!("{kind}:{id}"));
    };
    let entity = EntityRef { kind: kind.clone(), id: id.clone() };
    let op = &ctx.operator;
    let headers = contributions(&snapshot, |pt| matches!(pt, Point::EntityHeader(k) if *k == kind))
        .into_iter()
        .map(|(m, c)| slot(op, m, c, Some(&entity)))
        .collect();
    let summaries = contributions(&snapshot, |pt| matches!(pt, Point::EntitySummary(k) if *k == kind))
        .into_iter()
        .map(|(m, c)| slot(op, m, c, Some(&entity)))
        .collect();
    let tab_decls = contributions(&snapshot, |pt| matches!(pt, Point::EntityTab { kind: k, .. } if *k == kind));
    let mut tabs: Vec<TabView> = Vec::new();
    for (_, c) in &tab_decls {
        if let Point::EntityTab { tab, tab_label, .. } = &c.point {
            if tabs.iter().all(|t| &t.id != tab) {
                let url = format!("{}?tab={}", actions::entity_url(&entity), actions::encode(tab));
                tabs.push(TabView { id: tab.clone(), label: tab_label.clone(), url, counts: Vec::new() });
            }
        }
    }
    for (m, c) in contributions(&snapshot, |pt| matches!(pt, Point::EntityTabCount { kind: k, .. } if *k == kind)) {
        if let Point::EntityTabCount { tab, .. } = &c.point {
            if let Some(t) = tabs.iter_mut().find(|t| &t.id == tab) {
                t.counts.push(slot(op, m, c, Some(&entity)));
            }
        }
    }
    let selected_tab = query
        .get("tab")
        .filter(|t| tabs.iter().any(|x| &&x.id == t))
        .cloned()
        .or_else(|| tabs.first().map(|t| t.id.clone()));
    let tab_panels = tab_decls
        .iter()
        .filter(|(_, c)| matches!(&c.point, Point::EntityTab { tab, .. } if Some(tab) == selected_tab.as_ref()))
        .map(|(m, c)| slot(op, m, c, Some(&entity)))
        .collect();
    let header_actions =
        action_items(&snapshot, op, Some(&entity), |pt| matches!(pt, ActionPoint::HeaderAction(k) if *k == kind));
    let view = EntityView {
        shell: shell(&p, &ctx, &snapshot, &query).await,
        kind: kind_decl,
        entity,
        headers,
        summaries,
        header_actions,
        tabs,
        selected_tab,
        tab_panels,
    };
    let mut out = String::new();
    render_shell(&mut out, &format!("{}:{}", view.entity.kind, view.entity.id), &view.shell);
    render_slots(&mut out, &view.headers);
    render_slots(&mut out, &view.summaries);
    render_actions(&mut out, &view.header_actions);
    for tab in &view.tabs {
        let _ = writeln!(out, "tab {}: {}", tab.id, tab.url);
        render_slots(&mut out, &tab.counts);
    }
    render_slots(&mut out, &view.tab_panels);
    text(StatusCode::OK, out)
}

/// `GET /admin/_panel/{module}/{contribution}?entity=kind:id&...` — one panel; every
/// state answers 200 so the fragment swaps in.
async fn panel(
    State(p): State<Portal>,
    jar: CookieJar,
    Path((module, contribution)): Path<(String, String)>,
    Query(mut query): Query<BTreeMap<String, String>>,
) -> Response {
    let ctx = match authorize(&p, &jar, false).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let snapshot = p.engine.snapshot();
    let Some(decl) = snapshot
        .route(&module)
        .and_then(|(m, _)| m.contributions.iter().find(|c| c.id == contribution))
        .cloned()
    else {
        return not_working(StatusCode::NOT_FOUND, &format!("{module}/{contribution}"));
    };
    let entity = match query.remove("entity") {
        None => None,
        Some(raw) => match actions::parse_entity(&raw) {
            Some(e) => Some(e),
            None => return text(StatusCode::BAD_REQUEST, "entity must be kind:id\n".to_string()),
        },
    };
    let req = PanelRequest { module: module.clone(), contribution, entity, params: query };
    let view: PanelView = panels::resolve(&p.engine, &snapshot, &ctx.operator, &module, &decl, req).await;
    let mut out = format!("{}/{}: {}\n", view.module, view.contribution, view.state.as_str());
    if let Some(error) = &view.error {
        let _ = writeln!(out, "error: {error}");
    }
    text(StatusCode::OK, out)
}

/// `GET /admin/_menu/{kind}/{id}` — the row actions for one entity.
async fn menu(State(p): State<Portal>, jar: CookieJar, Path((kind, id)): Path<(String, String)>) -> Response {
    let ctx = match authorize(&p, &jar, false).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let snapshot = p.engine.snapshot();
    let entity = EntityRef { kind: kind.clone(), id };
    let view = MenuView {
        actions: action_items(&snapshot, &ctx.operator, Some(&entity), |pt| {
            matches!(pt, ActionPoint::RowAction(k) if *k == kind)
        }),
        entity,
    };
    let mut out = format!("menu {}:{}: working\n", view.entity.kind, view.entity.id);
    render_actions(&mut out, &view.actions);
    text(StatusCode::OK, out)
}

/// `GET /admin/_search?q=&kind=`.
async fn search_route(
    State(p): State<Portal>,
    jar: CookieJar,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Err(r) = authorize(&p, &jar, false).await {
        return r;
    }
    let q = query.get("q").cloned().unwrap_or_default();
    let kind = query.get("kind").cloned().unwrap_or_default();
    if q.len() > search::MAX_QUERY_BYTES {
        return text(StatusCode::BAD_REQUEST, format!("q exceeds {} bytes\n", search::MAX_QUERY_BYTES));
    }
    let snapshot = p.engine.snapshot();
    let groups = search::run(&snapshot, &q, &kind).await;
    let view = SearchView { q, kind, groups };
    let mut out = String::from("search: working\n");
    for g in &view.groups {
        match &g.error {
            None => {
                let _ = writeln!(out, "{}/{}: ok {} hits", g.module, g.kind, g.hits.len());
            }
            Some(e) => {
                let _ = writeln!(out, "{}/{}: error {e}", g.module, g.kind);
            }
        }
        for hit in &g.hits {
            let _ = writeln!(out, "  {}:{} {}", hit.target.kind, hit.target.id, hit.title);
        }
    }
    text(StatusCode::OK, out)
}

/// `GET /admin/_action/{module}/{action}?entity=kind:id` — the modal's view model.
async fn action_modal(
    State(p): State<Portal>,
    jar: CookieJar,
    Path((module, action)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let ctx = match authorize(&p, &jar, false).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let snapshot = p.engine.snapshot();
    let Some(found) = actions::find(&snapshot, &module, &action) else {
        return not_working(StatusCode::NOT_FOUND, &format!("{module}/{action}"));
    };
    match actions::admit(&ctx.operator, found.decl) {
        Ok(()) => {}
        Err(Refusal::Denied) => return text(StatusCode::FORBIDDEN, format!("{module}/{action}: denied\n")),
        Err(Refusal::NotImplemented) => {
            return text(StatusCode::FORBIDDEN, format!("{module}/{action}: Not implemented yet\n"))
        }
    }
    let entity = match query.get("entity") {
        None => None,
        Some(raw) => match actions::parse_entity(raw) {
            Some(e) => Some(e),
            None => return text(StatusCode::BAD_REQUEST, "entity must be kind:id\n".to_string()),
        },
    };
    let view = ActionModalView {
        post_url: format!("/admin/_action/{}/{}", actions::encode(&module), actions::encode(&action)),
        module,
        action: found.decl.clone(),
        entity,
        csrf: ctx.csrf,
    };
    text(StatusCode::OK, format!("{}/{}: working\n", view.module, view.action.id))
}

/// `POST /admin/_action/{module}/{action}`: CSRF → role → `implemented` → invoke →
/// `admin.action` → outcome mapping (Ok is a 303 PRG carrying the flash token).
async fn action_submit(
    State(p): State<Portal>,
    jar: CookieJar,
    Path((module, action)): Path<(String, String)>,
    Form(body): Form<HashMap<String, String>>,
) -> Response {
    let authed = match p.st.gate(&jar, true).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    if let Some(r) = p.st.check_csrf(&authed, &body) {
        return r;
    }
    let operator = match roles::resolve(&p.st, &authed).await {
        Ok(o) => o,
        Err(r) => return r,
    };
    let snapshot = p.engine.snapshot();
    let Some(found) = actions::find(&snapshot, &module, &action) else {
        return not_working(StatusCode::NOT_FOUND, &format!("{module}/{action}"));
    };
    match actions::admit(&operator, found.decl) {
        Ok(()) => {}
        Err(Refusal::Denied) => return text(StatusCode::FORBIDDEN, format!("{module}/{action}: denied\n")),
        Err(Refusal::NotImplemented) => {
            return text(StatusCode::FORBIDDEN, format!("{module}/{action}: Not implemented yet\n"))
        }
    }
    if matches!(found.decl.then, controlplane::Then::Link(_)) {
        return text(StatusCode::BAD_REQUEST, format!("{module}/{action}: a link action has no invoke\n"));
    }
    let mut result = ActionResultView {
        module: module.clone(),
        action: action.clone(),
        field_errors: BTreeMap::new(),
        current_revision: None,
        error: None,
    };
    let req = match actions::build_request(&module, found.decl, &operator, &body) {
        Ok(req) => req,
        Err(BuildError::Malformed(msg)) => return text(StatusCode::BAD_REQUEST, format!("{msg}\n")),
        Err(BuildError::Fields(errors)) => {
            result.field_errors = errors;
            return render_result(StatusCode::UNPROCESSABLE_ENTITY, &result);
        }
    };
    let invoked = actions::invoke(found.face.as_ref(), req.clone()).await;
    if let Err(e) = actions::record(&p.st, &operator, &req, &invoked).await {
        if matches!(invoked, Invoked::Outcome(InvokeOutcome::Ok { .. })) {
            tracing::error!(err = %e, module = %module, action = %action, "admin.action append failed");
            result.error = Some("action applied but audit append failed".to_string());
            return render_result(StatusCode::INTERNAL_SERVER_ERROR, &result);
        }
        // A 500 would mask the outcome the operator must act on; only an applied action escalates.
        tracing::warn!(err = %e, module = %module, action = %action, "admin.action append failed");
    }
    let status = actions::http_status(&invoked);
    match invoked {
        Invoked::Outcome(InvokeOutcome::Ok { message, reveal, redirect }) => {
            let target = actions::ok_target(redirect.as_ref(), &body, req.entity.as_ref());
            if message.is_empty() && reveal.is_none() {
                return see_other(&target);
            }
            match actions::stash_flash(&p.st.pool, &Flash { message, reveal }).await {
                Ok(token) => see_other(&actions::with_flash(&target, &token)),
                // The provider already applied the action; the show-once value is lost
                // rather than ever served twice.
                Err(err) => {
                    tracing::error!(%err, "admin flash: stash failed; redirecting without it");
                    see_other(&target)
                }
            }
        }
        Invoked::Outcome(InvokeOutcome::FieldErrors(errors)) => {
            result.field_errors = errors;
            render_result(status, &result)
        }
        Invoked::Outcome(InvokeOutcome::Conflict { current_revision }) => {
            result.current_revision = Some(current_revision);
            render_result(status, &result)
        }
        Invoked::Failed(error) => {
            result.error = Some(error.to_string());
            render_result(status, &result)
        }
        Invoked::TimedOut => {
            result.error = Some(format!(
                "no answer within {}ms; the outcome is unknown",
                actions::INVOKE_DEADLINE.as_millis()
            ));
            render_result(status, &result)
        }
    }
}

fn render_result(status: StatusCode, view: &ActionResultView) -> Response {
    let state = if status == StatusCode::UNPROCESSABLE_ENTITY { "field errors" } else { "not working" };
    let mut out = format!("{}/{}: {state}\n", view.module, view.action);
    for (field, msg) in &view.field_errors {
        let _ = writeln!(out, "field {field}: {msg}");
    }
    if let Some(rev) = view.current_revision {
        let _ = writeln!(out, "conflict: current revision {rev}");
    }
    if let Some(error) = &view.error {
        let _ = writeln!(out, "error: {error}");
    }
    text(status, out)
}
