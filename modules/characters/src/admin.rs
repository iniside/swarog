//! The `characters` control-plane provider: the player entity's Characters tab, its
//! "N characters" summary fact, the tab's count, the players list's Characters column, and
//! the `character` entity kind with its label, and the dashboard's "Character purges pending"
//! attention item. Characters is not a writer: it declares no action and `writes: false`.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use charactersapi::Character;
use controlplane::{
    Align, Cell, CellKind, Column, ContributionDecl, DataTable, EntityKindDecl, InvokeCall, InvokeOutcome,
    Manifest, PageInfo, PanelBody, PanelQuery, Point, Primitive, Provider, Role, Row, SearchHit, SearchQuery,
    DEFAULT_PANEL_DEADLINE_MS, MAX_PANEL_REFS,
};
use controlplane::order;
use opsapi::Error;

use crate::Service;

const MODULE: &str = "characters";
/// The entity kind every contribution but the label attaches to; accounts declares it.
const KIND: &str = "player";
/// The entity kind characters declares and labels.
const CHARACTER_KIND: &str = "character";
const TAB: &str = "characters";

const CHARACTERS: &str = "characters";
const FACT_COUNT: &str = "fact.count";
const TAB_COUNT: &str = "tab.count";
const COLUMN_COUNT: &str = "column.count";
const LABEL: &str = "label.character";
const ATTENTION_PURGES: &str = "attention.purges";

pub(crate) struct CharactersAdmin {
    svc: Arc<Service>,
}

impl CharactersAdmin {
    pub(crate) fn new(svc: Arc<Service>) -> CharactersAdmin {
        CharactersAdmin { svc }
    }

    /// Each ref's character count, keyed by the ref as given, 0 for a player with none.
    /// More than [`MAX_PANEL_REFS`] refs is refused before any SQL.
    pub(crate) async fn counts(&self, refs: &[String]) -> Result<BTreeMap<String, i64>, Error> {
        within_cap(refs)?;
        self.svc.store.counts_by_player(refs).await.map_err(read_error)
    }

    /// The character label for one slot's body: each ref's character name, keyed by the
    /// ref as given; a ref naming no character, or not a uuid, is left out. More than
    /// [`MAX_PANEL_REFS`] refs is refused before any SQL.
    pub(crate) async fn labels(&self, refs: &[String]) -> Result<PanelBody, Error> {
        within_cap(refs)?;
        let names = self.svc.store.names_by_id(refs).await.map_err(crate::internal)?;
        Ok(label_cells(refs, &names))
    }

    /// The requested player's character count.
    async fn count(&self, req: &PanelQuery) -> Result<i64, Error> {
        let id = player_id(req)?;
        let counts = self.counts(&[id.to_string()]).await?;
        Ok(counts.get(id).copied().unwrap_or(0))
    }

    /// The players list's Characters column for one table page: every ref in `req.refs`.
    async fn column(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let counts = self.counts(&req.refs).await?;
        Ok(column_cells(&req.refs, &counts))
    }

    /// The purge backlog as the dashboard's attention item.
    async fn purges(&self) -> Result<PanelBody, Error> {
        let (pending, oldest) = self.svc.store.pending_purges().await.map_err(crate::internal)?;
        Ok(purges_body(pending, &oldest))
    }

    async fn characters(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let id = player_id(req)?;
        let characters = self.svc.store.list_for_panel(id).await.map_err(read_error)?;
        Ok(characters_body(&characters))
    }
}

/// More than [`MAX_PANEL_REFS`] refs is the caller's error.
fn within_cap(refs: &[String]) -> Result<(), Error> {
    if refs.len() > MAX_PANEL_REFS {
        return Err(Error::invalid(format!(
            "{MODULE}: {} refs, over the cap of {MAX_PANEL_REFS}",
            refs.len()
        )));
    }
    Ok(())
}

/// A malformed player id (Postgres 22P02) is the caller's error; anything else is internal.
fn read_error(e: sqlx::Error) -> Error {
    match &e {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("22P02") => {
            Error::invalid(format!("{MODULE}: malformed player id"))
        }
        _ => crate::internal(e),
    }
}

/// The entity id of a player panel's request.
fn player_id(req: &PanelQuery) -> Result<&str, Error> {
    match &req.entity {
        Some(entity) if entity.kind == KIND => Ok(&entity.id),
        _ => Err(Error::invalid("characters: this panel needs a player")),
    }
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

/// The summary fact's wording: "1 character", otherwise "N characters".
pub(crate) fn fact_text(n: i64) -> String {
    if n == 1 {
        "1 character".to_string()
    } else {
        format!("{n} characters")
    }
}

/// The tab count: a number cell for every count, 0 included, so the count always draws.
pub(crate) fn count_cell(n: i64) -> Cell {
    text(CellKind::Number, n.to_string())
}

/// The Characters list column: one number cell per ref, keyed by the ref as the table gave
/// it, "0" for a ref the counts lack, so every row draws a count.
pub(crate) fn column_cells(refs: &[String], counts: &BTreeMap<String, i64>) -> PanelBody {
    let cells = refs
        .iter()
        .map(|r| (r.clone(), count_cell(counts.get(r).copied().unwrap_or(0))))
        .collect();
    PanelBody::new(Primitive::ColumnCells(cells))
}

/// The character label: one text cell per ref the names know, keyed by the ref as the
/// portal gave it; a ref the names lack draws no cell.
pub(crate) fn label_cells(refs: &[String], names: &BTreeMap<String, String>) -> PanelBody {
    let cells = refs
        .iter()
        .filter_map(|r| names.get(r).map(|name| (r.clone(), text(CellKind::Text, name.clone()))))
        .collect();
    PanelBody::new(Primitive::ColumnCells(cells))
}

/// The "Character purges pending" attention item over characters' purge sweep backlog.
pub(crate) fn purges_body(pending: i64, oldest: &str) -> PanelBody {
    controlplane::purge_backlog_body("Character purges pending", pending, oldest, crate::projection::PURGE_SWEEP_SUB.id)
}

fn character_row(c: &Character) -> Row {
    Row {
        entity: None,
        cells: vec![
            text(CellKind::Text, c.name.clone()),
            text(CellKind::Text, c.class.clone()),
            text(CellKind::MonoMuted, c.created_at.clone()),
            text(CellKind::MonoMuted, c.id.clone()),
        ],
        highlight: false,
        item: None,
        actions: Vec::new(),
    }
}

/// The Characters tab panel: one row per character in the given order, no toolbar and no
/// pager; `meta` is the row count. Zero rows draws the portal's empty state.
pub(crate) fn characters_body(characters: &[Character]) -> PanelBody {
    let table = DataTable {
        columns: vec![
            column("name", "Name", CellKind::Text, Align::Left),
            column("class", "Class", CellKind::Text, Align::Left),
            column("created", "Created", CellKind::MonoMuted, Align::Left),
            column("id", "Character id", CellKind::MonoMuted, Align::Left),
        ],
        rows: characters.iter().map(character_row).collect(),
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
    };
    PanelBody {
        meta: Some(characters.len().to_string()),
        ..PanelBody::new(Primitive::DataTable(table))
    }
}

fn contribution(id: &str, point: Point, order: i32, title: &str, icon: &str) -> ContributionDecl {
    ContributionDecl {
        id: id.to_string(),
        point,
        order,
        title: title.to_string(),
        icon: icon.to_string(),
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
        tab_label: "Characters".to_string(),
        columns: 1,
        params: Vec::new(),
    };
    let tab_count = Point::EntityTabCount {
        kind: KIND.to_string(),
        tab: TAB.to_string(),
    };
    let list_column = Point::EntityListColumn {
        kind: KIND.to_string(),
        label: "Characters".to_string(),
        align: Align::Right,
    };
    let label = Point::EntityLabel {
        kind: CHARACTER_KIND.to_string(),
    };
    Manifest {
        module: MODULE.to_string(),
        sections: Vec::new(),
        entries: Vec::new(),
        pages: Vec::new(),
        entity_kinds: vec![EntityKindDecl {
            kind: CHARACTER_KIND.to_string(),
            label: "Character".to_string(),
            icon: "swords".to_string(),
        }],
        contributions: vec![
            contribution(CHARACTERS, tab, order::player_tab::CHARACTERS, "Characters", ""),
            contribution(
                FACT_COUNT,
                Point::EntitySummary(KIND.to_string()),
                order::player_fact::CHARACTERS,
                "Characters",
                "swords",
            ),
            contribution(TAB_COUNT, tab_count, 0, "Characters", ""),
            contribution(COLUMN_COUNT, list_column, order::player_column::CHARACTERS, "Characters", ""),
            contribution(LABEL, label, 0, "Character", ""),
            contribution(
                ATTENTION_PURGES,
                Point::DashboardAttention,
                order::dashboard_attention::CHARACTERS_PURGES,
                "Character purges pending",
                "",
            ),
        ],
        actions: Vec::new(),
        search: Vec::new(),
        writes: false,
    }
}

#[async_trait]
impl Provider for CharactersAdmin {
    fn manifest(&self) -> Manifest {
        manifest()
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, Error> {
        match req.contribution.as_str() {
            CHARACTERS => self.characters(&req).await,
            FACT_COUNT => {
                let n = self.count(&req).await?;
                Ok(PanelBody::new(Primitive::Cell(text(CellKind::Text, fact_text(n)))))
            }
            TAB_COUNT => {
                let n = self.count(&req).await?;
                Ok(PanelBody::new(Primitive::Cell(count_cell(n))))
            }
            COLUMN_COUNT => self.column(&req).await,
            LABEL => self.labels(&req.refs).await,
            ATTENTION_PURGES => self.purges().await,
            other => Err(Error::not_found(format!(
                "{MODULE} has no contribution {other:?}"
            ))),
        }
    }

    async fn search(&self, _req: SearchQuery) -> Result<Vec<SearchHit>, Error> {
        Ok(Vec::new())
    }

    async fn invoke(&self, req: InvokeCall) -> Result<InvokeOutcome, Error> {
        Err(Error::not_found(format!("{MODULE} has no action {:?}", req.action)))
    }
}

#[cfg(test)]
#[path = "admin_tests.rs"]
mod tests;
