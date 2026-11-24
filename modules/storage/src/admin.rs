//! The `storage` control-plane provider: the player entity's Cloud saves tab (one keyset
//! page of the player's objects, read-only), the dashboard's "Cloud saves used" tile and its
//! "Storage purges pending" attention item. `writes` is false and the manifest declares no
//! action: the tab is read-only and no download carrier exists in the control plane.

use async_trait::async_trait;
use controlplane::order;
use controlplane::{
    Align, Cell, CellKind, Column, ContributionDecl, DataTable, Delta, InvokeCall, InvokeOutcome, KpiTile, Manifest,
    Noun, PageInfo, Palette, PanelBody, PanelQuery, Point, Primitive, Provider, Role, Row, SearchHit, SearchQuery,
    DEFAULT_PANEL_DEADLINE_MS,
};
use opsapi::Error;
use std::sync::Arc;
use storageapi::MAX_OBJECTS_PER_OWNER;

use crate::internal;
use crate::service::{decode_admin_cursor, is_uuid_text, Service};
use crate::store::AdminObjectRow;

const MODULE: &str = "storage";
/// The entity kind the tab and the row menu attach to; accounts declares it.
const KIND: &str = "player";
const TAB: &str = "saves";
/// Rows per page of the Cloud saves tab.
pub(crate) const PAGE_SIZE: usize = 25;
/// How long the portal keeps the tile's body: its byte sum scans and detoasts every
/// stored value ([`crate::store::Store::admin_used_bytes`]).
pub(crate) const TILE_CACHE_TTL_S: u32 = 300;

pub(crate) const SAVES: &str = "saves";
pub(crate) const TILE_USED: &str = "tile.used";
pub(crate) const ATTENTION_PURGES: &str = "attention.purges";

/// The tab's banner, verbatim from the mockup (UILayout/redesign/assets/pages-core.js:160).
pub(crate) const READONLY_TEXT: &str =
    "Read-only: storage exposes saves for inspection; edits and deletes are not remotely supported.";

pub(crate) struct StorageAdmin {
    svc: Arc<Service>,
}

impl StorageAdmin {
    pub(crate) fn new(svc: Arc<Service>) -> StorageAdmin {
        StorageAdmin { svc }
    }

    /// The Cloud saves tab: one keyset page of the player's objects, newest write first,
    /// and the meta over every object the player holds.
    async fn saves(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let id = player_id(req)?;
        let after = decode_admin_cursor(req.cursor.as_deref().unwrap_or(""))?;
        let after = after.as_ref().map(|(at, c, k)| (at.as_str(), c.as_str(), k.as_str()));
        let (count, bytes) = self.svc.store.admin_owner_totals(id).await.map_err(internal)?;
        let page = self.svc.store.admin_page(id, after, PAGE_SIZE).await.map_err(internal)?;
        Ok(saves_body(&page.rows, count, bytes, page.next_cursor))
    }

    async fn tile(&self) -> Result<PanelBody, Error> {
        let bytes = self.svc.store.admin_used_bytes().await.map_err(internal)?;
        Ok(tile_body(bytes))
    }

    /// The purge backlog as the dashboard's attention item.
    async fn purges(&self) -> Result<PanelBody, Error> {
        let (pending, oldest) = self.svc.store.pending_purges().await.map_err(internal)?;
        Ok(purges_body(pending, &oldest))
    }
}

/// The entity id of a player panel's request.
fn player_id(req: &PanelQuery) -> Result<&str, Error> {
    match &req.entity {
        Some(entity) if entity.kind == KIND && is_uuid_text(&entity.id) => Ok(&entity.id),
        Some(entity) if entity.kind == KIND => Err(Error::invalid(format!("{MODULE}: malformed id"))),
        _ => Err(Error::invalid(format!("{MODULE}: this panel needs a player"))),
    }
}

/// A byte count as a decimal size with one decimal place: `B` below 1000 (a whole number),
/// then `KB`, `MB`, `GB`, `TB`, each 1000 of the one before, the unit picked so the value
/// stays below 1000 once rounded ("999.96 KB" is "1.0 MB", never "1000.0 KB"). A negative
/// count reads as 0. The one formatter of the Size cells, the meta and the tile.
///
/// The bytes are the NORMALIZED jsonb text's (`octet_length(value::text)`), not the bytes
/// the clients sent: jsonb does not keep byte identity.
pub(crate) fn format_bytes(bytes: i64) -> (String, &'static str) {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    let bytes = u128::try_from(bytes).unwrap_or(0);
    if bytes < 1000 {
        return (bytes.to_string(), "B");
    }
    let mut scale: u128 = 1000;
    for (i, unit) in UNITS.iter().enumerate() {
        // Tenths of this unit, rounded half up.
        let tenths = (bytes * 10 + scale / 2) / scale;
        if tenths < 10_000 || i == UNITS.len() - 1 {
            return (format!("{}.{}", tenths / 10, tenths % 10), unit);
        }
        scale *= 1000;
    }
    unreachable!("the last unit always returns")
}

/// [`format_bytes`] as one text: "18.4 MB".
pub(crate) fn size_text(bytes: i64) -> String {
    let (value, unit) = format_bytes(bytes);
    format!("{value} {unit}")
}

/// A count with a comma every three digits: "10,000".
fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        out.insert(0, '-');
    }
    out
}

/// The tab's meta: "{N} of 10,000 slots · {size}", the count and the size of every object
/// the player holds against the per-owner object cap. No byte quota exists, so the cap is
/// the one truthful ceiling; "slots" counts the cap, so it stays plural at N = 1.
pub(crate) fn meta_text(count: i64, bytes: i64) -> String {
    format!("{} of {} slots · {}", thousands(count), thousands(MAX_OBJECTS_PER_OWNER), size_text(bytes))
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

/// A tab row: not clickable (no `entity`) and carrying no action. The Slot cell is
/// "collection/key"; `/` is outside both segments' alphabet, so it is unambiguous.
fn object_row(o: &AdminObjectRow) -> Row {
    let slot = format!("{}/{}", o.collection, o.key);
    Row {
        entity: None,
        cells: vec![
            text(CellKind::Mono, slot),
            text(CellKind::Bytes, size_text(o.bytes)),
            text(CellKind::Text, o.updated_ago.clone()),
            text(CellKind::Ref, format!("v{}", o.version)),
            text(CellKind::MonoMuted, o.checksum.clone()),
        ],
        highlight: false,
        item: None,
        actions: Vec::new(),
    }
}

/// The Cloud saves tab panel: the page's rows, newest write first, and no total, so the
/// portal draws a foot only when a next page exists. No column carries a sort arrow: the
/// mockup draws none.
pub(crate) fn saves_body(rows: &[AdminObjectRow], count: i64, bytes: i64, next_cursor: Option<String>) -> PanelBody {
    let table = DataTable {
        columns: vec![
            column("slot", "Slot", CellKind::Mono, Align::Left),
            column("size", "Size", CellKind::Bytes, Align::Right),
            column("updated", "Updated", CellKind::Text, Align::Left),
            column("version", "Version", CellKind::Ref, Align::Left),
            column("checksum", "Checksum", CellKind::MonoMuted, Align::Left),
        ],
        rows: rows.iter().map(object_row).collect(),
        page: PageInfo {
            next_cursor,
            size: PAGE_SIZE as u32,
            total: None,
        },
        filters: Vec::new(),
        search_placeholder: None,
        selectable: false,
        list_columns_for: None,
        list_columns_at: 0,
        item_noun: Some(Noun::new("slot", "slots")),
    };
    PanelBody {
        meta: Some(meta_text(count, bytes)),
        ..PanelBody::new(Primitive::DataTable(table))
    }
}

/// The "Cloud saves used" tile: every stored byte across every owner, the object cap as
/// its plain delta text, no series (storage keeps no history of its byte total) and no link.
pub(crate) fn tile_body(bytes: i64) -> PanelBody {
    let (value, unit) = format_bytes(bytes);
    PanelBody::new(Primitive::KpiTile(KpiTile {
        label: "Cloud saves used".to_string(),
        value,
        unit: Some(unit.to_string()),
        delta: Some(Delta {
            value: String::new(),
            period: format!("{} slots / player", thousands(MAX_OBJECTS_PER_OWNER)),
            tone: None,
        }),
        series: Vec::new(),
        spark: Palette::Chart2,
        capacity: None,
        link: None,
    }))
}

/// The "Storage purges pending" attention item over storage's purge sweep backlog.
pub(crate) fn purges_body(pending: i64, oldest: &str) -> PanelBody {
    controlplane::purge_backlog_body("Storage purges pending", pending, oldest, crate::projection::PURGE_SWEEP_SUB.id)
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

pub(crate) fn manifest() -> Manifest {
    let tab = Point::EntityTab {
        kind: KIND.to_string(),
        tab: TAB.to_string(),
        tab_label: "Cloud saves".to_string(),
        columns: 1,
        params: Vec::new(),
    };
    Manifest {
        module: MODULE.to_string(),
        sections: Vec::new(),
        entries: Vec::new(),
        pages: Vec::new(),
        entity_kinds: Vec::new(),
        contributions: vec![
            ContributionDecl {
                readonly: Some(READONLY_TEXT.to_string()),
                ..contribution(SAVES, tab, order::player_tab::STORAGE, "Cloud saves")
            },
            ContributionDecl {
                cache_ttl_s: Some(TILE_CACHE_TTL_S),
                ..contribution(TILE_USED, Point::DashboardTile, order::dashboard_tile::STORAGE, "Cloud saves used")
            },
            contribution(
                ATTENTION_PURGES,
                Point::DashboardAttention,
                order::dashboard_attention::STORAGE_PURGES,
                "Storage purges pending",
            ),
        ],
        actions: Vec::new(),
        search: Vec::new(),
        writes: false,
    }
}

#[async_trait]
impl Provider for StorageAdmin {
    fn manifest(&self) -> Manifest {
        manifest()
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, Error> {
        match req.contribution.as_str() {
            SAVES => self.saves(&req).await,
            TILE_USED => self.tile().await,
            ATTENTION_PURGES => self.purges().await,
            other => Err(Error::not_found(format!("{MODULE} has no contribution {other:?}"))),
        }
    }

    async fn search(&self, _req: SearchQuery) -> Result<Vec<SearchHit>, Error> {
        Ok(Vec::new())
    }

    /// The manifest declares no action, so the portal never invokes one; a direct call is
    /// refused all the same.
    async fn invoke(&self, req: InvokeCall) -> Result<InvokeOutcome, Error> {
        Err(Error::not_found(format!("{MODULE} has no action {:?}", req.action)))
    }
}
