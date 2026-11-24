//! `controlplane` — the admin control-plane contract. A module implements [`Provider`]
//! and contributes it to [`PROVIDER_SLOT`]; [`ControlPlane`] is one process's face over
//! its providers. No HTML crosses this contract.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use rpc_macro::rpc;
use serde::{Deserialize, Serialize};

/// An enum's variant names, generated from the enum's own definition.
pub trait Variants {
    /// The enum's name.
    const NAME: &'static str;
    /// Every variant, in declaration order.
    const VARIANTS: &'static [&'static str];
    /// This value's variant.
    fn variant(&self) -> &'static str;
}

/// An enum declared through `variants!`: its name, its variants, and whether the portal draws it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeclaredEnum {
    pub name: &'static str,
    pub variants: &'static [&'static str],
    /// `true` for `variants! { drawn; .. }`, `false` for `variants! { undrawn; .. }`.
    pub drawn: bool,
}

#[linkme::distributed_slice]
static DECLARED_ENUMS: [DeclaredEnum];

/// Every enum declared through `variants!`, in no fixed order.
pub fn declared_enums() -> &'static [DeclaredEnum] {
    &DECLARED_ENUMS
}

/// Emits the enum as written plus its [`Variants`] impl, and registers it in
/// [`declared_enums`] as drawn or undrawn by the portal.
macro_rules! variants {
    (drawn; $($item:tt)*) => {
        variants!(@emit true; $($item)*);
    };
    (undrawn; $($item:tt)*) => {
        variants!(@emit false; $($item)*);
    };
    (
        @emit $drawn:literal;
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[$vmeta:meta])*
                $variant:ident
                $( ( $($tuple:ty),* $(,)? ) )?
                $( { $( $(#[$fmeta:meta])* $field:ident : $fty:ty ),* $(,)? } )?
            ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis enum $name {
            $(
                $(#[$vmeta])*
                $variant
                $( ( $($tuple),* ) )?
                $( { $( $(#[$fmeta])* $field : $fty ),* } )?
            ),*
        }

        impl Variants for $name {
            const NAME: &'static str = stringify!($name);
            const VARIANTS: &'static [&'static str] = &[$(stringify!($variant)),*];

            fn variant(&self) -> &'static str {
                match self {
                    $(Self::$variant { .. } => stringify!($variant),)*
                }
            }
        }

        const _: () = {
            #[linkme::distributed_slice(DECLARED_ENUMS)]
            static ENTRY: DeclaredEnum = DeclaredEnum {
                name: <$name as Variants>::NAME,
                variants: <$name as Variants>::VARIANTS,
                drawn: $drawn,
            };
        };
    };
}

pub mod events;
pub mod order;

/// What a module writes: one static manifest plus the panel, search, invoke and prepare
/// handlers.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Static; does no I/O.
    fn manifest(&self) -> Manifest;
    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, opsapi::Error>;
    async fn search(&self, req: SearchQuery) -> Result<Vec<SearchHit>, opsapi::Error>;
    async fn invoke(&self, req: InvokeCall) -> Result<InvokeOutcome, opsapi::Error>;
    /// The spec a `prepare: true` action's modal shows for `req.item`, of the declared
    /// `Then` variant. Only actions declaring `prepare` are asked. It must write nothing:
    /// `ControlPlane::prepare` is `#[retry_safe]`, so a call may run more than once.
    async fn prepare(&self, req: PrepareCall) -> Result<Then, opsapi::Error> {
        Err(opsapi::Error::not_found(format!("{} prepares no action {:?}", req.module, req.action)))
    }
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
    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, opsapi::Error>;
    #[retry_safe]
    async fn search(&self, req: SearchQuery) -> Result<Vec<SearchHit>, opsapi::Error>;
    async fn invoke(&self, req: InvokeCall) -> Result<InvokeOutcome, opsapi::Error>;
    #[retry_safe]
    async fn prepare(&self, req: PrepareCall) -> Result<Then, opsapi::Error>;
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

/// One sidebar section after [`merge_sections`]; `owner` is the module whose declaration
/// won, `None` for a fixed [`SECTIONS`] entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergedSection<'a> {
    pub section: NavSection,
    pub owner: Option<&'a str>,
}

/// A module's section declaration that did not win its id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LostSection<'a> {
    pub module: &'a str,
    pub declared: &'a NavSection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionMerge<'a> {
    /// Sorted by `(order, id)`.
    pub sections: Vec<MergedSection<'a>>,
    /// In merge order: by module name, then declaration order within a module.
    pub lost: Vec<LostSection<'a>>,
}

/// Merges [`SECTIONS`] with every manifest's `sections`. A fixed section wins its id;
/// otherwise the lexicographically smallest module wins, with its first declaration on a
/// repeat within that module.
pub fn merge_sections<'a>(manifests: impl IntoIterator<Item = &'a Manifest>) -> SectionMerge<'a> {
    let mut by_id: BTreeMap<String, MergedSection<'a>> = SECTIONS
        .iter()
        .map(|(id, label, order)| {
            let section = NavSection { id: id.to_string(), label: label.to_string(), order: *order };
            (id.to_string(), MergedSection { section, owner: None })
        })
        .collect();
    let mut declared: Vec<(&'a str, &'a NavSection)> = manifests
        .into_iter()
        .flat_map(|m| m.sections.iter().map(move |s| (m.module.as_str(), s)))
        .collect();
    declared.sort_by(|a, b| a.0.cmp(b.0));
    let mut lost = Vec::new();
    for (module, section) in declared {
        if by_id.contains_key(&section.id) {
            lost.push(LostSection { module, declared: section });
        } else {
            by_id.insert(section.id.clone(), MergedSection { section: section.clone(), owner: Some(module) });
        }
    }
    let mut sections: Vec<MergedSection<'a>> = by_id.into_values().collect();
    sections.sort_by(|a, b| (a.section.order, &a.section.id).cmp(&(b.section.order, &b.section.id)));
    SectionMerge { sections, lost }
}

/// The default [`ContributionDecl::deadline_ms`].
pub const DEFAULT_PANEL_DEADLINE_MS: u32 = 2000;
/// The default [`SearchDecl::deadline_ms`].
pub const DEFAULT_SEARCH_DEADLINE_MS: u32 = 800;
/// The byte cap every writing provider applies to each `InvokeCall::input` value before
/// it reads one.
pub const MAX_INPUT_VALUE_BYTES: usize = 4096;
/// The most entity ids one [`PanelQuery::refs`] carries. The portal splits a longer list
/// into calls of at most this many; [`LocalFace`] refuses a longer one before the provider
/// runs.
pub const MAX_PANEL_REFS: usize = 100;

/// The hash every FAKE stand-in for data the backend does not store is drawn from: a pure
/// function of `salt` and an entity id, reading no clock and no random source.
pub fn fake_hash(salt: &str, id: &str) -> u64 {
    // FNV-1a over u64 with wrapping arithmetic, not std's seeded hashers, so an id draws the
    // same fake on every platform, build and process.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in salt.bytes().chain([0]).chain(id.bytes()) {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Serves one process's local providers, dispatching each request by `req.module`.
pub struct LocalFace {
    manifests: Vec<Manifest>,
    by_module: BTreeMap<String, LocalProvider>,
}

struct LocalProvider {
    provider: Arc<dyn Provider>,
    manifest: Manifest,
}

impl LocalFace {
    /// Reads every provider's manifest once. Panics when two providers declare the same
    /// `Manifest.module`.
    pub fn new(providers: Vec<Arc<dyn Provider>>) -> Self {
        let mut manifests = Vec::with_capacity(providers.len());
        let mut by_module = BTreeMap::new();
        for provider in providers {
            let manifest = provider.manifest();
            let local = LocalProvider { provider, manifest: manifest.clone() };
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

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, opsapi::Error> {
        if req.refs.len() > MAX_PANEL_REFS {
            return Err(opsapi::Error::invalid(format!(
                "control-plane panel request carries {} refs, over the cap of {MAX_PANEL_REFS}",
                req.refs.len()
            )));
        }
        let mut body = self.local(&req.module)?.provider.panel(req).await?;
        make_finite(&mut body.primitive);
        Ok(body)
    }

    async fn search(&self, req: SearchQuery) -> Result<Vec<SearchHit>, opsapi::Error> {
        self.local(&req.module)?.provider.search(req).await
    }

    async fn invoke(&self, req: InvokeCall) -> Result<InvokeOutcome, opsapi::Error> {
        let local = self.local(&req.module)?;
        if local.manifest.actions.iter().any(|a| a.id == req.action && !a.then.invokes()) {
            return Err(opsapi::Error::invalid(format!(
                "control-plane action {:?} of module {:?} is a link and has no invoke",
                req.action, req.module
            )));
        }
        // `Manifest::write_refusal` is the one write authority: an invoke it refuses never
        // reaches the provider.
        if let Some(refusal) = local.manifest.write_refusal(&req.action) {
            return Err(opsapi::Error::forbidden(match refusal.readonly {
                None => format!("control-plane module {:?} does not declare writes", req.module),
                Some((contribution, _)) => {
                    format!("control-plane contribution {contribution:?} of module {:?} is read-only", req.module)
                }
            }));
        }
        local.provider.invoke(req).await
    }

    async fn prepare(&self, req: PrepareCall) -> Result<Then, opsapi::Error> {
        self.local(&req.module)?.provider.prepare(req).await
    }
}

// serde_json writes a non-finite f64 as `null`, which then fails to decode as f64.
fn make_finite(primitive: &mut Primitive) {
    fn fix(value: &mut f64) {
        if !value.is_finite() {
            *value = 0.0;
        }
    }
    fn fix_tile(tile: &mut KpiTile) {
        tile.series.iter_mut().for_each(fix);
        if let Some((used, total)) = &mut tile.capacity {
            fix(used);
            fix(total);
        }
    }
    match primitive {
        Primitive::KpiTile(tile) => fix_tile(tile),
        Primitive::KpiTiles(tiles) => tiles.iter_mut().for_each(fix_tile),
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

variants! {
    drawn;
    /// Operator role, ordered `Viewer < Operator < Admin`. Only the portal compares roles.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    pub enum Role {
        Viewer,
        Operator,
        Admin,
    }
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

variants! {
    drawn;
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum Link {
        Page { module: String, page: String, params: BTreeMap<String, String> },
        Entity(EntityRef),
    }
}

/// The `Link::Page` param the portal passes to each of the page's panels as
/// `PanelQuery::search`.
pub const PAGE_PARAM_SEARCH: &str = "q";
/// The `Link::Page` param the portal passes to each of the page's panels as
/// `PanelQuery::queue_state`.
pub const PAGE_PARAM_QUEUE_STATE: &str = "state";
/// The module of core admin's Events page, the `Link::Page` target for event-plane state.
pub const EVENT_LOG_PAGE_MODULE: &str = "core.events";
/// The page id of core admin's Events page within `EVENT_LOG_PAGE_MODULE`.
pub const EVENT_LOG_PAGE: &str = "events";

/// The dashboard's "<X> purges pending" attention item of a module that tombstones deleted
/// players and drains them with a sweep subscription: no item at 0 pending, so the dashboard
/// draws nothing, otherwise one warn item naming the backlog and the oldest tombstone's day,
/// linking the Events page filtered to `sweep_sub`, whose lag and pause state explain a
/// backlog. `oldest` is the oldest tombstone's UTC day, `YYYY-MM-DD`, drawn as given. Each
/// purging module's admin provider calls this rather than drawing its own.
pub fn purge_backlog_body(title: &str, pending: i64, oldest: &str, sweep_sub: &str) -> PanelBody {
    if pending == 0 {
        return PanelBody::new(Primitive::AttentionItems(Vec::new()));
    }
    let players = if pending == 1 { "1 player".to_string() } else { format!("{pending} players") };
    PanelBody::new(Primitive::AttentionItems(vec![AttentionItem {
        severity: Severity::Warn,
        title: title.to_string(),
        text: format!("{players} awaiting sweep · oldest {oldest}"),
        link: Link::Page {
            module: EVENT_LOG_PAGE_MODULE.to_string(),
            page: EVENT_LOG_PAGE.to_string(),
            params: BTreeMap::from([(PAGE_PARAM_SEARCH.to_string(), sweep_sub.to_string())]),
        },
        action_label: "Events".to_string(),
        since: String::new(),
    }]))
}

/// A portal row's relative age from whole seconds, truncated to the largest unit that fits:
/// `"now"` under a minute, then `"N min ago"`, `"N h ago"`, `"N d ago"`. A negative age
/// (a writer's clock ahead of the reader's) reads as `"now"`.
pub fn ago(secs: i64) -> String {
    match secs.max(0) {
        s if s < 60 => "now".to_string(),
        s if s < 3_600 => format!("{} min ago", s / 60),
        s if s < 86_400 => format!("{} h ago", s / 3_600),
        s => format!("{} d ago", s / 86_400),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelQuery {
    pub module: String,
    pub contribution: String,
    pub entity: Option<EntityRef>,
    /// A data table's search text.
    pub search: Option<String>,
    /// A data table's next-page cursor, from `PageInfo::next_cursor`.
    pub cursor: Option<String>,
    /// The timeline position older entries are read from, from `Timeline::cursor_before`.
    pub before: Option<String>,
    /// The selected work-queue state, a `QueueState::key`.
    pub queue_state: Option<String>,
    /// The dashboard's time range: `Some` on every dashboard-point request, `None`
    /// elsewhere. A contribution may ignore it.
    pub range: Option<TimeRange>,
    /// Module-specific keys only.
    pub params: BTreeMap<String, String>,
    /// The distinct entity ids an `EntityListColumn` answers for one table page, or an
    /// `EntityLabel` for one slot's body, in first-drawn order, at most [`MAX_PANEL_REFS`];
    /// empty on every other request.
    pub refs: Vec<String>,
}

variants! {
    drawn;
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub enum TimeRange {
        H1,
        H24,
        D7,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub module: String,
    pub kind: String,
    pub q: String,
    pub limit: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    /// The hit's one destination: an entity's page or a module page.
    pub link: Link,
    pub title: String,
    pub subtitle: String,
    pub badge: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvokeCall {
    pub module: String,
    pub action: String,
    pub entity: Option<EntityRef>,
    pub input: BTreeMap<String, String>,
    pub expected_revision: Option<u64>,
    pub reason: Option<String>,
    /// The calling operator's login, set by the portal from the session. A provider may
    /// store it as the author of what the call writes.
    pub operator: String,
    /// The bound element: a `PropertyList::item` or a `QueueItem::id`. Unverified: the
    /// module checks it against its own data.
    pub item: Option<String>,
    /// The module's de-duplication key: equal for a repeat of one logical submit, new for
    /// a deliberate repeat.
    pub request_id: Option<String>,
}

/// Asks for the spec of a `prepare: true` action's modal, bound to the element the
/// operator opened it on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrepareCall {
    pub module: String,
    pub action: String,
    pub entity: Option<EntityRef>,
    /// As `InvokeCall::item`; the provider builds the text from it, never from a fresh read
    /// of the element current now.
    pub item: Option<String>,
    /// The calling operator's login, set by the portal from the session.
    pub operator: String,
}

variants! {
    undrawn;
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum InvokeOutcome {
        Ok { message: String, reveal: Option<SecretReveal>, redirect: Option<Link> },
        FieldErrors(BTreeMap<String, String>),
        Conflict { current_revision: u64 },
        /// The request was valid but there was nothing to do.
        NoOp { message: String },
    }
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

impl Manifest {
    /// The one write rule, applied by `LocalFace::invoke`, the portal's admission and
    /// admincheck: `None` when `action` may write. An action that is not invoked
    /// ([`Then::invokes`]) writes nothing and is never refused. Any other action is refused
    /// when the module declares no `writes`, or when the contribution its point names declares
    /// `readonly`. An action this manifest does not declare is judged by `writes` alone.
    pub fn write_refusal(&self, action: &str) -> Option<WriteRefusal> {
        let Some(decl) = self.actions.iter().find(|a| a.id == action) else {
            return (!self.writes).then_some(WriteRefusal { readonly: None });
        };
        if !decl.then.invokes() {
            return None;
        }
        if !self.writes {
            return Some(WriteRefusal { readonly: None });
        }
        let on = decl.point.contribution()?;
        let text = self.contributions.iter().find(|c| c.id == on)?.readonly.as_ref()?;
        Some(WriteRefusal { readonly: Some((on.to_string(), text.clone())) })
    }

    /// Whether a `Then::Form` this manifest declares names `action` as one of its
    /// [`FormSpec::modes`]: the portal then draws the action in no menu and runs it only from
    /// that form.
    pub fn is_mode_action(&self, action: &str) -> bool {
        self.actions
            .iter()
            .any(|a| matches!(&a.then, Then::Form(spec) if spec.modes.iter().any(|m| m.action == action)))
    }
}

/// Why [`Manifest::write_refusal`] refuses an action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteRefusal {
    /// The read-only contribution the action sits on and its [`ContributionDecl::readonly`]
    /// text; `None` when the module declares no writes.
    pub readonly: Option<(String, String)>,
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
    /// Contribution ids rendered as one KPI tile row above `layout`.
    pub tiles: Vec<String>,
    pub layout: Layout,
    /// The page URL keys the portal forwards to the page's panels as `PanelQuery::params`;
    /// a key neither a page laying out the contribution nor its `Point::EntityTab::params`
    /// names never reaches its provider.
    pub params: Vec<String>,
}

variants! {
    drawn;
    /// Page layout; every string is a contribution id.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum Layout {
        Stack(Vec<String>),
        Grid21(Vec<String>, Vec<String>),
        Grid2(Vec<String>, Vec<String>),
        /// Each layout below the one before it.
        Rows(Vec<Layout>),
    }
}

impl Layout {
    /// Every contribution id the layout places, in declaration order.
    pub fn ids(&self) -> Vec<&String> {
        match self {
            Layout::Stack(ids) => ids.iter().collect(),
            Layout::Grid21(a, b) | Layout::Grid2(a, b) => a.iter().chain(b).collect(),
            Layout::Rows(rows) => rows.iter().flat_map(Layout::ids).collect(),
        }
    }
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
    /// Forces the panel's read-only state with this banner text, and an entity tab holding
    /// the contribution shows a lock.
    pub readonly: Option<String>,
}

variants! {
    drawn;
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum Point {
        Page(String),
        DashboardTile,
        DashboardAttention,
        DashboardPanel,
        /// Full width, below the attention and activity grid.
        DashboardWide,
        EntityHeader(String),
        /// `columns` 2 lays the tab's panels out in two columns; the portal reads it from the
        /// tab's first contribution by `order`. `params` are the URL keys the portal forwards
        /// to this contribution as `PanelQuery::params`, as a page's `PageDecl::params` do
        /// for the panels it lays out; any other key is dropped.
        EntityTab { kind: String, tab: String, tab_label: String, columns: u8, params: Vec<String> },
        EntityTabCount { kind: String, tab: String },
        /// A KPI tile in one row above the tab's panels.
        EntityTabTile { kind: String, tab: String },
        EntitySummary(String),
        /// A column the portal adds to every `DataTable` whose `list_columns_for` is `kind`,
        /// fetched once per table page with `PanelQuery::refs`; answers `Primitive::ColumnCells`.
        EntityListColumn { kind: String, label: String, align: Align },
        /// The text the portal draws for each `PanelQuery::refs` id of `kind` a slot's body
        /// names, asked only of the module whose manifest declares `kind`; answers
        /// `Primitive::ColumnCells` with one text cell per id it knows.
        EntityLabel { kind: String },
        NavCounter,
    }
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
    /// At most one `Emphasis::Primary` per `PageDecl::head_actions`.
    pub emphasis: Emphasis,
    /// Draws the icon after the label; valid only on a `Then::Link` action.
    pub trailing_icon: bool,
    /// Draws the icon alone, the label kept as its accessible name; valid only on a
    /// `TableRow` action with an icon.
    pub icon_only: bool,
    /// Renders on an empty panel; valid only on a `PanelTool` action.
    pub when_empty: bool,
    /// The modal's spec comes from `Provider::prepare` for the bound item; the declared
    /// `Then::Confirm` or `Then::Form` fixes only its variant.
    pub prepare: bool,
}

variants! {
    drawn;
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum Emphasis {
        Primary,
        Default,
        Ghost,
    }
}

variants! {
    drawn;
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ActionPoint {
        RowAction(String),
        HeaderAction(String),
        PageHead(String),
        /// A contribution id of the same module: the action renders in that panel's tools,
        /// bound to its `PropertyList::item` when it has one.
        PanelTool(String),
        /// A contribution id of the same module whose body is a `Primitive::WorkQueue`: the
        /// action renders on each `QueueItem` listing it, bound to that item's id.
        QueueRow(String),
        /// A contribution id of the same module: the action is the Save of that
        /// contribution's `Primitive::Form` and renders nowhere else. It is `Then::Invoke`.
        PanelForm(String),
        /// A contribution id of the same module whose body is a `Primitive::DataTable`: the
        /// action renders on each `Row` listing it, bound to the row's entity, else the
        /// panel's entity, and the row's `item`.
        TableRow(String),
    }
}

impl ActionPoint {
    /// The contribution id the point places its action on; `None` for a point placed by
    /// entity kind or page.
    pub fn contribution(&self) -> Option<&str> {
        match self {
            ActionPoint::PanelTool(c)
            | ActionPoint::QueueRow(c)
            | ActionPoint::PanelForm(c)
            | ActionPoint::TableRow(c) => Some(c),
            ActionPoint::RowAction(_) | ActionPoint::HeaderAction(_) | ActionPoint::PageHead(_) => None,
        }
    }
}

variants! {
    drawn;
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum Then {
        Invoke,
        Form(FormSpec),
        Confirm(ConfirmSpec),
        Link(Link),
    }
}

impl Then {
    /// Whether the action is invoked through `cp.invoke`; a link only navigates.
    pub fn invokes(&self) -> bool {
        match self {
            Then::Invoke | Then::Form(_) | Then::Confirm(_) => true,
            Then::Link(_) => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormSpec {
    pub fields: Vec<Field>,
    pub hidden: BTreeMap<String, String>,
    pub submit_label: String,
    pub note: Option<String>,
    pub revision: Option<u64>,
    /// The modal's title; the action's label when absent. A panel form has none.
    pub title: Option<String>,
    /// The modes the form switches between, drawn as `div.seg` above its fields; empty for a
    /// form of one action. The submit runs the action of the mode the body names.
    pub modes: Vec<FormMode>,
    /// The change note the form asks for, posted as `_reason` and sent as
    /// [`InvokeCall::reason`]; `ReasonReq::None` draws no input.
    pub reason: ReasonReq,
    /// The change note's label; "Reason" when absent.
    pub reason_label: Option<String>,
    /// The change note input's placeholder.
    pub reason_hint: Option<String>,
}

/// One mode of a [`FormSpec`]: its segment's label, the submit button's label while it is
/// picked, and the action the submit runs. That action is a `Then::Invoke` action of the
/// same module at the point of the form's own action ([`form_mode_errors`]); the portal draws
/// it in no menu and runs it only from the form.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormMode {
    pub id: String,
    pub label: String,
    pub action: String,
    pub submit_label: String,
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
    pub mono: bool,
    pub placeholder: Option<String>,
}

variants! {
    drawn;
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
        /// Declared with no `default`: the portal fills it with the id of the form instance it
        /// draws, the id the submit sends as `InvokeCall::request_id`, and draws it read-only
        /// with no `name`. It never enters `InvokeCall::input`; a form drawn without an
        /// instance leaves it out.
        RequestId,
    }
}

variants! {
    drawn;
    /// A decoded field value; [`decode_field`] and [`encode_field`] are the one codec between
    /// a field's raw string (as carried in `InvokeCall::input` and `Field::default`) and it.
    #[derive(Clone, Debug, PartialEq)]
    pub enum FieldValue {
        Text(String),
        Int(i64),
        Float(f64),
        Bool(bool),
        Multi(Vec<String>),
        Money { amount: String, currency: String },
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldError {
    pub reason: String,
}

impl FieldError {
    fn new(reason: impl Into<String>) -> FieldError {
        FieldError { reason: reason.into() }
    }
}

/// Decodes `raw` as a value of `kind`. Multi is a JSON array of distinct members; Money is a
/// JSON object `{"amount": "<decimal>", "currency": "<member>"}`.
pub fn decode_field(kind: &FieldKind, raw: &str) -> Result<FieldValue, FieldError> {
    match kind {
        FieldKind::String | FieldKind::Text => Ok(FieldValue::Text(raw.to_string())),
        FieldKind::Ref(_) if raw.is_empty() => Err(FieldError::new("must not be empty")),
        FieldKind::Ref(_) => Ok(FieldValue::Text(raw.to_string())),
        FieldKind::Int => raw.parse().map(FieldValue::Int).map_err(|_| FieldError::new("must be an integer")),
        FieldKind::Float => match raw.parse::<f64>() {
            Ok(v) if v.is_finite() => Ok(FieldValue::Float(v)),
            _ => Err(FieldError::new("must be a finite number")),
        },
        FieldKind::Bool => match raw {
            "true" => Ok(FieldValue::Bool(true)),
            "false" => Ok(FieldValue::Bool(false)),
            _ => Err(FieldError::new("must be true or false")),
        },
        FieldKind::Enum(options) if options.iter().any(|o| o == raw) => Ok(FieldValue::Text(raw.to_string())),
        FieldKind::Enum(_) => Err(FieldError::new("is not one of the options")),
        FieldKind::Multi(options) => {
            let picked: Vec<String> =
                serde_json::from_str(raw).map_err(|_| FieldError::new("must be a JSON array of strings"))?;
            for (i, value) in picked.iter().enumerate() {
                if !options.contains(value) {
                    return Err(FieldError::new(format!("{value:?} is not one of the options")));
                }
                if picked[..i].contains(value) {
                    return Err(FieldError::new(format!("{value:?} is picked twice")));
                }
            }
            Ok(FieldValue::Multi(picked))
        }
        FieldKind::Json => serde_json::from_str::<serde::de::IgnoredAny>(raw)
            .map(|_| FieldValue::Text(raw.to_string()))
            .map_err(|_| FieldError::new("must be valid JSON")),
        FieldKind::Duration => {
            let digits = raw.strip_suffix(['s', 'm', 'h', 'd']).unwrap_or("");
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                Ok(FieldValue::Text(raw.to_string()))
            } else {
                Err(FieldError::new("must be digits followed by s, m, h or d"))
            }
        }
        FieldKind::Money(currencies) => {
            let (amount, currency) = decode_money(raw)
                .map_err(|MoneyShapeError| FieldError::new("must be a JSON object with amount and currency"))?;
            if !is_decimal(&amount) {
                return Err(FieldError::new("amount must be a decimal number"));
            }
            if !currencies.contains(&currency) {
                return Err(FieldError::new(format!("currency {currency:?} is not one of the options")));
            }
            Ok(FieldValue::Money { amount, currency })
        }
        FieldKind::RequestId => Err(FieldError::new("is filled by the portal and carries no value")),
    }
}

/// The raw string of `value`; `decode_field(kind, &encode_field(kind, v)) == Ok(v)` for every
/// `v` that `decode_field` can return for `kind`.
pub fn encode_field(_kind: &FieldKind, value: &FieldValue) -> String {
    match value {
        FieldValue::Text(text) => text.clone(),
        FieldValue::Int(v) => v.to_string(),
        FieldValue::Float(v) => v.to_string(),
        FieldValue::Bool(v) => v.to_string(),
        FieldValue::Multi(picked) => serde_json::to_string(picked).unwrap_or_default(),
        FieldValue::Money { amount, currency } => {
            serde_json::json!({ "amount": amount, "currency": currency }).to_string()
        }
    }
}

/// A raw Money string that is not a JSON object of exactly the string keys `amount` and
/// `currency`; each caller words its own message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MoneyShapeError;

/// The one parser of a Money field's raw string `{"amount": "<text>", "currency": "<text>"}`
/// (as [`encode_field`] writes it) into `(amount, currency)`. It checks the shape only: the
/// amount's number form and the currency's membership are the caller's rules.
pub fn decode_money(raw: &str) -> Result<(String, String), MoneyShapeError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Raw {
        amount: String,
        currency: String,
    }
    let Raw { amount, currency } = serde_json::from_str(raw).map_err(|_| MoneyShapeError)?;
    Ok((amount, currency))
}

/// `-?[0-9]+(\.[0-9]+)?`
fn is_decimal(raw: &str) -> bool {
    let unsigned = raw.strip_prefix('-').unwrap_or(raw);
    let (whole, fraction) = match unsigned.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (unsigned, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits(whole) && fraction.is_none_or(digits)
}

/// Every reason `spec` cannot be served as a form: a field key that contains `__`, starts
/// with `_` or equals a `hidden` key, a `default` that does not decode, a
/// [`FieldKind::RequestId`] field with a `default`, or a mode with an empty or repeated id.
pub fn form_spec_errors(spec: &FormSpec) -> Vec<String> {
    let mut errors = Vec::new();
    for (i, mode) in spec.modes.iter().enumerate() {
        if mode.id.is_empty() {
            errors.push("a mode has an empty id".to_string());
        } else if spec.modes[..i].iter().any(|m| m.id == mode.id) {
            errors.push(format!("mode id {:?} is repeated", mode.id));
        }
    }
    for field in &spec.fields {
        let key = &field.key;
        if key.contains("__") || key.starts_with('_') {
            errors.push(format!("field key {key:?} contains `__` or starts with `_`"));
        }
        if spec.hidden.contains_key(key) {
            errors.push(format!("field key {key:?} is also a hidden key"));
        }
        if field.kind == FieldKind::RequestId {
            if field.default.is_some() {
                errors.push(format!("field {key:?} is a request id, which the portal fills, and declares a default"));
            }
        } else if let Some(default) = &field.default {
            if let Err(e) = decode_field(&field.kind, default) {
                errors.push(format!("field {key:?} default does not decode: {}", e.reason));
            }
        }
    }
    errors
}

/// Every reason the modes of `spec`, the form of an action at `point` in `manifest`, cannot
/// be served: [`form_mode_error`] of each mode.
pub fn form_mode_errors(manifest: &Manifest, point: &ActionPoint, spec: &FormSpec) -> Vec<String> {
    spec.modes.iter().filter_map(|mode| form_mode_error(manifest, point, mode)).collect()
}

/// Why `mode` of the form of an action at `point` in `manifest` cannot be served: its action
/// is not an action of `manifest`, is not `Then::Invoke`, sets `prepare` or sits at
/// another point than `point`; or, off a `PanelForm` point, no `Then::Form` of
/// `manifest` declares it as a mode ([`Manifest::is_mode_action`]), so it would draw in a
/// menu.
pub fn form_mode_error(manifest: &Manifest, point: &ActionPoint, mode: &FormMode) -> Option<String> {
    let named = format!("mode {:?} names action {:?}", mode.id, mode.action);
    let Some(action) = manifest.actions.iter().find(|a| a.id == mode.action) else {
        return Some(format!("{named}, which is not an action of this module"));
    };
    if !matches!(action.then, Then::Invoke) || action.prepare {
        return Some(format!("{named}, which is not a Then::Invoke action without prepare"));
    }
    if action.point != *point {
        return Some(format!("{named}, which sits at {:?}, not at the form's point {point:?}", action.point));
    }
    if !matches!(point, ActionPoint::PanelForm(_)) && !manifest.is_mode_action(&action.id) {
        return Some(format!("{named}, which no declared Then::Form names as a mode"));
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmSpec {
    pub title: String,
    pub body: String,
    pub tone: ConfirmTone,
    pub impact: Vec<(String, Cell)>,
    pub reason: ReasonReq,
    /// The reason input's placeholder.
    pub reason_hint: Option<String>,
    pub type_to_confirm: Option<String>,
    pub confirm_label: String,
}

variants! {
    drawn;
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ConfirmTone {
        Danger,
        Warn,
        Info,
    }
}

variants! {
    drawn;
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ReasonReq {
        Required,
        Optional,
        None,
    }
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

variants! {
    drawn;
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum Tone {
        Ok,
        Warn,
        Err,
        Info,
        Accent,
    }
}

variants! {
    drawn;
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum Align {
        Left,
        Right,
    }
}

variants! {
    drawn;
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
        /// Dimmed plain text.
        Muted,
        /// A plain badge: no status colour unless `tone` sets one.
        Label,
        /// Dimmed monospace text.
        MonoMuted,
        /// A display-only switch, on when `value` is `"true"`; the value is not drawn.
        Toggle,
        /// Bold plain text.
        Strong,
        /// Secondary plain text, drawn `span.dim`; not a time.
        Dim,
    }
}

/// One value; numbers arrive preformatted by the module.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub kind: CellKind,
    pub value: String,
    pub tone: Option<Tone>,
    pub link: Option<Link>,
    /// A fill percentage drawn as a bar before the value; the portal clamps it to 100. With
    /// a bar, `tone` colours the fill only.
    pub bar: Option<u8>,
    /// Further values drawn after this one in the same place, space-separated.
    pub more: Vec<Cell>,
    /// This value and its `more` sit on one aligned line.
    pub row: bool,
    /// An entity whose label the cell draws without linking it, when `value` is its id; a
    /// label that does not resolve draws `value`. A cell holds a `link` or a `label`, never
    /// both: the portal refuses a provider answer carrying both.
    pub label: Option<EntityRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub key: String,
    pub label: String,
    pub kind: CellKind,
    pub sortable: bool,
    pub align: Align,
    /// Set on the column the rows are ordered by.
    pub sorted: Option<SortDir>,
    /// Long values wrap instead of staying on one line.
    pub wrap: bool,
}

variants! {
    drawn;
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum SortDir {
        Asc,
        Desc,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub entity: Option<EntityRef>,
    pub cells: Vec<Cell>,
    pub highlight: bool,
    /// The element the row's `ActionPoint::TableRow` actions act on.
    pub item: Option<String>,
    /// `ActionDecl` ids of `ActionPoint::TableRow` actions.
    pub actions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageInfo {
    pub next_cursor: Option<String>,
    pub size: u32,
    pub total: Option<u64>,
}

variants! {
    drawn;
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum FilterKind {
        Enum(Vec<String>),
        Text,
        Range,
        Bool,
        Ref(String),
        /// A toolbar select over these options; its empty first option narrows nothing.
        Select(Vec<String>),
    }
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
    /// The index in `columns` the `EntityListColumn` columns are inserted at; past the
    /// end appends them.
    pub list_columns_at: usize,
    /// The noun the foot counts rows in; `None` counts "rows".
    pub item_noun: Option<Noun>,
}

/// A counted noun: `one` names a count of exactly 1, `many` every other count.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Noun {
    pub one: String,
    pub many: String,
}

impl Noun {
    pub fn new(one: &str, many: &str) -> Noun {
        Noun { one: one.to_string(), many: many.to_string() }
    }

    /// The form that agrees with `count`.
    pub fn of(&self, count: u64) -> &str {
        if count == 1 {
            &self.one
        } else {
            &self.many
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropertyList {
    pub items: Vec<(String, Cell)>,
    pub columns: u8,
    /// The element the panel's `PanelTool` actions act on.
    pub item: Option<String>,
    /// Plain text shown below the items as a code block; the provider bounds its size.
    pub code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    pub value: String,
    pub period: String,
    /// The colour of `value`: `Ok` a good change, `Err` a bad one, `Warn` a worrying rise;
    /// `Info` and `None` neutral.
    pub tone: Option<Tone>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KpiTile {
    pub label: String,
    pub value: String,
    pub unit: Option<String>,
    pub delta: Option<Delta>,
    pub series: Vec<f64>,
    /// The sparkline's colour.
    pub spark: Palette,
    pub capacity: Option<(f64, f64)>,
    pub link: Option<Link>,
}

variants! {
    drawn;
    /// The chart colours a module may pick: the design-system `--chart-1`, `--chart-2` and
    /// `--warn` tokens.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum Palette {
        Chart1,
        Chart2,
        Warn,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineEntry {
    /// RFC 3339, UTC.
    pub at: String,
    pub actor: Option<Cell>,
    /// Bold, before `verb`.
    pub title: Option<String>,
    pub verb: String,
    pub target: Option<EntityRef>,
    /// Plain text.
    pub summary: String,
    /// Drawn after `summary`.
    pub badges: Vec<Cell>,
    pub detail: Option<String>,
    pub icon: String,
    pub tone: Option<Tone>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timeline {
    pub entries: Vec<TimelineEntry>,
    pub cursor_before: Option<String>,
    pub live: bool,
    /// Draws a label above each day's entries.
    pub days: bool,
    /// Each entry's time shows its date ("Oct 14") instead of its time of day.
    pub date_only: bool,
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
    /// `ActionDecl` ids of `ActionPoint::QueueRow` actions.
    pub actions: Vec<String>,
    /// One per `WorkQueue::columns`, in order.
    pub cells: Vec<Cell>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkQueue {
    pub states: Vec<QueueState>,
    pub selected: String,
    pub items: Vec<QueueItem>,
    /// The header of the `QueueItem::id` column.
    pub item_label: String,
    /// The noun the foot counts items in.
    pub item_noun: Noun,
    /// Drawn between the id and the state columns.
    pub columns: Vec<Column>,
    /// `total` is the selected state's count, `None` when search or a filter narrows it.
    pub page: PageInfo,
    pub filters: Vec<FilterDef>,
    pub search_placeholder: Option<String>,
    pub selectable: bool,
}

variants! {
    drawn;
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ServiceState {
        Running,
        Degraded,
        Restarting,
        Crashed,
        Down,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceItem {
    pub name: String,
    pub state: ServiceState,
    pub version: Option<String>,
    /// The right-hand value, formatted by the provider ("33 ms", "lag 4 s").
    pub value: Option<String>,
    pub uptime: Option<String>,
    pub note: Option<String>,
    pub history: Vec<(String, ServiceState)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub items: Vec<ServiceItem>,
    /// 2 splits the list into two columns; any other value renders one.
    pub columns: u8,
    /// The header of the items' `value` column.
    pub value_label: Option<String>,
}

variants! {
    drawn;
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum Axis {
        Left,
        Right,
    }
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

variants! {
    drawn;
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum Severity {
        Err,
        Warn,
    }
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
    /// A plain-text line below the secret.
    pub extra: Option<String>,
}

impl std::fmt::Debug for SecretReveal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretReveal")
            .field("label", &self.label)
            .field("value", &"<redacted>")
            .field("hint", &self.hint)
            .field("expires_at", &self.expires_at)
            .field("extra", &self.extra)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityHeader {
    pub title: String,
    /// Replaces the entity id in the page's last breadcrumb; `title` when absent.
    pub crumb: Option<String>,
    pub subtitle: String,
    pub status: Option<Cell>,
    /// Drawn after `status`.
    pub badges: Vec<Cell>,
    pub avatar_seed: String,
}

/// A panel's answer: its primitive plus the plain-text chrome around it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PanelBody {
    pub primitive: Primitive,
    /// Muted text right of the title.
    pub meta: Option<String>,
    /// Joined to the declared `ContributionDecl::title` with ": " by the portal; omitted with its separator when absent.
    pub heading: Option<String>,
    /// Muted text in the tools row.
    pub note: Option<String>,
    /// Shown only while the panel is `ok` or `readonly`.
    pub banner: Option<Banner>,
    /// Text in the panel's foot.
    pub foot: Option<String>,
    /// A plain badge in the tools row.
    pub tag: Option<String>,
}

impl PanelBody {
    pub fn new(primitive: Primitive) -> PanelBody {
        PanelBody { primitive, meta: None, heading: None, note: None, banner: None, foot: None, tag: None }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Banner {
    pub tone: Tone,
    pub text: String,
}

variants! {
    drawn;
    /// The closed set of things a panel can return.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    pub enum Primitive {
        DataTable(DataTable),
        PropertyList(PropertyList),
        KpiTile(KpiTile),
        /// One KPI tile per entry, for a number of tiles the data decides.
        KpiTiles(Vec<KpiTile>),
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
}

#[cfg(test)]
mod tests;
