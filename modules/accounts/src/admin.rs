//! The `accounts` control-plane provider: the Users & players page, the `player` entity
//! (header, facts, the Profile & identities tab), the player search, the active-players
//! tile and the nav counter.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use controlplane::{
    fake_hash, ActionDecl, ActionPoint, Align, Cell, CellKind, Column, ContributionDecl, DataTable, Delta,
    Emphasis, EntityHeader, EntityKindDecl, EntityRef, FilterDef, FilterKind, InvokeCall,
    InvokeOutcome, KpiTile, Layout, Link, Manifest, NavEntry, Noun, PageDecl, PageInfo, Palette,
    PanelBody, PanelQuery, Point, Primitive, PropertyList, Provider, Role, Row, SearchDecl,
    SearchHit, SearchQuery, SortDir, Then, Tone, DEFAULT_PANEL_DEADLINE_MS,
    DEFAULT_SEARCH_DEADLINE_MS,
};
use opsapi::Error;

use crate::providers::{DEV, EPIC, GOOGLE, GUEST};
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

const EXPORT_CSV: &str = "export-csv";
const CREATE_ACCOUNT: &str = "create-account";
const SUSPEND: &str = "suspend";
const DELETE_ACCOUNT: &str = "delete-account";
const SUSPEND_HEADER: &str = "suspend-header";
const EDIT_PROFILE: &str = "edit-profile";
const LINK_IDENTITY: &str = "link-identity";
const UNLINK_IDENTITY: &str = "unlink-identity";
const REVOKE_ALL: &str = "revoke-all";
const REVOKE_SESSION: &str = "revoke-session";

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
const PROVIDERS: [&str; 4] = [GUEST, DEV, EPIC, GOOGLE];

/// The length of the canonical uuid spelling, hyphens included.
const UUID_BYTES: usize = 36;

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

/// A filter value that narrows: absent, empty and `"any"` do not.
fn filter_value<'a>(req: &'a PanelQuery, key: &str) -> Option<&'a str> {
    req.params
        .get(key)
        .map(String::as_str)
        .filter(|v| !v.is_empty() && *v != "any")
}

/// `"now"`, `"4 min ago"`, `"3 h ago"` or `"2 d ago"`.
fn ago(secs: i64) -> String {
    match secs.max(0) {
        s if s < 60 => "now".to_string(),
        s if s < 3_600 => format!("{} min ago", s / 60),
        s if s < 86_400 => format!("{} h ago", s / 3_600),
        s => format!("{} d ago", s / 86_400),
    }
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

// FAKE: no country is stored for an account.
fn fake_country(player_id: &str) -> &'static str {
    fake_pick("country", player_id, &["JP", "PL", "SE", "ES", "IN", "DE", "US", "BR", "FR", "UA"])
}

// FAKE: no identity records when it was last used.
fn fake_last_used(player_id: &str, provider: &str) -> &'static str {
    let key = format!("{player_id}/{provider}");
    fake_pick("last-used", &key, &["4 min ago", "1 h ago", "yesterday", "3 d ago", "12 d ago"])
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
    row.last_seen_secs.map_or_else(|| "—".to_string(), ago)
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
        when_empty: false,
        prepare: false,
    }
}

fn manifest() -> Manifest {
    let page = || ActionPoint::PageHead(PAGE.to_string());
    let row = || ActionPoint::RowAction(KIND.to_string());
    let tool = |c: &str| ActionPoint::PanelTool(c.to_string());
    let table_row = |c: &str| ActionPoint::TableRow(c.to_string());
    let (plain, primary, ghost) = (Emphasis::Default, Emphasis::Primary, Emphasis::Ghost);
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
            head_actions: vec![EXPORT_CSV.to_string(), CREATE_ACCOUNT.to_string()],
            tiles: Vec::new(),
            layout: Layout::Stack(vec![PLAYERS.to_string()]),
        }],
        entity_kinds: vec![EntityKindDecl {
            kind: KIND.to_string(),
            label: "Player".to_string(),
            icon: "user".to_string(),
        }],
        contributions: vec![
            contribution(NAV_PLAYERS, Point::NavCounter, 0, "Users & players", "users", Some(60)),
            contribution(TILE_ACTIVE, Point::DashboardTile, 10, "Active players (24 h)", "", Some(60)),
            // Untitled: the mockup's players table has no panel head.
            contribution(PLAYERS, Point::Page(PAGE.to_string()), 10, "", "", None),
            contribution(HEADER, Point::EntityHeader(KIND.to_string()), 0, "Player", "user", None),
            contribution(FACT_ID, Point::EntitySummary(KIND.to_string()), 10, "User id", "hash", None),
            contribution(FACT_JOINED, Point::EntitySummary(KIND.to_string()), 20, "Joined", "clock", None),
            tab(PROFILE, 10, "Profile"),
            tab(IDENTITIES, 30, "Linked identities"),
            tab(SESSIONS, 40, "Active sessions"),
            contribution(LABEL, Point::EntityLabel { kind: KIND.to_string() }, 0, "Player", "", None),
        ],
        actions: vec![
            // NEW: no export exists.
            not_implemented(EXPORT_CSV, page(), "Export CSV", "download", 10, false, plain),
            // NEW: accounts are created only by a player's own registration.
            not_implemented(CREATE_ACCOUNT, page(), "Create account", "plus", 20, false, primary),
            // NEW: no suspension exists.
            not_implemented(SUSPEND, row(), "Suspend account…", "ban", 20, false, plain),
            // NEW: deletion is the player's own two-step flow, with no operator path.
            not_implemented(DELETE_ACCOUNT, row(), "Delete account…", "trash", 30, true, plain),
            not_implemented(
                SUSPEND_HEADER,
                ActionPoint::HeaderAction(KIND.to_string()),
                "Suspend",
                "ban",
                30,
                true,
                plain,
            ),
            // NEW: the display name is immutable.
            not_implemented(EDIT_PROFILE, tool(PROFILE), "Edit", "edit", 10, false, plain),
            // NEW: linking is the player's own flow, with no operator path.
            not_implemented(LINK_IDENTITY, tool(IDENTITIES), "Link", "plus", 10, false, plain),
            // NEW: no unlink exists.
            not_implemented(UNLINK_IDENTITY, table_row(IDENTITIES), "Unlink", "", 10, true, ghost),
            // NEW: no operator revocation exists.
            not_implemented(REVOKE_ALL, tool(SESSIONS), "Revoke all", "", 10, true, plain),
            not_implemented(REVOKE_SESSION, table_row(SESSIONS), "Revoke", "", 10, false, ghost),
        ],
        search: vec![SearchDecl {
            kind: KIND.to_string(),
            label: "Users & players".to_string(),
            icon: "user".to_string(),
            deadline_ms: DEFAULT_SEARCH_DEADLINE_MS,
        }],
        writes: false,
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
            // FAKE: no daily history of active players is kept.
            delta: Some(Delta {
                value: "▲ 6.1%".to_string(),
                period: "vs prev. day".to_string(),
                tone: Some(Tone::Ok),
            }),
            // FAKE: see the delta.
            series: vec![31.0, 30.0, 33.0, 32.0, 35.0, 34.0, 36.0, 35.0, 37.0, 38.4],
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
        let provider = filter_value(req, FILTER_PROVIDER);
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
                prop("Country", text(CellKind::Text, fake_country(&row.id))),
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
        let rows = identities.iter().map(|i| identity_row(id, i)).collect();
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

fn identity_row(player_id: &str, identity: &IdentityRow) -> Row {
    Row {
        entity: None,
        cells: vec![
            text(CellKind::Ref, identity.provider.clone()),
            text(CellKind::Mono, mask(&identity.subject)),
            text(CellKind::Text, identity.linked.clone()),
            text(CellKind::Text, fake_last_used(player_id, &identity.provider)),
        ],
        highlight: false,
        item: Some(identity.provider.clone()),
        actions: vec![UNLINK_IDENTITY.to_string()],
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
        match req.action.as_str() {
            EXPORT_CSV | CREATE_ACCOUNT | SUSPEND | DELETE_ACCOUNT | SUSPEND_HEADER
            | EDIT_PROFILE | LINK_IDENTITY | UNLINK_IDENTITY | REVOKE_ALL | REVOKE_SESSION => {
                Err(Error::forbidden("not implemented yet"))
            }
            other => Err(Error::not_found(format!("{MODULE} has no action {other:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fakes_draw_pinned_values_from_the_shared_helper() {
        assert_eq!(fake_country("p-1"), "ES");
        assert_eq!(fake_ip("fam-1"), "198.51.100.64");
    }
}
