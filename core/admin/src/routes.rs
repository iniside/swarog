//! The control-plane portal routes. Each handler resolves a typed view model through the
//! engine and renders it through the templates in `crate::render`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use controlplane::{ActionDecl, ActionPoint, ContributionDecl, EntityRef, InvokeOutcome, Layout, Manifest, PanelRequest, Point};
use serde::Serialize;

use crate::engine::actions::{self, BuildError, Flash, Invoked};
use crate::engine::manifests::Snapshot;
use crate::engine::panels::{self, PanelView};
use crate::engine::roles::{self, Operator};
use crate::engine::search;
use crate::engine::Engine;
use crate::render::{
    self, ActionItem, ActionResultView, Crumb, DashboardView, EntityView, FlashView, MenuView, MissingView,
    NavSectionView, NavView, OperatorView, PageView, PanelInput, ScopeView, SearchGroupView, SearchHitView, SearchView,
    ShellView, SlotView, TabView,
};
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

fn nav_key(module: &str, id: &str) -> String {
    format!("{module}/{id}")
}

fn panel_url(module: &str, contribution: &str) -> String {
    format!("/admin/_panel/{}/{}", render::encode(module), render::encode(contribution))
}

fn nav_sections(snapshot: &Snapshot) -> Vec<NavSectionView> {
    snapshot
        .sections
        .iter()
        .map(|section| NavSectionView {
            id: section.id.clone(),
            label: section.label.clone(),
            items: snapshot
                .nav
                .iter()
                .filter(|n| n.section == section.id)
                .map(|n| NavView {
                    key: nav_key(&n.module, &n.id),
                    label: n.label.clone(),
                    icon: n.icon.clone(),
                    href: render::page_url(&n.module, &n.page),
                    module: n.module.clone(),
                    down: !n.source_ok,
                    counter_url: n.counter.as_ref().map(|c| panel_url(&n.module, c)),
                })
                .collect(),
        })
        .collect()
}

/// The breadcrumb of a nav entry's section, linked to the section's first entry.
fn section_crumb(sections: &[NavSectionView], section: &str) -> Option<Crumb> {
    let s = sections.iter().find(|s| s.id == section)?;
    Some(Crumb { label: s.label.clone(), href: s.items.first().map(|n| n.href.clone()) })
}

async fn shell(
    p: &Portal,
    ctx: &Ctx,
    snapshot: &Snapshot,
    query: &HashMap<String, String>,
    active: Option<String>,
    crumbs: Vec<Crumb>,
) -> ShellView {
    let flash = match query.get("reveal") {
        Some(token) => actions::take_flash(&p.st.pool, token).await,
        None => None,
    };
    let mut scopes: Vec<ScopeView> = Vec::new();
    for decl in snapshot.modules.values().flat_map(|r| r.manifest.search.iter()) {
        if scopes.iter().all(|s| s.kind != decl.kind) {
            scopes.push(ScopeView { kind: decl.kind.clone(), label: decl.label.clone(), icon: decl.icon.clone() });
        }
    }
    ShellView {
        operator: OperatorView::new(&ctx.operator.login, crate::role_name(ctx.operator.role)),
        csrf: ctx.csrf.clone(),
        sections: nav_sections(snapshot),
        active,
        crumbs,
        scopes,
        flash: flash.map(|f: Flash| FlashView { message: f.message, reveal: f.reveal.as_ref().map(render::reveal) }),
        deployment: None,
        health: None,
        notifications: None,
    }
}

/// Whether `decl` is laid out in a `Tiles` page of `manifest`.
fn in_tiles(manifest: &Manifest, decl: &ContributionDecl) -> bool {
    let Point::Page(page) = &decl.point else {
        return false;
    };
    manifest
        .pages
        .iter()
        .filter(|pg| &pg.id == page)
        .any(|pg| matches!(&pg.layout, Layout::Tiles(ids) if ids.contains(&decl.id)))
}

fn slot(operator: &Operator, module: &str, decl: &ContributionDecl, entity: Option<&EntityRef>, tiles: bool) -> SlotView {
    let mut url = panel_url(module, &decl.id);
    if let Some(e) = entity {
        url.push_str(&format!("?entity={}", render::encode(&render::entity_param(e))));
    }
    SlotView {
        module: module.to_string(),
        contribution: decl.id.clone(),
        title: decl.title.clone(),
        icon: decl.icon.clone(),
        url,
        denied: !roles::allows(operator, decl.min_role),
        min_role: crate::role_name(decl.min_role),
        mode: render::mode_of(&decl.point, tiles),
    }
}

fn action_item(
    operator: &Operator,
    csrf: &str,
    module: &str,
    writes: bool,
    decl: &ActionDecl,
    entity: Option<&EntityRef>,
) -> ActionItem {
    let url = format!("/admin/_action/{}/{}", render::encode(module), render::encode(&decl.id));
    let (enabled, hint) = match actions::admit(operator, decl, writes) {
        Ok(()) => (true, None),
        Err(refusal) => (false, Some(refusal.hint())),
    };
    let mut vals = BTreeMap::new();
    if !csrf.is_empty() {
        vals.insert(actions::FIELD_CSRF.to_string(), csrf.to_string());
    }
    let item = ActionItem {
        module: module.to_string(),
        id: decl.id.clone(),
        label: decl.label.clone(),
        icon: decl.icon.clone(),
        danger: decl.danger,
        url,
        enabled,
        hint,
        then: render::then_name(&decl.then),
        href: match &decl.then {
            controlplane::Then::Link(link) => Some(render::link_url(link)),
            _ => None,
        },
        vals,
    };
    match entity {
        Some(e) => render::for_entity(&item, e),
        None => item,
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
    ctx: &Ctx,
    entity: Option<&EntityRef>,
    want: impl Fn(&ActionPoint) -> bool,
) -> Vec<ActionItem> {
    let mut found: Vec<(&str, bool, &ActionDecl)> = snapshot
        .modules
        .iter()
        .flat_map(|(m, r)| r.manifest.actions.iter().map(move |a| (m.as_str(), r.manifest.writes, a)))
        .filter(|(_, _, a)| want(&a.point))
        .collect();
    found.sort_by(|a, b| (a.2.order, a.0, &a.2.id).cmp(&(b.2.order, b.0, &b.2.id)));
    found.into_iter().map(|(m, w, a)| action_item(&ctx.operator, &ctx.csrf, m, w, a, entity)).collect()
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn html(status: StatusCode, rendered: Result<String, minijinja::Error>) -> Response {
    match rendered {
        Ok(body) => (status, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response(),
        Err(e) => {
            tracing::error!(err = %e, "admin render failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "render failed").into_response()
        }
    }
}

fn screen<V: Serialize>(p: &Portal, name: &str, view: &V) -> Response {
    screen_with(p, StatusCode::OK, name, view)
}

fn screen_with<V: Serialize>(p: &Portal, status: StatusCode, name: &str, view: &V) -> Response {
    let ctx = minijinja::context! { screen => name, v => minijinja::Value::from_serialize(view) };
    html(status, render::page(&p.st.env, "shell.html", &ctx))
}

fn toast(p: &Portal, status: StatusCode, text: String) -> Response {
    let view = ActionResultView { modal: None, toast: Some(text) };
    html(status, render::fragment(&p.st.env, "modal.html", "result", &view))
}

fn not_found(p: &Portal, what: &str) -> Response {
    toast(p, StatusCode::NOT_FOUND, format!("{what}: not found"))
}

fn is_htmx(headers: &HeaderMap) -> bool {
    headers.get("hx-request").is_some_and(|v| v.as_bytes() == b"true")
}

struct Missing {
    what: String,
    title: &'static str,
    detail: String,
}

/// A 404 for a full-page GET: the shell around the error state, or the toast when htmx
/// asked.
async fn missing_page(
    p: &Portal,
    ctx: &Ctx,
    snapshot: &Snapshot,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    miss: Missing,
) -> Response {
    if is_htmx(headers) {
        return not_found(p, &miss.what);
    }
    let crumbs = vec![Crumb { label: miss.title.to_string(), href: None }];
    let view = MissingView {
        shell: shell(p, ctx, snapshot, query, None, crumbs).await,
        title: miss.title.to_string(),
        detail: miss.detail,
    };
    screen_with(p, StatusCode::NOT_FOUND, "missing", &view)
}

/// A redirect an htmx request follows itself (`HX-Redirect`), else a 303.
fn redirect(headers: &HeaderMap, target: &str) -> Response {
    if headers.contains_key("hx-request") {
        let value = HeaderValue::from_str(target).unwrap_or_else(|_| HeaderValue::from_static("/admin"));
        (StatusCode::OK, [("hx-redirect", value)]).into_response()
    } else {
        see_other(target)
    }
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
            .map(|(m, c)| slot(&ctx.operator, m, c, None, false))
            .collect::<Vec<_>>()
    };
    let crumbs = vec![Crumb { label: "Dashboard".to_string(), href: None }];
    let view = DashboardView {
        shell: shell(&p, &ctx, &snapshot, &query, None, crumbs).await,
        tiles: pick(Point::DashboardTile),
        attention: pick(Point::DashboardAttention),
        panels: pick(Point::DashboardPanel),
    };
    screen(&p, "dashboard", &view)
}

/// `GET /admin/p/{module}/{page}`.
async fn page(
    State(p): State<Portal>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((module, page_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let ctx = match authorize(&p, &jar, false).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let snapshot = p.engine.snapshot();
    let Some((manifest, _)) = snapshot.route(&module) else {
        let miss = Missing {
            what: module.clone(),
            title: "Unknown module",
            detail: format!("No module serves “{module}”."),
        };
        return missing_page(&p, &ctx, &snapshot, &headers, &query, miss).await;
    };
    let Some(decl) = manifest.pages.iter().find(|pg| pg.id == page_id) else {
        let miss = Missing {
            what: format!("{module}/{page_id}"),
            title: "Unknown page",
            detail: format!("The {module} module declares no page “{page_id}”."),
        };
        return missing_page(&p, &ctx, &snapshot, &headers, &query, miss).await;
    };
    let tiles = matches!(decl.layout, Layout::Tiles(_));
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
                    found.map(|c| slot(&ctx.operator, &module, c, None, tiles))
                })
                .collect()
        })
        .collect();
    let head_actions = decl
        .head_actions
        .iter()
        .filter_map(|id| manifest.actions.iter().find(|a| &a.id == id))
        .map(|a| action_item(&ctx.operator, &ctx.csrf, &module, manifest.writes, a, None))
        .collect();
    let entry = snapshot.nav.iter().find(|n| n.module == module && n.page == page_id);
    let sections = nav_sections(&snapshot);
    let mut crumbs: Vec<Crumb> = entry.and_then(|n| section_crumb(&sections, &n.section)).into_iter().collect();
    crumbs.push(Crumb { label: decl.title.clone(), href: None });
    let active = entry.map(|n| nav_key(&n.module, &n.id));
    let view = PageView {
        shell: shell(&p, &ctx, &snapshot, &query, active, crumbs).await,
        module: module.clone(),
        title: decl.title.clone(),
        subtitle: decl.subtitle.clone(),
        layout: render::layout_name(&decl.layout),
        head_actions,
        columns,
        missing,
    };
    screen(&p, "page", &view)
}

/// `GET /admin/e/{kind}/{id}?tab=`.
async fn entity(
    State(p): State<Portal>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let ctx = match authorize(&p, &jar, false).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let snapshot = p.engine.snapshot();
    let Some((owner, kind_decl)) = snapshot
        .modules
        .iter()
        .find_map(|(m, r)| r.manifest.entity_kinds.iter().find(|k| k.kind == kind).map(|k| (m.clone(), k.clone())))
    else {
        let miss = Missing {
            what: format!("{kind}:{id}"),
            title: "Unknown entity kind",
            detail: format!("No module declares the entity kind “{kind}”."),
        };
        return missing_page(&p, &ctx, &snapshot, &headers, &query, miss).await;
    };
    let entity = EntityRef { kind: kind.clone(), id: id.clone() };
    let op = &ctx.operator;
    let headers = contributions(&snapshot, |pt| matches!(pt, Point::EntityHeader(k) if *k == kind))
        .into_iter()
        .map(|(m, c)| slot(op, m, c, Some(&entity), false))
        .collect();
    let summaries = contributions(&snapshot, |pt| matches!(pt, Point::EntitySummary(k) if *k == kind))
        .into_iter()
        .map(|(m, c)| slot(op, m, c, Some(&entity), false))
        .collect();
    let tab_decls = contributions(&snapshot, |pt| matches!(pt, Point::EntityTab { kind: k, .. } if *k == kind));
    let mut tabs: Vec<TabView> = Vec::new();
    for (_, c) in &tab_decls {
        if let Point::EntityTab { tab, tab_label, .. } = &c.point {
            if tabs.iter().all(|t| &t.id != tab) {
                let url = format!("{}?tab={}", render::entity_url(&entity), render::encode(tab));
                tabs.push(TabView { id: tab.clone(), label: tab_label.clone(), url, counts: Vec::new() });
            }
        }
    }
    for (m, c) in contributions(&snapshot, |pt| matches!(pt, Point::EntityTabCount { kind: k, .. } if *k == kind)) {
        if let Point::EntityTabCount { tab, .. } = &c.point {
            if let Some(t) = tabs.iter_mut().find(|t| &t.id == tab) {
                t.counts.push(slot(op, m, c, Some(&entity), false));
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
        .map(|(m, c)| slot(op, m, c, Some(&entity), false))
        .collect();
    let header_actions =
        action_items(&snapshot, &ctx, Some(&entity), |pt| matches!(pt, ActionPoint::HeaderAction(k) if *k == kind));
    let has_menu = snapshot
        .modules
        .values()
        .flat_map(|r| r.manifest.actions.iter())
        .any(|a| matches!(&a.point, ActionPoint::RowAction(k) if *k == kind));
    let entry = snapshot.nav.iter().find(|n| n.module == owner);
    let sections = nav_sections(&snapshot);
    let mut crumbs: Vec<Crumb> = entry.and_then(|n| section_crumb(&sections, &n.section)).into_iter().collect();
    if let Some(n) = entry {
        crumbs.push(Crumb { label: n.label.clone(), href: Some(render::page_url(&n.module, &n.page)) });
    }
    crumbs.push(Crumb { label: id.clone(), href: None });
    let active = entry.map(|n| nav_key(&n.module, &n.id));
    let view = EntityView {
        shell: shell(&p, &ctx, &snapshot, &query, active, crumbs).await,
        kind_label: kind_decl.label.clone(),
        avatar_color: render::avatar_color(&id),
        avatar_initials: render::initials(&id),
        menu_url: has_menu.then(|| format!("/admin/_menu/{}/{}", render::encode(&kind), render::encode(&id))),
        entity_id: id,
        headers,
        summaries,
        header_actions,
        tabs,
        selected_tab,
        tab_panels,
    };
    screen(&p, "entity", &view)
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
    let Some((manifest, decl)) = snapshot
        .route(&module)
        .and_then(|(m, _)| m.contributions.iter().find(|c| c.id == contribution).map(|c| (m, c)))
    else {
        return not_found(&p, &format!("{module}/{contribution}"));
    };
    let raw_entity = query.remove("entity");
    let entity = match &raw_entity {
        None => None,
        Some(raw) => match actions::parse_entity(raw) {
            Some(e) => Some(e),
            None => return toast(&p, StatusCode::BAD_REQUEST, "entity must be kind:id".to_string()),
        },
    };
    let mut url_query: Vec<(String, String)> = raw_entity.into_iter().map(|e| ("entity".to_string(), e)).collect();
    url_query.extend(query.iter().map(|(k, v)| (k.clone(), v.clone())));
    let req = PanelRequest { module: module.clone(), contribution: contribution.clone(), entity: entity.clone(), params: query };
    let view: PanelView = panels::resolve(&p.engine, &snapshot, &ctx.operator, &module, decl, req).await;
    let queue_actions: Vec<ActionItem> = manifest
        .actions
        .iter()
        .map(|a| action_item(&ctx.operator, &ctx.csrf, &module, manifest.writes, a, None))
        .collect();
    let row_menu_kinds: Vec<String> = snapshot
        .modules
        .values()
        .flat_map(|r| r.manifest.actions.iter())
        .filter_map(|a| match &a.point {
            ActionPoint::RowAction(kind) => Some(kind.clone()),
            _ => None,
        })
        .collect();
    let path = panel_url(&module, &contribution);
    let fallback = entity.as_ref().map(|e| e.id.clone()).unwrap_or_default();
    let input = PanelInput {
        module: &view.module,
        contribution: &view.contribution,
        title: &view.title,
        icon: &decl.icon,
        mode: render::mode_of(&decl.point, in_tiles(manifest, decl)),
        state: view.state.as_str(),
        path: &path,
        query: &url_query,
        error: view.error.as_deref(),
        age_s: view.age_s,
        min_role: crate::role_name(decl.min_role),
        fallback_title: &fallback,
        primitive: view.primitive.as_ref(),
        actions: &queue_actions,
        row_menu_kinds: &row_menu_kinds,
    };
    let rendered = render::panel(&input);
    html(StatusCode::OK, render::fragment(&p.st.env, "primitives.html", "panel_fragment", &rendered))
}

/// `GET /admin/_menu/{kind}/{id}` — the row actions for one entity, grouped by module.
async fn menu(State(p): State<Portal>, jar: CookieJar, Path((kind, id)): Path<(String, String)>) -> Response {
    let ctx = match authorize(&p, &jar, false).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let snapshot = p.engine.snapshot();
    let entity = EntityRef { kind: kind.clone(), id };
    let items =
        action_items(&snapshot, &ctx, Some(&entity), |pt| matches!(pt, ActionPoint::RowAction(k) if *k == kind));
    let view = MenuView { entity, groups: render::menu_groups(items) };
    html(StatusCode::OK, render::fragment(&p.st.env, "primitives.html", "menu", &view))
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
        return toast(&p, StatusCode::BAD_REQUEST, format!("q exceeds {} bytes", search::MAX_QUERY_BYTES));
    }
    let snapshot = p.engine.snapshot();
    let groups = search::run(&snapshot, &q, &kind)
        .await
        .into_iter()
        .map(|g| SearchGroupView {
            module: g.module,
            label: g.label,
            icon: g.icon,
            hits: g
                .hits
                .iter()
                .map(|h| SearchHitView {
                    hl: false,
                    title: h.title.clone(),
                    subtitle: h.subtitle.clone(),
                    badge: h.badge.clone(),
                    href: render::entity_url(&h.target),
                })
                .collect(),
            error: g.error,
        })
        .collect();
    let view = SearchView::new(q, kind, groups);
    html(StatusCode::OK, render::fragment(&p.st.env, "primitives.html", "search", &view))
}

/// `GET /admin/_action/{module}/{action}?entity=kind:id` — the action's modal.
async fn action_modal(
    State(p): State<Portal>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((module, action)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let ctx = match authorize(&p, &jar, false).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let snapshot = p.engine.snapshot();
    let Some(found) = actions::find(&snapshot, &module, &action) else {
        return not_found(&p, &format!("{module}/{action}"));
    };
    if let Err(refusal) = actions::admit(&ctx.operator, found.decl, found.writes) {
        return toast(&p, StatusCode::FORBIDDEN, refusal.hint().to_string());
    }
    if let controlplane::Then::Link(link) = &found.decl.then {
        return redirect(&headers, &render::link_url(link));
    }
    let entity = match query.get("entity") {
        None => None,
        Some(raw) => match actions::parse_entity(raw) {
            Some(e) => Some(e),
            None => return toast(&p, StatusCode::BAD_REQUEST, "entity must be kind:id".to_string()),
        },
    };
    let post_url = format!("/admin/_action/{}/{}", render::encode(&module), render::encode(&action));
    let view = render::modal(found.decl, post_url, ctx.csrf, entity.as_ref(), &BTreeMap::new(), &BTreeMap::new());
    html(StatusCode::OK, render::fragment(&p.st.env, "modal.html", "modal", &view))
}

/// `POST /admin/_action/{module}/{action}`: CSRF → role → `implemented` → `writes` →
/// invoke → `admin.action` → outcome mapping (Ok is a 303 PRG carrying the flash token).
async fn action_submit(
    State(p): State<Portal>,
    jar: CookieJar,
    headers: HeaderMap,
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
        return not_found(&p, &format!("{module}/{action}"));
    };
    if let Err(refusal) = actions::admit(&operator, found.decl, found.writes) {
        return toast(&p, StatusCode::FORBIDDEN, refusal.hint().to_string());
    }
    if matches!(found.decl.then, controlplane::Then::Link(_)) {
        return toast(&p, StatusCode::BAD_REQUEST, format!("{module}/{action}: a link action has no invoke"));
    }
    let posted: BTreeMap<String, String> = body.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let post_url = format!("/admin/_action/{}/{}", render::encode(&module), render::encode(&action));
    let reshow = |entity: Option<&EntityRef>, errors: &BTreeMap<String, String>, banner: Option<String>| {
        let mut modal = render::modal(found.decl, post_url.clone(), authed.csrf.clone(), entity, &posted, errors);
        if banner.is_some() {
            modal.banner = banner;
        }
        ActionResultView { modal: Some(modal), toast: None }
    };
    let req = match actions::build_request(&module, found.decl, &operator, &body) {
        Ok(req) => req,
        Err(BuildError::Malformed(msg)) => return toast(&p, StatusCode::BAD_REQUEST, msg),
        Err(BuildError::Fields(errors)) => {
            let entity = body.get(actions::FIELD_ENTITY).and_then(|raw| actions::parse_entity(raw));
            return render_result(&p, StatusCode::UNPROCESSABLE_ENTITY, &reshow(entity.as_ref(), &errors, None));
        }
    };
    let invoked = actions::invoke(found.face.as_ref(), req.clone()).await;
    if let Err(e) = actions::record(&p.st, &operator, &req, &invoked).await {
        if matches!(invoked, Invoked::Outcome(InvokeOutcome::Ok { .. })) {
            tracing::error!(err = %e, module = %module, action = %action, "admin.action append failed");
            return toast(&p, StatusCode::INTERNAL_SERVER_ERROR, "action applied but audit append failed".to_string());
        }
        // A 500 would mask the outcome the operator must act on; only an applied action escalates.
        tracing::warn!(err = %e, module = %module, action = %action, "admin.action append failed");
    }
    let status = actions::http_status(&invoked);
    match invoked {
        Invoked::Outcome(InvokeOutcome::Ok { message, reveal, redirect: link }) => {
            let target = actions::ok_target(link.as_ref(), &body, req.entity.as_ref());
            if message.is_empty() && reveal.is_none() {
                return redirect(&headers, &target);
            }
            match actions::stash_flash(&p.st.pool, &Flash { message, reveal }).await {
                Ok(token) => redirect(&headers, &actions::with_flash(&target, &token)),
                // The provider already applied the action; the show-once value is lost
                // rather than ever served twice.
                Err(err) => {
                    tracing::error!(%err, "admin flash: stash failed; redirecting without it");
                    redirect(&headers, &target)
                }
            }
        }
        Invoked::Outcome(InvokeOutcome::FieldErrors(errors)) => {
            render_result(&p, status, &reshow(req.entity.as_ref(), &errors, None))
        }
        Invoked::Outcome(InvokeOutcome::Conflict { current_revision }) => {
            let banner = format!(
                "This changed since you opened it (now revision {current_revision}). Reload the page and try again."
            );
            render_result(&p, status, &reshow(req.entity.as_ref(), &BTreeMap::new(), Some(banner)))
        }
        Invoked::Failed(error) => toast(&p, status, error.to_string()),
        Invoked::TimedOut => toast(
            &p,
            status,
            format!("no answer within {}ms; the outcome is unknown", actions::INVOKE_DEADLINE.as_millis()),
        ),
    }
}

fn render_result(p: &Portal, status: StatusCode, view: &ActionResultView) -> Response {
    html(status, render::fragment(&p.st.env, "modal.html", "result", view))
}
