//! The `accounts` control-plane provider: the Users & players page, the `player` entity
//! (header, facts, the Profile & identities tab), the player search, the active-players
//! tile, the nav counter and the operator session revocations.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use controlplane::{
    fake_hash, ActionDecl, ActionPoint, Align, Cell, CellKind, Column, ConfirmSpec, ConfirmTone,
    ContributionDecl, DataTable, Emphasis, EntityHeader, EntityKindDecl, EntityRef, FilterDef,
    FilterKind, InvokeCall, InvokeOutcome, KpiTile, Layout, Link, Manifest, NavEntry, Noun,
    PageDecl, PageInfo, Palette, PanelBody, PanelQuery, Point, PrepareCall, Primitive,
    PropertyList, Provider, ReasonReq, Role, Row, SearchDecl, SearchHit, SearchQuery, SortDir,
    Then, DEFAULT_PANEL_DEADLINE_MS, DEFAULT_SEARCH_DEADLINE_MS,
};
use controlplane::order;
use opsapi::Error;

use crate::providers::{EPIC, GOOGLE, GUEST, PASSWORD};
use crate::store::{render_handle, FamilyRow, IdentityRow, PlayerListRow, PlayerListing};
use crate::Service;

const MODULE: &str = "accounts";
const PAGE: &str = "players";
const KIND: &str = "player";
const TAB: &str = "profile";

const NAV_PLAYERS: &str = "nav.players";
const TILE_ACTIVE: &str = "tile.active";
const PLAYERS: &str = "players";
const HEADER: &str = "header";
const FACT_ID: &str = "fact.id";
const FACT_JOINED: &str = "fact.joined";
const PROFILE: &str = "profile";
const IDENTITIES: &str = "identities";
const SESSIONS: &str = "sessions";
const LABEL: &str = "label";

const SUSPEND: &str = "suspend";
const DELETE_ACCOUNT: &str = "delete-account";
const SUSPEND_HEADER: &str = "suspend-header";
pub(crate) const REVOKE_ALL: &str = "revoke-all";
pub(crate) const REVOKE_SESSION: &str = "revoke-session";

/// The players table's filter keys in `PanelQuery::params`.
const FILTER_KIND: &str = "kind";
const FILTER_STATUS: &str = "status";
const FILTER_PROVIDER: &str = "provider";

/// Every account is this kind: operators live in `admin.users`, not here.
const ACCOUNT_KIND: &str = "player";
/// Every account has this status: no suspension or ban exists.
const ACCOUNT_STATUS: &str = "active";

const PAGE_SIZE: usize = 25;
/// The most live sessions the Active sessions panel lists; its meta counts them all.
const SESSIONS_SHOWN: i64 = 50;
/// The longest table search the provider runs; a longer one is refused before any statement.
const MAX_NEEDLE_BYTES: usize = 256;

/// The provider filter's options: every provider an identity can carry.
const PROVIDERS: [&str; 4] = [GUEST, PASSWORD, EPIC, GOOGLE];

/// The length of the canonical uuid spelling, hyphens included.
pub(crate) const UUID_BYTES: usize = 36;

/// The largest cursor µs `store::players_page` compares exactly: its
/// `$7 * interval '1 microsecond'` multiplies through float8, exact up to 2^53, which is
/// mid-2255 after the epoch.
const MAX_CURSOR_MICROS: i64 = 1 << 53;

pub(crate) struct AccountsAdmin {
    svc: Arc<Service>,
}

impl AccountsAdmin {
    pub(crate) fn new(svc: Arc<Service>) -> AccountsAdmin {
        AccountsAdmin { svc }
    }
}

fn internal(e: sqlx::Error) -> Error {
    Error::internal(e.to_string())
}

fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == UUID_BYTES
        && bytes.iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// The player an action is bound to: its entity, a uuid-shaped `player` ref, checked
/// before any statement.
fn bound_player(entity: Option<&EntityRef>) -> Result<&str, Error> {
    match entity {
        Some(e) if e.kind == KIND && is_uuid(&e.id) => Ok(&e.id),
        Some(e) if e.kind == KIND => Err(Error::invalid("accounts: malformed player id")),
        _ => Err(Error::invalid("accounts: this action needs a player")),
    }
}

/// Every input value is at most [`controlplane::MAX_INPUT_VALUE_BYTES`].
fn input_within_cap(input: &BTreeMap<String, String>) -> bool {
    input.values().all(|v| v.len() <= controlplane::MAX_INPUT_VALUE_BYTES)
}

fn done(message: &str) -> InvokeOutcome {
    InvokeOutcome::Ok { message: message.to_string(), reveal: None, redirect: None }
}

/// `"<created_at µs>.<id>"` of the last row shown.
fn cursor_of(row: &PlayerListRow) -> String {
    format!("{}.{}", row.created_at_micros, row.id)
}

/// A hand-edited cursor is `Invalid` here, never a database error from the page query.
fn parse_cursor(raw: &str) -> Result<(i64, &str), Error> {
    let invalid = || Error::invalid("accounts: the page cursor is malformed");
    let (micros, id) = raw.split_once('.').ok_or_else(invalid)?;
    if micros.is_empty() || !micros.bytes().all(|b| b.is_ascii_digit()) || !is_uuid(id) {
        return Err(invalid());
    }
    let micros: i64 = micros.parse().map_err(|_| invalid())?;
    if micros > MAX_CURSOR_MICROS {
        return Err(invalid());
    }
    Ok((micros, id))
}

/// `"in 40 min"`, `"in 6 h"` or `"in 29 d"`.
fn expires_in(secs: i64) -> String {
    match secs.max(0) {
        s if s < 3_600 => format!("in {} min", s / 60),
        s if s < 86_400 => format!("in {} h", s / 3_600),
        s => format!("in {} d", s / 86_400),
    }
}

/// A provider subject shown only as its first and last four characters: a `dev` subject
/// is the player's email.
fn mask(subject: &str) -> String {
    let chars: Vec<char> = subject.chars().collect();
    if chars.len() <= 8 {
        return "…".to_string();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

fn thousands(n: i64) -> String {
    let digits = n.max(0).to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn fake_pick(salt: &str, id: &str, options: &[&'static str]) -> &'static str {
    options[(fake_hash(salt, id) % options.len() as u64) as usize]
}

// FAKE: a refresh family stores neither its client nor its address; both hash the family
// id, never the token.
fn fake_client(family_id: &str) -> &'static str {
    fake_pick(
        "client",
        family_id,
        &[
            "Windows · game client",
            "macOS · game client",
            "Steam Deck · game client",
            "Admin portal · Chrome",
        ],
    )
}

// FAKE: see `fake_client`; the address is in the documentation ranges (RFC 5737).
fn fake_ip(family_id: &str) -> String {
    let hash = fake_hash("ip", family_id);
    let net = if hash & 1 == 0 { "198.51.100" } else { "203.0.113" };
    format!("{net}.{}", (hash >> 8) % 254 + 1)
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

fn player_ref(id: &str) -> EntityRef {
    EntityRef {
        kind: KIND.to_string(),
        id: id.to_string(),
    }
}

fn players_page() -> Link {
    Link::Page {
        module: MODULE.to_string(),
        page: PAGE.to_string(),
        params: BTreeMap::new(),
    }
}

/// The identities as one cell: a tag per provider.
fn providers_cell(providers: &[String]) -> Cell {
    let mut tags = providers.iter().map(|p| text(CellKind::Ref, p.clone()));
    match tags.next() {
        Some(first) => Cell {
            more: tags.collect(),
            ..first
        },
        None => text(CellKind::Muted, "—"),
    }
}

fn last_seen(row: &PlayerListRow) -> String {
    row.last_seen_secs.map_or_else(|| "—".to_string(), controlplane::ago)
}

fn column(key: &str, label: &str, kind: CellKind) -> Column {
    Column {
        key: key.to_string(),
        label: label.to_string(),
        kind,
        sortable: false,
        align: Align::Left,
        sorted: None,
        wrap: false,
    }
}

fn enum_filter(key: &str, label: &str, options: &[&str]) -> FilterDef {
    FilterDef {
        key: key.to_string(),
        label: label.to_string(),
        kind: FilterKind::Enum(options.iter().map(|o| o.to_string()).collect()),
    }
}

/// A small table with no toolbar and no foot.
fn detail_table(columns: Vec<Column>, rows: Vec<Row>) -> DataTable {
    DataTable {
        columns,
        rows,
        page: PageInfo {
            next_cursor: None,
            size: 0,
            total: None,
        },
        filters: Vec::new(),
        search_placeholder: None,
        selectable: false,
        list_columns_for: None,
        list_columns_at: 0,
        item_noun: None,
    }
}

fn contribution(
    id: &str,
    point: Point,
    order: i32,
    title: &str,
    icon: &str,
    cache_ttl_s: Option<u32>,
) -> ContributionDecl {
    ContributionDecl {
        id: id.to_string(),
        point,
        order,
        title: title.to_string(),
        icon: icon.to_string(),
        min_role: Role::Viewer,
        cache_ttl_s,
        deadline_ms: DEFAULT_PANEL_DEADLINE_MS,
        readonly: None,
    }
}

fn tab(id: &str, order: i32, title: &str) -> ContributionDecl {
    let point = Point::EntityTab {
        kind: KIND.to_string(),
        tab: TAB.to_string(),
        tab_label: "Profile & identities".to_string(),
        columns: 2,
        params: Vec::new(),
    };
    contribution(id, point, order, title, "", None)
}

/// An action no module function backs yet: rendered disabled, never invoked.
fn not_implemented(
    id: &str,
    point: ActionPoint,
    label: &str,
    icon: &str,
    order: i32,
    danger: bool,
    emphasis: Emphasis,
) -> ActionDecl {
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
        emphasis,
        trailing_icon: false,
        icon_only: false,
        when_empty: false,
        prepare: false,
    }
}

/// `revoke-all`'s modal; `prepare` names the player and counts the live sessions.
fn revoke_all_spec(title: String, impact: Vec<(String, Cell)>) -> ConfirmSpec {
    ConfirmSpec {
        title,
        body: "The player is signed out on every device and must log in again.".to_string(),
        tone: ConfirmTone::Danger,
        impact,
        reason: ReasonReq::None,
        reason_hint: None,
        type_to_confirm: None,
        confirm_label: "Revoke all".to_string(),
    }
}

/// An action `invoke` runs; a Confirm asks `prepare` for its modal.
fn implemented(
    id: &str,
    point: ActionPoint,
    label: &str,
    danger: bool,
    emphasis: Emphasis,
    then: Then,
) -> ActionDecl {
    ActionDecl {
        prepare: matches!(then, Then::Confirm(_)),
        then,
        implemented: true,
        ..not_implemented(id, point, label, "", 10, danger, emphasis)
    }
}

fn manifest() -> Manifest {
    let row = || ActionPoint::RowAction(KIND.to_string());
    let tool = |c: &str| ActionPoint::PanelTool(c.to_string());
    let table_row = |c: &str| ActionPoint::TableRow(c.to_string());
    let (plain, ghost) = (Emphasis::Default, Emphasis::Ghost);
    Manifest {
        module: MODULE.to_string(),
        sections: Vec::new(),
        entries: vec![NavEntry {
            section: "players".to_string(),
            id: PAGE.to_string(),
            label: "Users & players".to_string(),
            icon: "users".to_string(),
            order: 10,
            page: PAGE.to_string(),
            counter: Some(NAV_PLAYERS.to_string()),
        }],
        pages: vec![PageDecl {
            id: PAGE.to_string(),
            title: "Users & players".to_string(),
            subtitle: "Staff and players share one account space; Lore access is a role on the \
                       account."
                .to_string(),
            head_actions: Vec::new(),
            tiles: Vec::new(),
            layout: Layout::Stack(vec![PLAYERS.to_string()]),
            params: [FILTER_KIND, FILTER_STATUS, FILTER_PROVIDER].map(str::to_string).to_vec(),
        }],
        entity_kinds: vec![EntityKindDecl {
            kind: KIND.to_string(),
            label: "Player".to_string(),
            icon: "user".to_string(),
        }],
        contributions: vec![
            contribution(NAV_PLAYERS, Point::NavCounter, 0, "Users & players", "users", Some(60)),
            contribution(
                TILE_ACTIVE,
                Point::DashboardTile,
                order::dashboard_tile::ACCOUNTS,
                "Active players (24 h)",
                "",
                Some(60),
            ),
            // Untitled: the mockup's players table has no panel head.
            contribution(PLAYERS, Point::Page(PAGE.to_string()), 10, "", "", None),
            contribution(HEADER, Point::EntityHeader(KIND.to_string()), 0, "Player", "user", None),
            contribution(
                FACT_ID,
                Point::EntitySummary(KIND.to_string()),
                order::player_fact::ACCOUNTS_ID,
                "User id",
                "hash",
                None,
            ),
            contribution(
                FACT_JOINED,
                Point::EntitySummary(KIND.to_string()),
                order::player_fact::ACCOUNTS_JOINED,
                "Joined",
                "clock",
                None,
            ),
            tab(PROFILE, order::player_tab::ACCOUNTS_PROFILE, "Profile"),
            tab(IDENTITIES, order::player_tab::ACCOUNTS_IDENTITIES, "Linked identities"),
            tab(SESSIONS, order::player_tab::ACCOUNTS_SESSIONS, "Active sessions"),
            contribution(LABEL, Point::EntityLabel { kind: KIND.to_string() }, 0, "Player", "", None),
        ],
        actions: vec![
            // NEW: no suspension exists.
            not_implemented(
                SUSPEND,
                row(),
                "Suspend account…",
                "ban",
                order::player_row_action::ACCOUNTS_SUSPEND,
                false,
                plain,
            ),
            // NEW: deletion is the player's own two-step flow, with no operator path.
            not_implemented(
                DELETE_ACCOUNT,
                row(),
                "Delete account…",
                "trash",
                order::player_row_action::ACCOUNTS_DELETE,
                true,
                plain,
            ),
            not_implemented(
                SUSPEND_HEADER,
                ActionPoint::HeaderAction(KIND.to_string()),
                "Suspend",
                "ban",
                order::player_header_action::ACCOUNTS_SUSPEND,
                true,
                plain,
            ),
            implemented(
                REVOKE_ALL,
                tool(SESSIONS),
                "Revoke all",
                true,
                plain,
                Then::Confirm(revoke_all_spec("Revoke all sessions?".to_string(), Vec::new())),
            ),
            implemented(REVOKE_SESSION, table_row(SESSIONS), "Revoke", false, ghost, Then::Invoke),
        ],
        search: vec![SearchDecl {
            kind: KIND.to_string(),
            label: "Users & players".to_string(),
            icon: "user".to_string(),
            deadline_ms: DEFAULT_SEARCH_DEADLINE_MS,
        }],
        writes: true,
    }
}

/// The entity id of a player panel's request.
fn player_id(req: &PanelQuery) -> Result<&str, Error> {
    match &req.entity {
        Some(entity) if entity.kind == KIND => Ok(&entity.id),
        _ => Err(Error::invalid("accounts: this panel needs a player")),
    }
}

impl AccountsAdmin {
    async fn player(&self, req: &PanelQuery) -> Result<PlayerListRow, Error> {
        let id = player_id(req)?;
        self.svc
            .store
            .player_row(id)
            .await
            .map_err(internal)?
            .ok_or_else(|| Error::not_found(format!("accounts: no player {id}")))
    }

    async fn nav_counter(&self) -> Result<Primitive, Error> {
        let players = self.svc.store.player_count().await.map_err(internal)?;
        if players == 0 {
            return Ok(Primitive::Empty {
                title: "No players yet".to_string(),
                detail: String::new(),
            });
        }
        Ok(Primitive::Cell(text(CellKind::Badge, thousands(players))))
    }

    async fn tile(&self) -> Result<Primitive, Error> {
        let active = self.svc.store.active_players_24h().await.map_err(internal)?;
        Ok(Primitive::KpiTile(KpiTile {
            label: "Active players (24 h)".to_string(),
            value: thousands(active),
            unit: None,
            delta: None,
            series: Vec::new(),
            spark: Palette::Chart1,
            capacity: None,
            link: Some(players_page()),
        }))
    }

    async fn players(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let needle = req.search.as_deref().map(str::trim).filter(|s| !s.is_empty());
        if needle.is_some_and(|n| n.len() > MAX_NEEDLE_BYTES) {
            return Err(Error::invalid(format!(
                "accounts: the search is longer than {MAX_NEEDLE_BYTES} bytes"
            )));
        }
        // The Kind and Status chips are inert: every account is a player and active.
        let provider = req.params.get(FILTER_PROVIDER).map(String::as_str);
        if let Some(p) = provider.filter(|p| !PROVIDERS.contains(p)) {
            return Err(Error::invalid(format!("accounts: unknown provider {p:?}")));
        }
        let after = req.cursor.as_deref().map(parse_cursor).transpose()?;
        let listing = PlayerListing {
            needle,
            provider,
            after,
            limit: PAGE_SIZE as i64 + 1,
        };
        let mut rows = self.svc.store.players_page(&listing).await.map_err(internal)?;
        let next_cursor = if rows.len() > PAGE_SIZE {
            rows.truncate(PAGE_SIZE);
            rows.last().map(cursor_of)
        } else {
            None
        };
        let total = if needle.is_some() || provider.is_some() {
            None
        } else {
            Some(self.svc.store.player_count().await.map_err(internal)?.max(0) as u64)
        };
        let created = Column {
            sorted: Some(SortDir::Desc),
            ..column("created", "Created", CellKind::Muted)
        };
        let table = DataTable {
            columns: vec![
                column("user", "User", CellKind::User),
                column("kind", "Kind", CellKind::Label),
                column("status", "Status", CellKind::Badge),
                column("identities", "Identities", CellKind::Ref),
                column("last_seen", "Last seen", CellKind::Time),
                created,
            ],
            rows: rows.iter().map(player_row).collect(),
            page: PageInfo {
                next_cursor,
                size: PAGE_SIZE as u32,
                total,
            },
            filters: vec![
                enum_filter(FILTER_KIND, "Kind", &[ACCOUNT_KIND]),
                enum_filter(FILTER_STATUS, "Status", &[ACCOUNT_STATUS]),
                enum_filter(FILTER_PROVIDER, "Provider", &PROVIDERS),
            ],
            search_placeholder: Some("Handle, name, email or user id".to_string()),
            selectable: true,
            list_columns_for: Some(KIND.to_string()),
            // Between Identities and Last seen.
            list_columns_at: 4,
            item_noun: Some(Noun::new("account", "accounts")),
        };
        Ok(PanelBody::new(Primitive::DataTable(table)))
    }

    async fn header(&self, req: &PanelQuery) -> Result<Primitive, Error> {
        let row = self.player(req).await?;
        let handle = render_handle(&row.display_name, &row.discriminator);
        Ok(Primitive::EntityHeader(EntityHeader {
            title: row.display_name.clone(),
            crumb: Some(handle.clone()),
            subtitle: format!("@{handle}"),
            status: Some(text(CellKind::Badge, ACCOUNT_STATUS)),
            badges: vec![text(CellKind::Label, ACCOUNT_KIND)],
            avatar_seed: handle,
        }))
    }

    async fn profile(&self, req: &PanelQuery) -> Result<Primitive, Error> {
        let row = self.player(req).await?;
        let prop = |label: &str, cell: Cell| (label.to_string(), cell);
        let id = Cell {
            more: vec![text(CellKind::Ref, format!("{KIND}:{}", row.id))],
            ..text(CellKind::Mono, row.id.clone())
        };
        Ok(Primitive::PropertyList(PropertyList {
            items: vec![
                prop("User id", id),
                prop("Handle", text(CellKind::Text, render_handle(&row.display_name, &row.discriminator))),
                prop("Display name", text(CellKind::Text, row.display_name.clone())),
                prop("Kind", text(CellKind::Label, ACCOUNT_KIND)),
                prop("Status", text(CellKind::Badge, ACCOUNT_STATUS)),
                prop("Created", text(CellKind::Text, row.created.clone())),
                prop("Last seen", text(CellKind::Text, last_seen(&row))),
            ],
            columns: 1,
            item: None,
            code: None,
        }))
    }

    async fn identities(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let id = player_id(req)?;
        let identities = self.svc.store.identity_rows(id).await.map_err(internal)?;
        let rows = identities.iter().map(identity_row).collect();
        let columns = vec![
            column("provider", "Provider", CellKind::Ref),
            column("subject", "Subject", CellKind::Mono),
            column("linked", "Linked", CellKind::Text),
            column("last_used", "Last used", CellKind::Text),
        ];
        Ok(PanelBody {
            meta: Some(identities.len().to_string()),
            ..PanelBody::new(Primitive::DataTable(detail_table(columns, rows)))
        })
    }

    async fn sessions(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let id = player_id(req)?;
        let (families, live) = self
            .svc
            .store
            .live_families(id, SESSIONS_SHOWN)
            .await
            .map_err(internal)?;
        let columns = vec![
            column("client", "Client", CellKind::Text),
            column("ip", "IP", CellKind::Text),
            column("started", "Started", CellKind::Text),
            column("expires", "Expires", CellKind::Text),
        ];
        let rows = families.iter().map(session_row).collect();
        Ok(PanelBody {
            meta: Some(live.to_string()),
            ..PanelBody::new(Primitive::DataTable(detail_table(columns, rows)))
        })
    }

    /// Revokes one login of the bound player: the family in `req.item`, its refresh
    /// tokens and access sessions, under the player row lock taken first
    /// (`Store::lock_player_row_tx`). A refresh takes that row before its token too, so
    /// one rotating inside the family either committed before and goes with it, or waits
    /// and then finds its token gone. A family holding nothing, or not this player's, is
    /// NoOp.
    async fn revoke_session(&self, req: &InvokeCall) -> Result<InvokeOutcome, Error> {
        let player = bound_player(req.entity.as_ref())?;
        let family = match req.item.as_deref() {
            Some(item) if is_uuid(item) => item,
            _ => return Err(Error::invalid("accounts: revoke-session needs a session family id")),
        };
        let store = &self.svc.store;
        let mut tx = store.pool.begin().await.map_err(internal)?;
        if !store.lock_player_row_tx(&mut tx, player).await.map_err(internal)? {
            return Err(Error::not_found(format!("accounts: no player {player}")));
        }
        if !store.family_of_player_tx(&mut tx, player, family).await.map_err(internal)? {
            tx.rollback().await.map_err(internal)?;
            return Ok(InvokeOutcome::NoOp { message: "That session is no longer active".to_string() });
        }
        store.kill_family_tx(&mut tx, family).await.map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        Ok(done("Session revoked"))
    }

    /// Revokes every login of the bound player that still grants access, in one
    /// transaction (`Store::revoke_player_families_tx`). A player with no such family and
    /// no live browser session is NoOp, the same set the modal counts.
    async fn revoke_all(&self, req: &InvokeCall) -> Result<InvokeOutcome, Error> {
        let player = bound_player(req.entity.as_ref())?;
        let store = &self.svc.store;
        let mut tx = store.pool.begin().await.map_err(internal)?;
        match store.revoke_player_families_tx(&mut tx, player).await.map_err(internal)? {
            None => Err(Error::not_found(format!("accounts: no player {player}"))),
            Some(0) => {
                tx.rollback().await.map_err(internal)?;
                Ok(InvokeOutcome::NoOp { message: "The player has no sessions".to_string() })
            }
            Some(_) => {
                tx.commit().await.map_err(internal)?;
                Ok(done("All sessions revoked"))
            }
        }
    }

    /// The Revoke all modal, naming the player and counting the families and live browser
    /// sessions Revoke all would end, the set the action deletes.
    async fn prepare_revoke_all(&self, req: &PrepareCall) -> Result<Then, Error> {
        let player = bound_player(req.entity.as_ref())?;
        let store = &self.svc.store;
        let row = store
            .player_row(player)
            .await
            .map_err(internal)?
            .ok_or_else(|| Error::not_found(format!("accounts: no player {player}")))?;
        let mut conn = store.pool.acquire().await.map_err(internal)?;
        let families = store.revocable_families_tx(&mut conn, player).await.map_err(internal)?;
        let families = families.len();
        let browsers = store.live_browser_sessions_tx(&mut conn, player).await.map_err(internal)?;
        let live = families + browsers as usize;
        let handle = render_handle(&row.display_name, &row.discriminator);
        let impact = vec![("Active sessions".to_string(), text(CellKind::Text, live.to_string()))];
        Ok(Then::Confirm(revoke_all_spec(format!("Revoke all sessions of {handle}?"), impact)))
    }

    /// Each requested player's handle, keyed by id; an id naming no player is left out.
    async fn labels(&self, req: &PanelQuery) -> Result<Primitive, Error> {
        let players = self.svc.store.players_by_id(&req.refs).await.map_err(internal)?;
        let cells = players.into_iter().map(|p| (p.player_id, text(CellKind::Text, p.handle)));
        Ok(Primitive::ColumnCells(cells.collect()))
    }
}

fn player_row(row: &PlayerListRow) -> Row {
    let user = Cell {
        link: Some(Link::Entity(player_ref(&row.id))),
        ..text(CellKind::User, render_handle(&row.display_name, &row.discriminator))
    };
    Row {
        entity: Some(player_ref(&row.id)),
        cells: vec![
            user,
            text(CellKind::Label, ACCOUNT_KIND),
            text(CellKind::Badge, ACCOUNT_STATUS),
            providers_cell(&row.providers),
            text(CellKind::Time, last_seen(row)),
            text(CellKind::Muted, row.created.clone()),
        ],
        highlight: false,
        item: None,
        actions: Vec::new(),
    }
}

fn last_used(identity: &IdentityRow) -> String {
    identity.last_used_secs.map_or_else(|| "—".to_string(), controlplane::ago)
}

fn identity_row(identity: &IdentityRow) -> Row {
    Row {
        entity: None,
        cells: vec![
            text(CellKind::Ref, identity.provider.clone()),
            text(CellKind::Mono, mask(&identity.subject)),
            text(CellKind::Text, identity.linked.clone()),
            text(CellKind::Text, last_used(identity)),
        ],
        highlight: false,
        item: None,
        actions: Vec::new(),
    }
}

fn session_row(family: &FamilyRow) -> Row {
    Row {
        entity: None,
        cells: vec![
            text(CellKind::Text, fake_client(&family.family_id)),
            text(CellKind::Text, fake_ip(&family.family_id)),
            text(CellKind::Text, family.started.clone()),
            text(CellKind::Text, expires_in(family.expires_in_secs)),
        ],
        highlight: false,
        item: Some(family.family_id.clone()),
        actions: vec![REVOKE_SESSION.to_string()],
    }
}

#[async_trait]
impl Provider for AccountsAdmin {
    fn manifest(&self) -> Manifest {
        manifest()
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, Error> {
        match req.contribution.as_str() {
            NAV_PLAYERS => self.nav_counter().await.map(PanelBody::new),
            TILE_ACTIVE => self.tile().await.map(PanelBody::new),
            PLAYERS => self.players(&req).await,
            HEADER => self.header(&req).await.map(PanelBody::new),
            FACT_ID => {
                let row = self.player(&req).await?;
                Ok(PanelBody::new(Primitive::Cell(text(CellKind::Mono, row.id))))
            }
            FACT_JOINED => {
                let row = self.player(&req).await?;
                let joined = format!("joined {}", row.created);
                Ok(PanelBody::new(Primitive::Cell(text(CellKind::Text, joined))))
            }
            PROFILE => self.profile(&req).await.map(PanelBody::new),
            IDENTITIES => self.identities(&req).await,
            SESSIONS => self.sessions(&req).await,
            LABEL => self.labels(&req).await.map(PanelBody::new),
            other => Err(Error::not_found(format!(
                "{MODULE} has no contribution {other:?}"
            ))),
        }
    }

    async fn search(&self, req: SearchQuery) -> Result<Vec<SearchHit>, Error> {
        let q = req.q.trim();
        if req.kind != KIND || q.is_empty() || q.len() > MAX_NEEDLE_BYTES {
            return Ok(Vec::new());
        }
        let listing = PlayerListing {
            needle: Some(q),
            provider: None,
            after: None,
            limit: i64::from(req.limit.min(PAGE_SIZE as u32)),
        };
        let rows = self.svc.store.players_page(&listing).await.map_err(internal)?;
        Ok(rows
            .iter()
            .map(|row| SearchHit {
                link: Link::Entity(player_ref(&row.id)),
                title: render_handle(&row.display_name, &row.discriminator),
                subtitle: format!("{ACCOUNT_KIND} · {}", row.id),
                badge: Some(ACCOUNT_STATUS.to_string()),
            })
            .collect())
    }

    async fn invoke(&self, req: InvokeCall) -> Result<InvokeOutcome, Error> {
        if !input_within_cap(&req.input) {
            return Err(Error::invalid(format!(
                "an input value is longer than {} bytes",
                controlplane::MAX_INPUT_VALUE_BYTES
            )));
        }
        match req.action.as_str() {
            REVOKE_SESSION => self.revoke_session(&req).await,
            REVOKE_ALL => self.revoke_all(&req).await,
            SUSPEND | DELETE_ACCOUNT | SUSPEND_HEADER => {
                Err(Error::forbidden("not implemented yet"))
            }
            other => Err(Error::not_found(format!("{MODULE} has no action {other:?}"))),
        }
    }

    async fn prepare(&self, req: PrepareCall) -> Result<Then, Error> {
        match req.action.as_str() {
            REVOKE_ALL => self.prepare_revoke_all(&req).await,
            other => Err(Error::not_found(format!("{MODULE} prepares no action {other:?}"))),
        }
    }
}

#[cfg(test)]
#[path = "admin_tests.rs"]
mod tests;
