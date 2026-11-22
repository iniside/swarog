//! The `inventory` control-plane provider: the player entity's Inventory tab (the holdings
//! the player owns directly and through its characters), the tab's count, the dashboard's
//! "Inventory purges pending" attention item, and the `holding` row kind. Inventory has no
//! admin write yet: its "Grant item" tool, the player row-menu "Grant item…" and the holding
//! row-menu "Revoke item…" are declared `implemented: false` and `writes` is false.

use async_trait::async_trait;
use controlplane::{
    ActionDecl, ActionPoint, Align, Cell, CellKind, Column, ContributionDecl, DataTable, Emphasis, EntityKindDecl,
    EntityRef, FilterDef, FilterKind, InvokeCall, InvokeOutcome, Manifest, Noun, PageInfo, PanelBody, PanelQuery,
    Point, Primitive, Provider, Role, Row, SearchHit, SearchQuery, Then, DEFAULT_PANEL_DEADLINE_MS,
};
use opsapi::Error;
use std::sync::Arc;

use crate::store::{AdminHolding, HoldingKey, HoldingsFilter, ADMIN_PAGE_SIZE};
use crate::Inner;

const MODULE: &str = "inventory";
/// The entity kind the tab, its count and the grant item attach to; accounts declares it.
const KIND: &str = "player";
/// The row kind of the tab's table, declared here; it has a row menu and no entity page.
pub(crate) const HOLDING_KIND: &str = "holding";
/// The entity kind a character owner's cell is labelled by; characters declares it.
const CHARACTER_KIND: &str = "character";
const TAB: &str = "inventory";

pub(crate) const INVENTORY: &str = "inventory";
pub(crate) const TAB_COUNT: &str = "tab.count";
pub(crate) const ATTENTION_PURGES: &str = "attention.purges";

/// The tab's Owner select: a holding's owner type.
pub(crate) const PARAM_OWNER: &str = "owner";
/// The tab's Type select: an item kind.
pub(crate) const PARAM_TYPE: &str = "type";
const OWNER_TYPES: [&str; 2] = ["player", "character"];

const ITEM_GRANT: &str = "item.grant";
const ITEM_GRANT_TOOL: &str = "item.grant-tool";
const HOLDING_REVOKE: &str = "holding.revoke";

/// What the Source and Acquired cells draw: a holding stores neither.
const NO_VALUE: &str = "—";

pub(crate) struct InventoryAdmin {
    inner: Arc<Inner>,
}

impl InventoryAdmin {
    pub(crate) fn new(inner: Arc<Inner>) -> InventoryAdmin {
        InventoryAdmin { inner }
    }

    /// The Inventory tab: one keyset page of the player's holdings under the Owner and Type
    /// selects and the search, the exact count of every holding they keep, and the selects'
    /// options.
    async fn inventory(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let id = player_id(req)?;
        let owner_type = owner_filter(req.params.get(PARAM_OWNER).map(String::as_str))?;
        let kinds = self.inner.store.item_kinds().await.map_err(crate::internal)?;
        let kind = match selected(req.params.get(PARAM_TYPE).map(String::as_str)) {
            Some(k) if !kinds.iter().any(|known| known == k) => {
                return Err(Error::invalid(format!("{MODULE}: unknown item type {k:?}")));
            }
            k => k,
        };
        let needle = req.search.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let after = req.cursor.as_deref().map(parse_cursor).transpose()?;
        let filter = HoldingsFilter { owner_type, kind, needle };
        let mut page = self
            .inner
            .store
            .player_holdings_page(id, &filter, after.as_ref(), ADMIN_PAGE_SIZE as i64 + 1)
            .await
            .map_err(read_error)?;
        let next_cursor = if page.rows.len() > ADMIN_PAGE_SIZE {
            page.rows.truncate(ADMIN_PAGE_SIZE);
            page.rows.last().map(|h| holding_id(&h.key))
        } else {
            None
        };
        Ok(inventory_body(id, &page.rows, page.total, next_cursor, kinds))
    }

    /// The tab count: every holding of the player and its characters, unfiltered.
    async fn count(&self, req: &PanelQuery) -> Result<i64, Error> {
        let id = player_id(req)?;
        let all = HoldingsFilter { owner_type: None, kind: None, needle: None };
        let page = self.inner.store.player_holdings_page(id, &all, None, 0).await.map_err(read_error)?;
        Ok(page.total)
    }

    /// The purge backlog as the dashboard's attention item.
    async fn purges(&self) -> Result<PanelBody, Error> {
        let (pending, oldest) = self.inner.store.pending_purges().await.map_err(crate::internal)?;
        Ok(purges_body(pending, &oldest))
    }
}

/// A malformed id (Postgres 22P02: the player, or the cursor's owner) is the caller's
/// error; anything else is internal.
fn read_error(e: sqlx::Error) -> Error {
    match &e {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("22P02") => {
            Error::invalid(format!("{MODULE}: malformed id"))
        }
        _ => crate::internal(e),
    }
}

/// The entity id of a player panel's request.
fn player_id(req: &PanelQuery) -> Result<&str, Error> {
    match &req.entity {
        Some(entity) if entity.kind == KIND => Ok(&entity.id),
        _ => Err(Error::invalid(format!("{MODULE}: this panel needs a player"))),
    }
}

/// A select's value; the empty first option narrows nothing.
fn selected(value: Option<&str>) -> Option<&str> {
    value.filter(|v| !v.is_empty())
}

/// The Owner select: an owner type, or nothing.
pub(crate) fn owner_filter(value: Option<&str>) -> Result<Option<&str>, Error> {
    match selected(value) {
        Some(v) if !OWNER_TYPES.contains(&v) => Err(Error::invalid(format!("{MODULE}: unknown owner type {v:?}"))),
        v => Ok(v),
    }
}

/// A holding's row id and keyset cursor: its primary key, `<owner_type>:<owner_id>:<item_id>`.
pub(crate) fn holding_id(key: &HoldingKey) -> String {
    format!("{}:{}:{}", key.owner_type, key.owner_id, key.item_id)
}

/// The cursor a page handed out, [`holding_id`] of its last row; the owner id is checked
/// by the query (22P02).
pub(crate) fn parse_cursor(raw: &str) -> Result<HoldingKey, Error> {
    let invalid = || Error::invalid(format!("{MODULE}: the page cursor is malformed"));
    let mut parts = raw.splitn(3, ':');
    let (Some(owner_type), Some(owner_id), Some(item_id)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(invalid());
    };
    if !OWNER_TYPES.contains(&owner_type) || owner_id.is_empty() || item_id.is_empty() {
        return Err(invalid());
    }
    Ok(HoldingKey { owner_type: owner_type.to_string(), owner_id: owner_id.to_string(), item_id: item_id.to_string() })
}

fn text(kind: CellKind, value: impl Into<String>) -> Cell {
    Cell {
        kind,
        value: value.into(),
        tone: None,
        link: None,
        bar: None,
        more: Vec::new(),
        row: false,
        label: None,
    }
}

fn column(key: &str, label: &str, kind: CellKind, align: Align) -> Column {
    Column {
        key: key.to_string(),
        label: label.to_string(),
        kind,
        sortable: false,
        align,
        sorted: None,
        wrap: false,
    }
}

/// The tab count: a number cell for every count, 0 included.
pub(crate) fn count_cell(n: i64) -> Cell {
    text(CellKind::Number, n.to_string())
}

/// The Owner cell: "player" for the player's own holding; a character's id drawn as its
/// label for a character's.
pub(crate) fn owner_cell(key: &HoldingKey) -> Cell {
    if key.owner_type == CHARACTER_KIND {
        Cell {
            label: Some(EntityRef { kind: CHARACTER_KIND.to_string(), id: key.owner_id.clone() }),
            ..text(CellKind::Mono, key.owner_id.clone())
        }
    } else {
        text(CellKind::Mono, key.owner_type.clone())
    }
}

fn holding_row(h: &AdminHolding) -> Row {
    Row {
        entity: Some(EntityRef { kind: HOLDING_KIND.to_string(), id: holding_id(&h.key) }),
        cells: vec![
            text(CellKind::Text, h.item_name.clone()),
            text(CellKind::Ref, h.kind.clone()),
            text(CellKind::Number, h.quantity.to_string()),
            owner_cell(&h.key),
            text(CellKind::MonoMuted, NO_VALUE),
            text(CellKind::Text, NO_VALUE),
        ],
        highlight: false,
        item: None,
        actions: Vec::new(),
    }
}

/// The Inventory tab panel: the page's rows in key order, the Owner and Type selects, the
/// search, a keyset pager over `total`, and the meta "N items · owner player:<id>".
pub(crate) fn inventory_body(
    player_id: &str,
    rows: &[AdminHolding],
    total: i64,
    next_cursor: Option<String>,
    kinds: Vec<String>,
) -> PanelBody {
    let noun = Noun::new("item", "items");
    let total = total.max(0) as u64;
    let meta = format!("{total} {} · owner {KIND}:{player_id}", noun.of(total));
    let table = DataTable {
        columns: vec![
            column("item", "Item", CellKind::Text, Align::Left),
            column("type", "Type", CellKind::Ref, Align::Left),
            column("qty", "Qty", CellKind::Number, Align::Right),
            column("owner", "Owner", CellKind::Mono, Align::Left),
            column("source", "Source", CellKind::MonoMuted, Align::Left),
            column("acquired", "Acquired", CellKind::Text, Align::Left),
        ],
        rows: rows.iter().map(holding_row).collect(),
        page: PageInfo {
            next_cursor,
            size: ADMIN_PAGE_SIZE as u32,
            total: Some(total),
        },
        filters: vec![
            FilterDef {
                key: PARAM_OWNER.to_string(),
                label: "Owner".to_string(),
                kind: FilterKind::Select(OWNER_TYPES.iter().map(|t| t.to_string()).collect()),
            },
            FilterDef {
                key: PARAM_TYPE.to_string(),
                label: "Type".to_string(),
                kind: FilterKind::Select(kinds),
            },
        ],
        search_placeholder: Some("Item id or name".to_string()),
        selectable: false,
        list_columns_for: None,
        list_columns_at: 0,
        item_noun: Some(noun),
    };
    PanelBody {
        meta: Some(meta),
        ..PanelBody::new(Primitive::DataTable(table))
    }
}

/// The "Inventory purges pending" attention item over inventory's purge sweep backlog.
pub(crate) fn purges_body(pending: i64, oldest: &str) -> PanelBody {
    controlplane::purge_backlog_body("Inventory purges pending", pending, oldest, crate::projection::PURGE_SWEEP_SUB.id)
}

fn contribution(id: &str, point: Point, order: i32, title: &str) -> ContributionDecl {
    ContributionDecl {
        id: id.to_string(),
        point,
        order,
        title: title.to_string(),
        icon: String::new(),
        min_role: Role::Viewer,
        cache_ttl_s: None,
        deadline_ms: DEFAULT_PANEL_DEADLINE_MS,
        readonly: None,
    }
}

/// An action with no implementation yet: drawn disabled, never invoked.
fn not_implemented(id: &str, point: ActionPoint, label: &str, icon: &str, order: i32, danger: bool) -> ActionDecl {
    ActionDecl {
        id: id.to_string(),
        point,
        label: label.to_string(),
        icon: icon.to_string(),
        order,
        danger,
        multi: false,
        min_role: Role::Operator,
        then: Then::Invoke,
        implemented: false,
        emphasis: Emphasis::Default,
        trailing_icon: false,
        icon_only: false,
        when_empty: false,
        prepare: false,
    }
}

pub(crate) fn manifest() -> Manifest {
    let tab = Point::EntityTab {
        kind: KIND.to_string(),
        tab: TAB.to_string(),
        tab_label: "Inventory".to_string(),
        columns: 1,
        params: vec![PARAM_OWNER.to_string(), PARAM_TYPE.to_string()],
    };
    let tab_count = Point::EntityTabCount {
        kind: KIND.to_string(),
        tab: TAB.to_string(),
    };
    Manifest {
        module: MODULE.to_string(),
        sections: Vec::new(),
        entries: Vec::new(),
        pages: Vec::new(),
        entity_kinds: vec![EntityKindDecl {
            kind: HOLDING_KIND.to_string(),
            label: "Holding".to_string(),
            icon: "box".to_string(),
        }],
        contributions: vec![
            contribution(INVENTORY, tab, 40, "Inventory"),
            contribution(TAB_COUNT, tab_count, 0, "Inventory"),
            contribution(ATTENTION_PURGES, Point::DashboardAttention, 63, "Inventory purges pending"),
        ],
        actions: vec![
            not_implemented(ITEM_GRANT, ActionPoint::RowAction(KIND.to_string()), "Grant item…", "box", 50, false),
            not_implemented(ITEM_GRANT_TOOL, ActionPoint::PanelTool(INVENTORY.to_string()), "Grant item", "gift", 0, false),
            not_implemented(
                HOLDING_REVOKE,
                ActionPoint::RowAction(HOLDING_KIND.to_string()),
                "Revoke item…",
                "trash",
                0,
                true,
            ),
        ],
        search: Vec::new(),
        writes: false,
    }
}

#[async_trait]
impl Provider for InventoryAdmin {
    fn manifest(&self) -> Manifest {
        manifest()
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, Error> {
        match req.contribution.as_str() {
            INVENTORY => self.inventory(&req).await,
            TAB_COUNT => {
                let n = self.count(&req).await?;
                Ok(PanelBody::new(Primitive::Cell(count_cell(n))))
            }
            ATTENTION_PURGES => self.purges().await,
            other => Err(Error::not_found(format!("{MODULE} has no contribution {other:?}"))),
        }
    }

    async fn search(&self, _req: SearchQuery) -> Result<Vec<SearchHit>, Error> {
        Ok(Vec::new())
    }

    async fn invoke(&self, req: InvokeCall) -> Result<InvokeOutcome, Error> {
        Err(Error::not_found(format!("{MODULE} has no action {:?}", req.action)))
    }
}
