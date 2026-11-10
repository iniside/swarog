//! `controlplane` — the admin control-plane contract. A module implements [`Provider`]
//! and contributes it to [`PROVIDER_SLOT`]; [`ControlPlane`] is one process's face over
//! its providers. No HTML crosses this contract.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use rpc_macro::rpc;
use serde::{Deserialize, Serialize};

pub mod events;

/// What a module writes: one static manifest plus the panel, search and invoke handlers.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Static; does no I/O.
    fn manifest(&self) -> Manifest;
    async fn panel(&self, req: PanelRequest) -> Result<Primitive, opsapi::Error>;
    async fn search(&self, req: SearchRequest) -> Result<Vec<SearchHit>, opsapi::Error>;
    async fn invoke(&self, req: InvokeRequest) -> Result<InvokeOutcome, opsapi::Error>;
}

pub const PROVIDER_SLOT: contrib::Slot<Arc<dyn Provider>> =
    contrib::Slot::new("controlplane.provider");

/// One process's providers behind one face; implemented by core, never by a module.
#[rpc(prefix = "cp")]
#[async_trait]
pub trait ControlPlane: Send + Sync {
    #[retry_safe]
    async fn manifests(&self) -> Result<Vec<Manifest>, opsapi::Error>;
    #[retry_safe]
    async fn panel(&self, req: crate::PanelRequest) -> Result<Primitive, opsapi::Error>;
    #[retry_safe]
    async fn search(&self, req: crate::SearchRequest) -> Result<Vec<SearchHit>, opsapi::Error>;
    async fn invoke(&self, req: crate::InvokeRequest) -> Result<InvokeOutcome, opsapi::Error>;
}

/// A peer process's [`ControlPlane`] face.
#[derive(Clone)]
pub struct Source {
    pub peer: String,
    pub face: Arc<dyn ControlPlane>,
}

pub const SOURCE_SLOT: contrib::Slot<Source> = contrib::Slot::new("controlplane.source");

/// The fixed sidebar sections as `(id, label, order)`.
pub const SECTIONS: &[(&str, &str, i32)] = &[
    ("overview", "Overview", 0),
    ("players", "Players", 10),
    ("economy", "Economy", 30),
    ("platform", "Platform", 40),
];

/// The default [`ContributionDecl::deadline_ms`].
pub const DEFAULT_PANEL_DEADLINE_MS: u32 = 2000;
/// The default [`SearchDecl::deadline_ms`].
pub const DEFAULT_SEARCH_DEADLINE_MS: u32 = 800;

/// Serves one process's local providers, dispatching each request by `req.module`.
pub struct LocalFace {
    manifests: Vec<Manifest>,
    by_module: BTreeMap<String, LocalProvider>,
}

struct LocalProvider {
    provider: Arc<dyn Provider>,
    writes: bool,
}

impl LocalFace {
    /// Reads every provider's manifest once. Panics when two providers declare the same
    /// `Manifest.module`.
    pub fn new(providers: Vec<Arc<dyn Provider>>) -> Self {
        let mut manifests = Vec::with_capacity(providers.len());
        let mut by_module = BTreeMap::new();
        for provider in providers {
            let manifest = provider.manifest();
            let local = LocalProvider { provider, writes: manifest.writes };
            // Startup-only: a duplicate makes routing ambiguous, so the process refuses to boot.
            if by_module.insert(manifest.module.clone(), local).is_some() {
                panic!("controlplane: two providers declare module {:?}", manifest.module);
            }
            manifests.push(manifest);
        }
        LocalFace { manifests, by_module }
    }

    fn local(&self, module: &str) -> Result<&LocalProvider, opsapi::Error> {
        self.by_module.get(module).ok_or_else(|| {
            opsapi::Error::not_found(format!("no control-plane provider for module {module:?}"))
        })
    }
}

#[async_trait]
impl ControlPlane for LocalFace {
    async fn manifests(&self) -> Result<Vec<Manifest>, opsapi::Error> {
        Ok(self.manifests.clone())
    }

    async fn panel(&self, req: PanelRequest) -> Result<Primitive, opsapi::Error> {
        let mut primitive = self.local(&req.module)?.provider.panel(req).await?;
        make_finite(&mut primitive);
        Ok(primitive)
    }

    async fn search(&self, req: SearchRequest) -> Result<Vec<SearchHit>, opsapi::Error> {
        self.local(&req.module)?.provider.search(req).await
    }

    async fn invoke(&self, req: InvokeRequest) -> Result<InvokeOutcome, opsapi::Error> {
        let local = self.local(&req.module)?;
        // `Manifest.writes` is the one write authority: an undeclared writer never sees an invoke.
        if !local.writes {
            return Err(opsapi::Error::forbidden(format!(
                "control-plane module {:?} does not declare writes",
                req.module
            )));
        }
        local.provider.invoke(req).await
    }
}

// serde_json writes a non-finite f64 as `null`, which then fails to decode as f64.
fn make_finite(primitive: &mut Primitive) {
    fn fix(value: &mut f64) {
        if !value.is_finite() {
            *value = 0.0;
        }
    }
    match primitive {
        Primitive::KpiTile(tile) => {
            tile.series.iter_mut().for_each(fix);
            if let Some((used, total)) = &mut tile.capacity {
                fix(used);
                fix(total);
            }
        }
        Primitive::TimeSeriesChart(chart) => {
            for series in &mut chart.series {
                series.values.iter_mut().for_each(fix);
            }
        }
        Primitive::DataTable(_)
        | Primitive::PropertyList(_)
        | Primitive::Timeline(_)
        | Primitive::Form(_)
        | Primitive::WorkQueue(_)
        | Primitive::ServiceStatus(_)
        | Primitive::AttentionItems(_)
        | Primitive::ColumnCells(_)
        | Primitive::Cell(_)
        | Primitive::EntityHeader(_)
        | Primitive::Empty { .. } => {}
    }
}

// ---------------------------------------------------------------------------
// Roles
// ---------------------------------------------------------------------------

/// Operator role, ordered `Viewer < Operator < Admin`. Only the portal compares roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Role {
    Viewer,
    Operator,
    Admin,
}

// ---------------------------------------------------------------------------
// Requests and outcomes
// ---------------------------------------------------------------------------

/// An entity reference, rendered as `kind:id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityRef {
    pub kind: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Link {
    Page { module: String, page: String, params: BTreeMap<String, String> },
    Entity(EntityRef),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelRequest {
    pub module: String,
    pub contribution: String,
    pub entity: Option<EntityRef>,
    pub params: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchRequest {
    pub module: String,
    pub kind: String,
    pub q: String,
    pub limit: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub target: EntityRef,
    pub title: String,
    pub subtitle: String,
    pub badge: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvokeRequest {
    pub module: String,
    pub action: String,
    pub entity: Option<EntityRef>,
    pub input: BTreeMap<String, String>,
    pub expected_revision: Option<u64>,
    pub reason: Option<String>,
    /// The operator's login, carried only as an audit label.
    pub operator: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvokeOutcome {
    Ok { message: String, reveal: Option<SecretReveal>, redirect: Option<Link> },
    FieldErrors(BTreeMap<String, String>),
    Conflict { current_revision: u64 },
}

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

/// A module's static declaration of everything it contributes to the portal.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub module: String,
    pub sections: Vec<NavSection>,
    pub entries: Vec<NavEntry>,
    pub pages: Vec<PageDecl>,
    pub entity_kinds: Vec<EntityKindDecl>,
    pub contributions: Vec<ContributionDecl>,
    pub actions: Vec<ActionDecl>,
    pub search: Vec<SearchDecl>,
    pub writes: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavSection {
    pub id: String,
    pub label: String,
    pub order: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavEntry {
    pub section: String,
    pub id: String,
    pub label: String,
    pub icon: String,
    pub order: i32,
    pub page: String,
    pub counter: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageDecl {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub head_actions: Vec<String>,
    pub layout: Layout,
}

/// Page layout; every string is a contribution id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Layout {
    Stack(Vec<String>),
    Grid21(Vec<String>, Vec<String>),
    Grid2(Vec<String>, Vec<String>),
    Tiles(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityKindDecl {
    pub kind: String,
    pub label: String,
    pub icon: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContributionDecl {
    pub id: String,
    pub point: Point,
    pub order: i32,
    pub title: String,
    pub icon: String,
    pub min_role: Role,
    pub cache_ttl_s: Option<u32>,
    pub deadline_ms: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Point {
    Page(String),
    DashboardTile,
    DashboardAttention,
    DashboardPanel,
    EntityHeader(String),
    EntityTab { kind: String, tab: String, tab_label: String },
    EntityTabCount { kind: String, tab: String },
    EntitySummary(String),
    EntityListColumn { kind: String, label: String },
    NavCounter,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionDecl {
    pub id: String,
    pub point: ActionPoint,
    pub label: String,
    pub icon: String,
    pub order: i32,
    pub danger: bool,
    pub multi: bool,
    pub min_role: Role,
    pub then: Then,
    pub implemented: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionPoint {
    RowAction(String),
    HeaderAction(String),
    PageHead(String),
    PanelTool(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Then {
    Invoke,
    Form(FormSpec),
    Confirm(ConfirmSpec),
    Link(Link),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormSpec {
    pub fields: Vec<Field>,
    pub hidden: BTreeMap<String, String>,
    pub submit_label: String,
    pub note: Option<String>,
    pub revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
    pub required: bool,
    pub help: Option<String>,
    pub default: Option<String>,
    pub readonly: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldKind {
    String,
    Text,
    Int,
    Float,
    Bool,
    Enum(Vec<String>),
    Multi(Vec<String>),
    Json,
    Ref(String),
    Money(Vec<String>),
    Duration,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmSpec {
    pub title: String,
    pub body: String,
    pub tone: ConfirmTone,
    pub impact: Vec<(String, String)>,
    pub reason: ReasonReq,
    pub type_to_confirm: Option<String>,
    pub confirm_label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfirmTone {
    Danger,
    Warn,
    Info,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReasonReq {
    Required,
    Optional,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchDecl {
    pub kind: String,
    pub label: String,
    pub icon: String,
    pub deadline_ms: u32,
}

// ---------------------------------------------------------------------------
// Primitive
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tone {
    Ok,
    Warn,
    Err,
    Info,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Align {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trend {
    Up,
    Down,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CellKind {
    Text,
    Mono,
    Badge,
    Ref,
    Number,
    Bytes,
    Time,
    User,
}

/// One value; numbers arrive preformatted by the module.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub kind: CellKind,
    pub value: String,
    pub tone: Option<Tone>,
    pub link: Option<Link>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub key: String,
    pub label: String,
    pub kind: CellKind,
    pub sortable: bool,
    pub align: Align,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub entity: Option<EntityRef>,
    pub cells: Vec<Cell>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageInfo {
    pub next_cursor: Option<String>,
    pub size: u32,
    pub total: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterKind {
    Enum(Vec<String>),
    Text,
    Range,
    Bool,
    Ref(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterDef {
    pub key: String,
    pub label: String,
    pub kind: FilterKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataTable {
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
    pub page: PageInfo,
    pub filters: Vec<FilterDef>,
    pub search_placeholder: Option<String>,
    pub selectable: bool,
    /// The entity kind whose `EntityListColumn` contributions apply to this table.
    pub list_columns_for: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropertyList {
    pub items: Vec<(String, Cell)>,
    pub columns: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    pub value: String,
    pub period: String,
    pub good: Trend,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KpiTile {
    pub label: String,
    pub value: String,
    pub unit: Option<String>,
    pub delta: Option<Delta>,
    pub series: Vec<f64>,
    pub capacity: Option<(f64, f64)>,
    pub link: Option<Link>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineEntry {
    /// RFC 3339, UTC.
    pub at: String,
    pub actor: Option<EntityRef>,
    pub verb: String,
    pub target: Option<EntityRef>,
    /// Plain text.
    pub summary: String,
    pub detail: Option<String>,
    pub icon: String,
    pub tone: Option<Tone>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timeline {
    pub entries: Vec<TimelineEntry>,
    pub cursor_before: Option<String>,
    pub live: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueState {
    pub key: String,
    pub label: String,
    pub count: u64,
    pub tone: Option<Tone>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: String,
    pub entity: Option<EntityRef>,
    pub state: String,
    pub attempts: u32,
    pub max_attempts: u32,
    pub next_attempt_at: Option<String>,
    pub last_error: Option<String>,
    pub payload_preview: Option<String>,
    /// `ActionDecl` ids.
    pub actions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkQueue {
    pub states: Vec<QueueState>,
    pub selected: String,
    pub items: Vec<QueueItem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceState {
    Running,
    Degraded,
    Restarting,
    Crashed,
    Down,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceItem {
    pub name: String,
    pub state: ServiceState,
    pub version: Option<String>,
    pub p99_ms: Option<u32>,
    pub uptime: Option<String>,
    pub note: Option<String>,
    pub history: Vec<(String, ServiceState)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub items: Vec<ServiceItem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    Left,
    Right,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Series {
    pub label: String,
    pub unit: String,
    pub values: Vec<f64>,
    pub axis: Axis,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimeSeriesChart {
    pub buckets: Vec<String>,
    pub series: Vec<Series>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Err,
    Warn,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionItem {
    pub severity: Severity,
    pub title: String,
    pub text: String,
    pub link: Link,
    pub action_label: String,
    pub since: String,
}

/// A show-once secret; `Debug` redacts `value`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretReveal {
    pub label: String,
    pub value: String,
    pub hint: Option<String>,
    pub expires_at: Option<String>,
}

impl std::fmt::Debug for SecretReveal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretReveal")
            .field("label", &self.label)
            .field("value", &"<redacted>")
            .field("hint", &self.hint)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityHeader {
    pub title: String,
    pub subtitle: String,
    pub status: Option<Cell>,
    pub avatar_seed: String,
}

/// The closed set of things a panel can return.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Primitive {
    DataTable(DataTable),
    PropertyList(PropertyList),
    KpiTile(KpiTile),
    Timeline(Timeline),
    Form(FormSpec),
    WorkQueue(WorkQueue),
    ServiceStatus(ServiceStatus),
    TimeSeriesChart(TimeSeriesChart),
    AttentionItems(Vec<AttentionItem>),
    ColumnCells(BTreeMap<String, Cell>),
    Cell(Cell),
    EntityHeader(EntityHeader),
    Empty { title: String, detail: String },
}
