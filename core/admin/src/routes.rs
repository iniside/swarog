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
use controlplane::{
    ActionDecl, ActionPoint, ContributionDecl, EntityRef, FormSpec, InvokeCall, InvokeOutcome, Layout, Manifest,
    PanelQuery, Point, Primitive, Then,
};
use serde::Serialize;

use crate::engine::actions::{self, Claim, Claimant, Flash, FlashTone, FormRow, Invoked, StoredSpec};
use crate::engine::forms;
use crate::engine::manifests::Snapshot;
use crate::engine::panels::{self, PanelState, PanelView};
use crate::engine::roles::{self, Operator};
use crate::engine::search;
use crate::engine::Engine;
use crate::render::{
    self, ActionItem, ActionResultView, Crumb, DashboardView, EntityView, FlashView, FormFrame, FormInstance,
    MenuView, MissingView, ModalSpec, NavSectionView, NavView, OperatorView, PageView, PanelInput, ScopeView,
    SearchGroupView, SearchHitView, SearchView, ShellView, SlotView, TabView,
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

async fn authorize(p: &Portal, jar: &CookieJar, is_post: bool) -> Result<Ctx, Box<Response>> {
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
        flash: flash.map(|f: Flash| FlashView {
            message: f.message,
            reveal: f.reveal.as_ref().map(render::reveal),
            tone: f.tone.name(),
        }),
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
    let url = action_url(module, &decl.id);
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
        primary: decl.primary,
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
    render::bind(&item, entity, None)
}

fn action_url(module: &str, action: &str) -> String {
    format!("/admin/_action/{}/{}", render::encode(module), render::encode(action))
}

/// The `PanelTool` actions of `contribution` in `manifest`, unbound, by `(order, id)`.
fn panel_tools(
    operator: &Operator,
    csrf: &str,
    module: &str,
    manifest: &Manifest,
    contribution: &str,
) -> Vec<ActionItem> {
    let mut tools: Vec<&ActionDecl> = manifest
        .actions
        .iter()
        .filter(|a| matches!(&a.point, ActionPoint::PanelTool(c) if c == contribution))
        .collect();
    tools.sort_by(|a, b| (a.order, &a.id).cmp(&(b.order, &b.id)));
    tools.into_iter().map(|a| action_item(operator, csrf, module, manifest.writes, a, None)).collect()
}

/// The `PanelForm` action of `contribution` in `manifest`.
fn panel_form_action<'a>(manifest: &'a Manifest, contribution: &str) -> Option<&'a ActionDecl> {
    manifest.actions.iter().find(|a| matches!(&a.point, ActionPoint::PanelForm(c) if c == contribution))
}

/// Every `(module, decl)` across all manifests matching `want`, sorted by `(order, module, id)`.
fn contributions(
    snapshot: &Snapshot,
    want: impl Fn(&Point) -> bool,
) -> Vec<(&str, &ContributionDecl)> {
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

/// A toast always lands in `#modal-root`, whatever the request targeted, so it never
/// replaces a panel form.
fn toast_as(p: &Portal, status: StatusCode, text: String, tone: &'static str) -> Response {
    let view = ActionResultView { modal: None, toast: Some(text), tone };
    let mut resp = html(status, render::fragment(&p.st.env, "modal.html", "result", &view));
    resp.headers_mut().insert("hx-retarget", HeaderValue::from_static("#modal-root"));
    resp.headers_mut().insert("hx-reswap", HeaderValue::from_static("innerHTML"));
    resp
}

fn toast(p: &Portal, status: StatusCode, text: String) -> Response {
    toast_as(p, status, text, "err")
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
        Err(r) => return *r,
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
        Err(r) => return *r,
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
        .filter(|a| !matches!(a.point, ActionPoint::PanelForm(_)))
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
        Err(r) => return *r,
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
        Err(r) => return *r,
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
    let search = query.remove(render::QUERY_SEARCH);
    let req = PanelQuery {
        module: module.clone(),
        contribution: contribution.clone(),
        entity: entity.clone(),
        search: search.clone(),
        cursor: query.remove(render::QUERY_CURSOR),
        before: query.remove(render::QUERY_BEFORE),
        queue_state: query.remove(render::QUERY_STATE),
        params: query,
    };
    let mut view: PanelView = panels::resolve(&p.engine, &snapshot, &ctx.operator, &module, decl, req).await;
    let mut form = None;
    if let (PanelState::Ok, Some(Primitive::Form(spec))) = (view.state, &view.primitive) {
        match panel_form_instance(&p, &ctx, manifest, &module, decl, entity.as_ref(), spec).await {
            Ok(Some((post_url, form_id))) => form = Some((post_url, form_id)),
            Ok(None) => view.state = PanelState::Readonly,
            Err(msg) => {
                view.state = PanelState::Error;
                view.primitive = None;
                view.error = Some(msg);
            }
        }
    }
    let tools = panel_tools(&ctx.operator, &ctx.csrf, &module, manifest, &contribution);
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
    let no_errors = BTreeMap::new();
    let input = PanelInput {
        module: &view.module,
        contribution: &view.contribution,
        title: &view.title,
        icon: &decl.icon,
        mode: render::mode_of(&decl.point, in_tiles(manifest, decl)),
        state: view.state.as_str(),
        path: &path,
        query: &url_query,
        entity: entity.as_ref(),
        search: search.as_deref(),
        error: view.error.as_deref(),
        age_s: view.age_s,
        min_role: crate::role_name(decl.min_role),
        fallback_title: &fallback,
        primitive: view.primitive.as_ref(),
        actions: &tools,
        row_menu_kinds: &row_menu_kinds,
        csrf: &ctx.csrf,
        form: form.map(|(post_url, form_id)| FormInstance {
            post_url,
            form_id,
            posted: None,
            errors: &no_errors,
            banner: None,
            locked: false,
        }),
    };
    let rendered = render::panel(&input);
    html(StatusCode::OK, render::fragment(&p.st.env, "primitives.html", "panel_fragment", &rendered))
}

/// The form instance an `ok` Form panel posts as: `Ok(None)` (readonly) when the
/// contribution has no `PanelForm` action or the viewer cannot use it, `Err` (an error
/// panel) when the spec cannot be served or the instance cannot be stored.
async fn panel_form_instance(
    p: &Portal,
    ctx: &Ctx,
    manifest: &Manifest,
    module: &str,
    decl: &ContributionDecl,
    entity: Option<&EntityRef>,
    spec: &FormSpec,
) -> Result<Option<(String, String)>, String> {
    let Some(action) = panel_form_action(manifest, &decl.id) else {
        return Ok(None);
    };
    if actions::admit(&ctx.operator, action, manifest.writes).is_err() {
        return Ok(None);
    }
    let errors = controlplane::form_spec_errors(spec);
    if !errors.is_empty() {
        return Err(format!("Form unavailable: {}", errors.join("; ")));
    }
    let stored = StoredSpec::Form(spec.clone());
    let mint = forms::Mint {
        login: &ctx.operator.login,
        module,
        action: &action.id,
        contribution: Some(&decl.id),
        entity,
        item: None,
        spec: &stored,
    };
    match forms::mint(&p.st.pool, &decl.id, &mint).await {
        Ok(form_id) => Ok(Some((action_url(module, &action.id), form_id))),
        Err(err) => {
            tracing::error!(%err, module, contribution = %decl.id, "admin forms: minting a panel form failed");
            Err("Form unavailable".to_string())
        }
    }
}

/// `GET /admin/_menu/{kind}/{id}` — the row actions for one entity, grouped by module.
async fn menu(State(p): State<Portal>, jar: CookieJar, Path((kind, id)): Path<(String, String)>) -> Response {
    let ctx = match authorize(&p, &jar, false).await {
        Ok(c) => c,
        Err(r) => return *r,
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
        return *r;
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

fn modal_spec(spec: &StoredSpec) -> Option<ModalSpec<'_>> {
    match spec {
        StoredSpec::Form(form) => Some(ModalSpec::Form(form)),
        StoredSpec::Confirm(confirm) => Some(ModalSpec::Confirm(confirm)),
        StoredSpec::Click => None,
    }
}

/// `GET /admin/_action/{module}/{action}?entity=kind:id&item=` — the modal of a
/// `Then::Form` or `Then::Confirm` action over a freshly minted form instance.
async fn action_modal(
    State(p): State<Portal>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((module, action)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let ctx = match authorize(&p, &jar, false).await {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let snapshot = p.engine.snapshot();
    let Some(found) = actions::find(&snapshot, &module, &action) else {
        return not_found(&p, &format!("{module}/{action}"));
    };
    if let Err(refusal) = actions::admit(&ctx.operator, found.decl, found.writes) {
        return toast(&p, StatusCode::FORBIDDEN, refusal.hint().to_string());
    }
    if let Then::Link(link) = &found.decl.then {
        return redirect(&headers, &render::link_url(link));
    }
    let spec = match &found.decl.then {
        _ if matches!(found.decl.point, ActionPoint::PanelForm(_)) => None,
        Then::Form(form) => Some(StoredSpec::Form(form.clone())),
        Then::Confirm(confirm) => Some(StoredSpec::Confirm(confirm.clone())),
        Then::Invoke | Then::Link(_) => None,
    };
    let Some((spec, shown)) = spec.as_ref().and_then(|spec| modal_spec(spec).map(|shown| (spec, shown))) else {
        return toast(&p, StatusCode::BAD_REQUEST, format!("{module}/{action} has no modal"));
    };
    let entity = match query.get("entity") {
        None => None,
        Some(raw) => match actions::parse_entity(raw) {
            Some(e) => Some(e),
            None => return toast(&p, StatusCode::BAD_REQUEST, "entity must be kind:id".to_string()),
        },
    };
    let item = query.get(render::QUERY_ITEM).map(String::as_str);
    if item.is_some_and(|i| !actions::validate_item(i)) {
        return toast(&p, StatusCode::BAD_REQUEST, "item is empty, too long or has control characters".to_string());
    }
    let mint = forms::Mint {
        login: &ctx.operator.login,
        module: &module,
        action: &action,
        contribution: None,
        entity: entity.as_ref(),
        item,
        spec,
    };
    let form_id = match forms::mint(&p.st.pool, &action, &mint).await {
        Ok(id) => id,
        Err(err) => {
            tracing::error!(%err, module = %module, action = %action, "admin forms: minting a modal form failed");
            return toast(&p, StatusCode::SERVICE_UNAVAILABLE, "Form unavailable".to_string());
        }
    };
    let post_url = action_url(&module, &action);
    let view = render::modal(found.decl, shown, post_url, ctx.csrf, form_id, None, &BTreeMap::new());
    html(StatusCode::OK, render::fragment(&p.st.env, "modal.html", "modal", &view))
}

/// The answer to a click or submit that a claim already holds or completed: never an error.
const REPEATED: &str = "Already submitted — this action is being processed or has completed";

/// One POST to `/admin/_action/{module}/{action}` after CSRF and action lookup.
struct Submit<'a> {
    p: &'a Portal,
    headers: &'a HeaderMap,
    snapshot: &'a Snapshot,
    found: &'a actions::Found<'a>,
    operator: &'a Operator,
    csrf: &'a str,
    who: Claimant,
    body: &'a [(String, String)],
}

/// `POST /admin/_action/{module}/{action}`: CSRF → action lookup → a form instance
/// (`_form`) or a token-less click claim → checks → invoke → settle → `admin.action` →
/// outcome mapping (Ok and NoOp are a 303 PRG carrying the flash token).
async fn action_submit(
    State(p): State<Portal>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((module, action)): Path<(String, String)>,
    Form(body): Form<Vec<(String, String)>>,
) -> Response {
    let authed = match p.st.gate(&jar, true).await {
        Ok(a) => a,
        Err(r) => return *r,
    };
    if p.st.check_csrf(&authed, &body).is_some() {
        return toast(&p, StatusCode::FORBIDDEN, "invalid csrf token".to_string());
    }
    let operator = match roles::resolve(&p.st, &authed).await {
        Ok(o) => o,
        Err(r) => return *r,
    };
    let snapshot = p.engine.snapshot();
    let Some(found) = actions::find(&snapshot, &module, &action) else {
        return not_found(&p, &format!("{module}/{action}"));
    };
    if matches!(found.decl.then, Then::Link(_)) {
        return toast(&p, StatusCode::BAD_REQUEST, format!("{module}/{action}: a link action has no invoke"));
    }
    let who = Claimant { login: operator.login.clone(), module: module.clone(), action: action.clone() };
    let s = Submit {
        p: &p,
        headers: &headers,
        snapshot: &snapshot,
        found: &found,
        operator: &operator,
        csrf: &authed.csrf,
        who,
        body: &body,
    };
    match actions::first(&body, actions::FIELD_FORM) {
        Some(form_id) => submit_form(&s, form_id).await,
        None => submit_click(&s).await,
    }
}

fn store_unavailable(p: &Portal, err: anyhow::Error) -> Response {
    tracing::error!(%err, "admin forms: store unavailable");
    toast(p, StatusCode::SERVICE_UNAVAILABLE, "Form store unavailable; try again".to_string())
}

/// The repeated-click answer: the neutral toast to htmx, else a bare 303 with no flash.
fn repeated(s: &Submit<'_>, entity: Option<&EntityRef>) -> Response {
    if is_htmx(s.headers) {
        return toast_as(s.p, StatusCode::OK, REPEATED.to_string(), "info");
    }
    see_other(&actions::ok_target(None, s.body, entity))
}

/// Claims the form instance `id`; a claim that returns no row is classified once, and a
/// `Retry` claims exactly once more.
async fn take_form(s: &Submit<'_>, id: &str) -> Result<FormRow, Box<Response>> {
    let pool = &s.p.st.pool;
    match forms::claim_form(pool, id, &s.who).await {
        Ok(Some(row)) => return Ok(row),
        Ok(None) => {}
        Err(err) => return Err(Box::new(store_unavailable(s.p, err))),
    }
    let (row, now) = forms::lookup(pool, id).await.map_err(|err| Box::new(store_unavailable(s.p, err)))?;
    let entity = row.as_ref().and_then(|r| r.entity.as_ref());
    match actions::classify_form(row.as_ref(), &s.who, now) {
        Claim::Reject => {
            Err(Box::new(toast(s.p, StatusCode::BAD_REQUEST, "This form is no longer valid; reopen it".to_string())))
        }
        Claim::Repeated => Err(Box::new(repeated(s, entity))),
        Claim::Retry => match forms::claim_form(pool, id, &s.who).await {
            Ok(Some(row)) => Ok(row),
            Ok(None) => Err(Box::new(repeated(s, entity))),
            Err(err) => Err(Box::new(store_unavailable(s.p, err))),
        },
    }
}

/// Upserts and claims the click row `id`; a claim that returns no row is classified once,
/// and a `Retry` upserts and claims exactly once more.
async fn take_click(
    s: &Submit<'_>,
    id: &str,
    entity: Option<&EntityRef>,
    item: Option<&str>,
) -> Result<FormRow, Box<Response>> {
    let pool = &s.p.st.pool;
    let attempt = || async {
        forms::upsert_click(pool, id, &s.who, entity, item).await?;
        forms::claim_click(pool, id, &s.who).await
    };
    match attempt().await {
        Ok(Some(row)) => return Ok(row),
        Ok(None) => {}
        Err(err) => return Err(Box::new(store_unavailable(s.p, err))),
    }
    let (row, now) = forms::lookup(pool, id).await.map_err(|err| Box::new(store_unavailable(s.p, err)))?;
    match actions::classify_click(row.as_ref(), &s.who, now) {
        Claim::Reject => {
            Err(Box::new(toast(s.p, StatusCode::BAD_REQUEST, "This click does not match its action".to_string())))
        }
        Claim::Repeated => Err(Box::new(repeated(s, entity))),
        Claim::Retry => match attempt().await {
            Ok(Some(row)) => Ok(row),
            Ok(None) => Err(Box::new(repeated(s, entity))),
            Err(err) => Err(Box::new(store_unavailable(s.p, err))),
        },
    }
}

/// The form path: claim → allowed keys → `admit` → decode → invoke with a call built only
/// from the row. Every pre-invoke rejection first releases the claim.
async fn submit_form(s: &Submit<'_>, form_id: &str) -> Response {
    let row = match take_form(s, form_id).await {
        Ok(row) => row,
        Err(resp) => return *resp,
    };
    let pool = &s.p.st.pool;
    if let Err(msg) = actions::check_form_keys(&row.spec, s.body) {
        forms::settle(pool, &row, false).await;
        return toast(s.p, StatusCode::BAD_REQUEST, msg);
    }
    if let Err(refusal) = actions::admit(s.operator, s.found.decl, s.found.writes) {
        forms::settle(pool, &row, false).await;
        return toast(s.p, StatusCode::FORBIDDEN, refusal.hint().to_string());
    }
    let submission = match actions::build_submission(&row.spec, s.body) {
        Ok(submission) => submission,
        Err(errors) => {
            forms::settle(pool, &row, false).await;
            return reshow(s, &row, &errors, None, StatusCode::UNPROCESSABLE_ENTITY);
        }
    };
    let req = InvokeCall {
        module: row.module.clone(),
        action: row.action.clone(),
        entity: row.entity.clone(),
        input: submission.input,
        expected_revision: submission.expected_revision,
        reason: submission.reason,
        operator: s.operator.login.clone(),
        item: row.item.clone(),
        request_id: Some(row.id.clone()),
    };
    let invoked = actions::invoke(s.found.face.as_ref(), req.clone()).await;
    forms::settle(pool, &row, actions::consumes(&invoked)).await;
    if let Some(resp) = audit(s, &req, &invoked).await {
        return resp;
    }
    answer(s, Some(&row), &req, invoked).await
}

/// The token-less path of a plain `Then::Invoke` button: no field input, deduplicated by
/// the operator's click claim on that button.
async fn submit_click(s: &Submit<'_>) -> Response {
    if let Err(msg) = actions::check_click_keys(s.body) {
        return toast(s.p, StatusCode::BAD_REQUEST, msg);
    }
    let entity = match actions::first(s.body, actions::FIELD_ENTITY) {
        None => None,
        Some(raw) => match actions::parse_entity(raw) {
            Some(e) => Some(e),
            None => return toast(s.p, StatusCode::BAD_REQUEST, format!("{} must be kind:id", actions::FIELD_ENTITY)),
        },
    };
    let decl = s.found.decl;
    if !matches!(decl.then, Then::Invoke) || matches!(decl.point, ActionPoint::PanelForm(_)) {
        let msg = format!("{}/{} must be submitted from its form", s.who.module, decl.id);
        return toast(s.p, StatusCode::BAD_REQUEST, msg);
    }
    let item = actions::first(s.body, actions::FIELD_ITEM);
    if item.is_some_and(|i| !actions::validate_item(i)) {
        return toast(s.p, StatusCode::BAD_REQUEST, "item is empty, too long or has control characters".to_string());
    }
    let id = actions::click_row_id(&s.who, entity.as_ref(), item);
    let row = match take_click(s, &id, entity.as_ref(), item).await {
        Ok(row) => row,
        Err(resp) => return *resp,
    };
    let pool = &s.p.st.pool;
    if let Err(refusal) = actions::admit(s.operator, decl, s.found.writes) {
        forms::settle(pool, &row, false).await;
        return toast(s.p, StatusCode::FORBIDDEN, refusal.hint().to_string());
    }
    let req = InvokeCall {
        module: s.who.module.clone(),
        action: decl.id.clone(),
        entity,
        input: BTreeMap::new(),
        expected_revision: None,
        reason: None,
        operator: s.operator.login.clone(),
        item: item.map(str::to_string),
        request_id: Some(actions::click_request_id(&row)),
    };
    let invoked = actions::invoke(s.found.face.as_ref(), req.clone()).await;
    forms::settle(pool, &row, actions::consumes(&invoked)).await;
    if let Some(resp) = audit(s, &req, &invoked).await {
        return resp;
    }
    answer(s, None, &req, invoked).await
}

/// Appends the attempt's `admin.action`. Only an applied action escalates a failed append
/// to a 500: any other answer would mask the outcome the operator must act on.
async fn audit(s: &Submit<'_>, req: &InvokeCall, invoked: &Invoked) -> Option<Response> {
    let Err(e) = actions::record(&s.p.st, s.operator, req, invoked).await else {
        return None;
    };
    if matches!(invoked, Invoked::Outcome(InvokeOutcome::Ok { .. })) {
        tracing::error!(err = %e, module = %req.module, action = %req.action, "admin.action append failed");
        let msg = "action applied but audit append failed".to_string();
        return Some(toast(s.p, StatusCode::INTERNAL_SERVER_ERROR, msg));
    }
    tracing::warn!(err = %e, module = %req.module, action = %req.action, "admin.action append failed");
    None
}

async fn flash_redirect(s: &Submit<'_>, target: String, flash: Flash) -> Response {
    if flash.message.is_empty() && flash.reveal.is_none() {
        return redirect(s.headers, &target);
    }
    match actions::stash_flash(&s.p.st.pool, &flash).await {
        Ok(token) => redirect(s.headers, &actions::with_flash(&target, &token)),
        // The provider already applied the action; the show-once value is lost
        // rather than ever served twice.
        Err(err) => {
            tracing::error!(%err, "admin flash: stash failed; redirecting without it");
            redirect(s.headers, &target)
        }
    }
}

const CONFLICT_BANNER: &str = "This changed since you opened it. Reload the page and try again.";

/// Maps a settled invoke to its answer; `row` is the form instance, `None` for a click,
/// whose 422 and 409 are toasts.
async fn answer(s: &Submit<'_>, row: Option<&FormRow>, req: &InvokeCall, invoked: Invoked) -> Response {
    let status = actions::http_status(&invoked);
    let conflict = |banner: String| match row {
        Some(row) => reshow_locked(s, row, banner, status),
        None => toast(s.p, status, "This changed since you opened it — reload".to_string()),
    };
    match invoked {
        Invoked::Outcome(InvokeOutcome::Ok { message, reveal, redirect: link }) => {
            let target = actions::ok_target(link.as_ref(), s.body, req.entity.as_ref());
            flash_redirect(s, target, Flash { message, reveal, tone: FlashTone::Success }).await
        }
        Invoked::Outcome(InvokeOutcome::NoOp { message }) => {
            let target = actions::ok_target(None, s.body, req.entity.as_ref());
            flash_redirect(s, target, Flash { message, reveal: None, tone: FlashTone::Info }).await
        }
        Invoked::Outcome(InvokeOutcome::FieldErrors(errors)) => match row {
            Some(row) => reshow(s, row, &errors, None, status),
            None => {
                let joined: Vec<String> = errors.iter().map(|(k, v)| format!("{k}: {v}")).collect();
                toast(s.p, status, joined.join("; "))
            }
        },
        Invoked::Outcome(InvokeOutcome::Conflict { current_revision }) => conflict(format!(
            "This changed since you opened it (now revision {current_revision}). Reload the page and try again."
        )),
        Invoked::Failed(error) if error.status == opsapi::Status::Conflict => conflict(CONFLICT_BANNER.to_string()),
        Invoked::Failed(error) => toast(s.p, status, error.to_string()),
        Invoked::TimedOut => toast(
            s.p,
            status,
            format!("no answer within {}ms; the outcome is unknown", actions::INVOKE_DEADLINE.as_millis()),
        ),
    }
}

fn reshow_locked(s: &Submit<'_>, row: &FormRow, banner: String, status: StatusCode) -> Response {
    render_instance(s, row, &BTreeMap::new(), Some(banner), true, status)
}

fn reshow(
    s: &Submit<'_>,
    row: &FormRow,
    errors: &BTreeMap<String, String>,
    banner: Option<String>,
    status: StatusCode,
) -> Response {
    render_instance(s, row, errors, banner, false, status)
}

/// The form instance `row` shown again from its stored spec with the posted body and the
/// SAME `_form`: in its panel when it is a panel form, else in its modal.
fn render_instance(
    s: &Submit<'_>,
    row: &FormRow,
    errors: &BTreeMap<String, String>,
    banner: Option<String>,
    locked: bool,
    status: StatusCode,
) -> Response {
    let post_url = action_url(&row.module, &row.action);
    if let (Some(contribution), StoredSpec::Form(spec)) = (&row.contribution, &row.spec) {
        let Some((manifest, decl)) = s
            .snapshot
            .route(&row.module)
            .and_then(|(m, _)| m.contributions.iter().find(|c| &c.id == contribution).map(|c| (m, c)))
        else {
            return not_found(s.p, &format!("{}/{contribution}", row.module));
        };
        let path = panel_url(&row.module, contribution);
        let query: Vec<(String, String)> =
            row.entity.iter().map(|e| ("entity".to_string(), render::entity_param(e))).collect();
        let tools = panel_tools(s.operator, s.csrf, &row.module, manifest, contribution);
        let frame = FormFrame {
            module: &row.module,
            decl,
            mode: render::mode_of(&decl.point, in_tiles(manifest, decl)),
            path: &path,
            query: &query,
            entity: row.entity.as_ref(),
            actions: &tools,
            csrf: s.csrf,
        };
        let instance = FormInstance { post_url, form_id: row.id.clone(), posted: Some(s.body), errors, banner, locked };
        let rendered = render::panel_form_result(&frame, spec, instance);
        return html(status, render::fragment(&s.p.st.env, "primitives.html", "panel_fragment", &rendered));
    }
    let Some(shown) = modal_spec(&row.spec) else {
        return toast(s.p, status, "This action has no form to show again".to_string());
    };
    let mut modal =
        render::modal(s.found.decl, shown, post_url, s.csrf.to_string(), row.id.clone(), Some(s.body), errors);
    if banner.is_some() {
        modal.banner = banner;
    }
    modal.locked = locked;
    render_result(s.p, status, &ActionResultView { modal: Some(modal), toast: None, tone: "err" })
}

fn render_result(p: &Portal, status: StatusCode, view: &ActionResultView) -> Response {
    html(status, render::fragment(&p.st.env, "modal.html", "result", view))
}
