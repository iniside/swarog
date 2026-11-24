//! The `inventory` control-plane provider: the player entity's Inventory tab (the holdings
//! the player owns directly and through its characters), the tab's count, the dashboard's
//! "Inventory purges pending" attention item, the `holding` row kind, and the operator
//! writes: "Grant item" (the tab's tool and the player row-menu "Grant item…") adds to a
//! character's stack once per form instance, and the holding row-menu "Revoke item…" deletes
//! the holding row whole. Neither emits an event.

use async_trait::async_trait;
use controlplane::{
    ActionDecl, ActionPoint, Align, Cell, CellKind, Column, ConfirmSpec, ConfirmTone, ContributionDecl, DataTable,
    Emphasis, EntityKindDecl, EntityRef, Field, FieldKind, FilterDef, FilterKind, FormSpec, InvokeCall, InvokeOutcome,
    Manifest, Noun, PageInfo, PanelBody, PanelQuery, Point, PrepareCall, Primitive, Provider, ReasonReq, Role, Row,
    SearchHit, SearchQuery, Then, DEFAULT_PANEL_DEADLINE_MS,
};
use controlplane::order;
use opsapi::Error;
use sqlx::PgConnection;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::projection::lock_character;
use crate::store::{
    is_holdings_cap_violation, validate_quantity, AdminHolding, HoldingKey, HoldingsFilter, ADMIN_PAGE_SIZE,
    MAX_HOLDING_QTY,
};
use crate::{Inner, Owner};

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

pub(crate) const ITEM_GRANT: &str = "item.grant";
pub(crate) const ITEM_GRANT_TOOL: &str = "item.grant-tool";
pub(crate) const HOLDING_REVOKE: &str = "holding.revoke";

/// The grant form's field keys.
pub(crate) const FIELD_CHARACTER: &str = "character";
pub(crate) const FIELD_ITEM: &str = "item";
pub(crate) const FIELD_QUANTITY: &str = "quantity";
const FIELD_REQUEST_ID: &str = "request_id";
/// The longest request id a grant accepts. The id is the `inventory.operator_grants` primary
/// key, so it is bounded before any statement: an unbounded one fails the btree insert.
pub(crate) const MAX_REQUEST_ID_BYTES: usize = 128;

/// The length of the canonical uuid spelling, hyphens included.
pub(crate) const UUID_BYTES: usize = 36;

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
        let kind = match req.params.get(PARAM_TYPE).map(String::as_str) {
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

    /// One operator grant to a character of the bound player, applied once per form
    /// instance: the transaction records the request id first, so a repeat finds it and is
    /// NoOp. Under the player lock the grant is refused for a purged player; under the
    /// character lock, the one the wipe takes, it is refused for a character the player
    /// does not own (any more). A refusal rolls the request id back with the rest.
    async fn grant(&self, req: &InvokeCall) -> Result<InvokeOutcome, Error> {
        let player = bound_player(req.entity.as_ref())?;
        let request_id = req
            .request_id
            .as_deref()
            .filter(|r| !r.is_empty())
            .ok_or_else(|| Error::invalid(format!("{MODULE}: a grant needs a request id")))?;
        if request_id.len() > MAX_REQUEST_ID_BYTES {
            return Err(Error::invalid(format!(
                "{MODULE}: a request id is longer than {MAX_REQUEST_ID_BYTES} bytes"
            )));
        }
        let fields = match grant_fields(&req.input) {
            Ok(fields) => fields,
            Err(errors) => return Ok(InvokeOutcome::FieldErrors(errors)),
        };
        let mut tx = self.inner.store.pool.begin().await.map_err(crate::internal)?;
        let outcome = self.grant_tx(&mut tx, player, request_id, &fields).await;
        match outcome {
            Ok(InvokeOutcome::Ok { .. }) => tx.commit().await.map_err(crate::internal)?,
            _ => {
                let _ = tx.rollback().await;
            }
        }
        outcome
    }

    async fn grant_tx(
        &self,
        conn: &mut PgConnection,
        player: &str,
        request_id: &str,
        fields: &GrantFields<'_>,
    ) -> Result<InvokeOutcome, Error> {
        let store = &self.inner.store;
        if !store.claim_operator_grant_exec(conn, request_id).await.map_err(crate::internal)? {
            return Ok(InvokeOutcome::NoOp { message: "This grant was already applied".to_string() });
        }
        store.lock_player_tx(conn, player).await.map_err(crate::internal)?;
        if store.is_player_purged_tx(conn, player).await.map_err(read_error)? {
            return Err(Error::conflict(format!("{MODULE}: the player has been deleted")));
        }
        lock_character(conn, fields.character).await.map_err(crate::internal)?;
        if !store.is_character_of_exec(conn, fields.character, player).await.map_err(read_error)? {
            return Ok(field_error(FIELD_CHARACTER, "Not a character of this player"));
        }
        if !store.item_exists_exec(conn, fields.item).await.map_err(crate::internal)? {
            return Ok(field_error(FIELD_ITEM, "No such item"));
        }
        store
            .grant_exec(conn, &Owner::character(fields.character), fields.item, fields.qty)
            .await
            .map_err(|e| {
                if is_holdings_cap_violation(&e) {
                    Error::conflict(format!("{MODULE}: the character's stack of this item is at its cap"))
                } else {
                    crate::internal(e)
                }
            })?;
        Ok(done(format!("Granted {} × {}", fields.qty, fields.item)))
    }

    /// Deletes the bound holding row whole in one transaction; a row already gone is NoOp.
    async fn revoke(&self, req: &InvokeCall) -> Result<InvokeOutcome, Error> {
        let key = bound_holding(req.entity.as_ref())?;
        let store = &self.inner.store;
        let mut tx = store.pool.begin().await.map_err(crate::internal)?;
        let removed = store.delete_holding_exec(&mut tx, &key).await.map_err(read_error)?;
        if removed == 0 {
            tx.rollback().await.map_err(crate::internal)?;
            return Ok(InvokeOutcome::NoOp { message: "That holding is already gone".to_string() });
        }
        tx.commit().await.map_err(crate::internal)?;
        Ok(done("Item revoked".to_string()))
    }

    /// The grant form over the bound player's characters and the item catalog; a player
    /// with no character has nothing to grant to.
    async fn prepare_grant(&self, req: &PrepareCall) -> Result<Then, Error> {
        let player = bound_player(req.entity.as_ref())?;
        let characters = self.inner.store.player_characters(player).await.map_err(read_error)?;
        if characters.is_empty() {
            return Err(Error::invalid(format!("{MODULE}: the player has no character to grant an item to")));
        }
        let items = self.inner.store.item_ids().await.map_err(crate::internal)?;
        Ok(Then::Form(grant_form(characters, items)))
    }

    /// The revoke Confirm naming the bound holding's item, quantity and owner.
    async fn prepare_revoke(&self, req: &PrepareCall) -> Result<Then, Error> {
        let key = bound_holding(req.entity.as_ref())?;
        match self.inner.store.holding(&key).await.map_err(read_error)? {
            Some((name, quantity)) => Ok(Then::Confirm(revoke_spec(
                format!("Revoke {name}?"),
                vec![
                    ("Item".to_string(), text(CellKind::Mono, key.item_id.clone())),
                    ("Quantity".to_string(), text(CellKind::Number, quantity.to_string())),
                    ("Owner".to_string(), owner_cell(&key)),
                ],
            ))),
            None => Err(Error::not_found(format!("{MODULE}: that holding is already gone"))),
        }
    }
}

/// The grant form's values once each passed its check.
pub(crate) struct GrantFields<'a> {
    pub(crate) character: &'a str,
    pub(crate) item: &'a str,
    pub(crate) qty: i64,
}

/// The grant form's values, or each field's error: a uuid-shaped character, a non-empty
/// item id and a quantity `validate_quantity` admits.
pub(crate) fn grant_fields(input: &BTreeMap<String, String>) -> Result<GrantFields<'_>, BTreeMap<String, String>> {
    let get = |key: &str| input.get(key).map_or("", String::as_str);
    let mut errors = BTreeMap::new();
    let character = get(FIELD_CHARACTER);
    if !is_uuid(character) {
        errors.insert(FIELD_CHARACTER.to_string(), "Pick one of the player's characters".to_string());
    }
    let item = get(FIELD_ITEM);
    if item.is_empty() {
        errors.insert(FIELD_ITEM.to_string(), "Pick an item".to_string());
    }
    let qty = get(FIELD_QUANTITY).trim().parse::<i64>().ok().and_then(|q| validate_quantity(q).ok());
    if qty.is_none() {
        errors.insert(FIELD_QUANTITY.to_string(), format!("A whole number from 1 to {MAX_HOLDING_QTY}"));
    }
    match qty {
        Some(qty) if errors.is_empty() => Ok(GrantFields { character, item, qty }),
        _ => Err(errors),
    }
}

fn field_error(key: &str, message: &str) -> InvokeOutcome {
    InvokeOutcome::FieldErrors(BTreeMap::from([(key.to_string(), message.to_string())]))
}

fn done(message: String) -> InvokeOutcome {
    InvokeOutcome::Ok { message, reveal: None, redirect: None }
}

/// Every input value is at most [`controlplane::MAX_INPUT_VALUE_BYTES`].
pub(crate) fn input_within_cap(input: &BTreeMap<String, String>) -> bool {
    input.values().all(|v| v.len() <= controlplane::MAX_INPUT_VALUE_BYTES)
}

/// The canonical uuid spelling: 36 bytes, hyphens at 8, 13, 18 and 23, hex elsewhere.
fn is_uuid(value: &str) -> bool {
    value.len() == UUID_BYTES
        && value.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// The player an action is bound to: its entity, a uuid-shaped `player` ref, checked
/// before any statement.
fn bound_player(entity: Option<&EntityRef>) -> Result<&str, Error> {
    match entity {
        Some(e) if e.kind == KIND && is_uuid(&e.id) => Ok(&e.id),
        Some(e) if e.kind == KIND => Err(Error::invalid(format!("{MODULE}: malformed player id"))),
        _ => Err(Error::invalid(format!("{MODULE}: this action needs a player"))),
    }
}

/// The holding an action is bound to: its entity, a `holding` ref spelled as [`holding_id`]
/// with a uuid-shaped owner, checked before any statement.
fn bound_holding(entity: Option<&EntityRef>) -> Result<HoldingKey, Error> {
    match entity {
        Some(e) if e.kind == HOLDING_KIND => match parse_cursor(&e.id) {
            Ok(key) if is_uuid(&key.owner_id) => Ok(key),
            _ => Err(Error::invalid(format!("{MODULE}: malformed holding id"))),
        },
        _ => Err(Error::invalid(format!("{MODULE}: this action needs a holding"))),
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

/// The Owner select: an owner type, or nothing.
pub(crate) fn owner_filter(value: Option<&str>) -> Result<Option<&str>, Error> {
    match value {
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

/// An operator action whose modal `prepare` builds for the bound element.
fn action(id: &str, point: ActionPoint, label: &str, icon: &str, order: i32, danger: bool, then: Then) -> ActionDecl {
    ActionDecl {
        id: id.to_string(),
        point,
        label: label.to_string(),
        icon: icon.to_string(),
        order,
        danger,
        multi: false,
        min_role: Role::Operator,
        then,
        implemented: true,
        emphasis: Emphasis::Default,
        trailing_icon: false,
        icon_only: false,
        when_empty: false,
        prepare: true,
    }
}

fn field(key: &str, label: &str, kind: FieldKind, required: bool, help: Option<String>) -> Field {
    Field {
        key: key.to_string(),
        label: label.to_string(),
        kind,
        required,
        help,
        default: None,
        readonly: false,
        mono: false,
        placeholder: None,
    }
}

/// The Grant item modal over the player's characters and the catalog's item ids.
pub(crate) fn grant_form(characters: Vec<String>, items: Vec<String>) -> FormSpec {
    FormSpec {
        fields: vec![
            Field { mono: true, ..field(FIELD_CHARACTER, "Character", FieldKind::Enum(characters), true, None) },
            Field { mono: true, ..field(FIELD_ITEM, "Item", FieldKind::Enum(items), true, None) },
            Field {
                default: Some("1".to_string()),
                ..field(
                    FIELD_QUANTITY,
                    "Quantity",
                    FieldKind::Int,
                    true,
                    Some(format!("1 to {MAX_HOLDING_QTY}, added to the character's stack")),
                )
            },
            Field { mono: true, ..field(FIELD_REQUEST_ID, "Request id", FieldKind::RequestId, false, None) },
        ],
        hidden: BTreeMap::new(),
        submit_label: "Grant".to_string(),
        note: None,
        revision: None,
        title: Some("Grant item".to_string()),
        modes: Vec::new(),
        reason: ReasonReq::None,
        reason_label: None,
        reason_hint: None,
    }
}

/// The Revoke item Confirm; `prepare` names the holding.
fn revoke_spec(title: String, impact: Vec<(String, Cell)>) -> ConfirmSpec {
    ConfirmSpec {
        title,
        body: "The whole stack is removed from its owner.".to_string(),
        tone: ConfirmTone::Danger,
        impact,
        reason: ReasonReq::None,
        reason_hint: None,
        type_to_confirm: None,
        confirm_label: "Revoke".to_string(),
    }
}

pub(crate) fn manifest() -> Manifest {
    let grant = || Then::Form(grant_form(Vec::new(), Vec::new()));
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
            contribution(INVENTORY, tab, order::player_tab::INVENTORY, "Inventory"),
            contribution(TAB_COUNT, tab_count, 0, "Inventory"),
            contribution(
                ATTENTION_PURGES,
                Point::DashboardAttention,
                order::dashboard_attention::INVENTORY_PURGES,
                "Inventory purges pending",
            ),
        ],
        actions: vec![
            action(
                ITEM_GRANT,
                ActionPoint::RowAction(KIND.to_string()),
                "Grant item…",
                "box",
                order::player_row_action::INVENTORY,
                false,
                grant(),
            ),
            ActionDecl {
                when_empty: true,
                ..action(
                    ITEM_GRANT_TOOL,
                    ActionPoint::PanelTool(INVENTORY.to_string()),
                    "Grant item",
                    "gift",
                    0,
                    false,
                    grant(),
                )
            },
            action(
                HOLDING_REVOKE,
                ActionPoint::RowAction(HOLDING_KIND.to_string()),
                "Revoke item…",
                "trash",
                0,
                true,
                Then::Confirm(revoke_spec("Revoke item?".to_string(), Vec::new())),
            ),
        ],
        search: Vec::new(),
        writes: true,
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
        if !input_within_cap(&req.input) {
            return Err(Error::invalid(format!(
                "an input value is longer than {} bytes",
                controlplane::MAX_INPUT_VALUE_BYTES
            )));
        }
        match req.action.as_str() {
            ITEM_GRANT | ITEM_GRANT_TOOL => self.grant(&req).await,
            HOLDING_REVOKE => self.revoke(&req).await,
            other => Err(Error::not_found(format!("{MODULE} has no action {other:?}"))),
        }
    }

    async fn prepare(&self, req: PrepareCall) -> Result<Then, Error> {
        match req.action.as_str() {
            ITEM_GRANT | ITEM_GRANT_TOOL => self.prepare_grant(&req).await,
            HOLDING_REVOKE => self.prepare_revoke(&req).await,
            other => Err(Error::not_found(format!("{MODULE} has no action {other:?}"))),
        }
    }
}
