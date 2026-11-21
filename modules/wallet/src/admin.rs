//! The `wallet` control-plane provider: the player entity's Wallet tab (a tile per catalog
//! currency over the Transactions table) and the player's summary fact, the balance of the
//! headline currency. No action yet, so `writes: false`.

use std::sync::Arc;

use async_trait::async_trait;
use controlplane::{
    Align, Cell, CellKind, Column, ContributionDecl, DataTable, InvokeCall, InvokeOutcome, KpiTile, Manifest,
    PageInfo, Palette, PanelBody, PanelQuery, Point, Primitive, Provider, Role, Row, SearchHit, SearchQuery, Tone,
    DEFAULT_PANEL_DEADLINE_MS,
};
use opsapi::Error;
use walletapi::{Balance, Currency};

use crate::store::{LedgerEntry, LedgerPage, LEDGER_PAGE_SIZE};
use crate::Service;

const MODULE: &str = "wallet";
/// The entity kind every contribution attaches to; accounts declares it.
const KIND: &str = "player";
const TAB: &str = "wallet";

const TRANSACTIONS: &str = "wallet";
const TILES: &str = "wallet.tiles";
const FACT_BALANCE: &str = "fact.balance";

pub(crate) struct WalletAdmin {
    svc: Arc<Service>,
    /// The catalog code the fact headlines (`WalletModule::with_headline_currency`).
    headline: String,
}

impl WalletAdmin {
    pub(crate) fn new(svc: Arc<Service>, headline: String) -> WalletAdmin {
        WalletAdmin { svc, headline }
    }

    async fn fact(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let balances = self.svc.store.list_balances(player_id(req)?).await.map_err(crate::internal)?;
        Ok(PanelBody::new(Primitive::Cell(text(
            CellKind::Text,
            fact_text(&self.headline, &balances),
        ))))
    }

    async fn tiles(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let id = player_id(req)?;
        let (currencies, balances) =
            tokio::try_join!(self.svc.store.list_currencies(), self.svc.store.list_balances(id))
                .map_err(crate::internal)?;
        Ok(tiles_body(&currencies, &balances))
    }

    async fn transactions(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let id = player_id(req)?;
        let after = req.cursor.as_deref().map(parse_cursor).transpose()?;
        let page = self.svc.store.player_ledger(id, after).await.map_err(crate::internal)?;
        Ok(transactions_body(&page))
    }
}

/// The entity id of a player panel's request.
fn player_id(req: &PanelQuery) -> Result<&str, Error> {
    match &req.entity {
        Some(entity) if entity.kind == KIND => Ok(&entity.id),
        _ => Err(Error::invalid("wallet: this panel needs a player")),
    }
}

/// The Transactions cursor is the `seq` of the last row drawn, in decimal.
pub(crate) fn parse_cursor(raw: &str) -> Result<i64, Error> {
    let invalid = || Error::invalid("wallet: the page cursor is malformed");
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    raw.parse().map_err(|_| invalid())
}

/// An exact en-US grouped integer: "91,300", "-1,200".
pub(crate) fn grouped(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A movement's amount: grouped, "+" on a credit.
pub(crate) fn signed(delta: i64) -> String {
    if delta > 0 {
        format!("+{}", grouped(delta))
    } else {
        grouped(delta)
    }
}

/// The balance of `code`, 0 when the player holds no row for it.
fn balance_of(balances: &[Balance], code: &str) -> i64 {
    balances.iter().find(|b| b.currency == code).map_or(0, |b| b.amount)
}

/// The summary fact's wording: "N <code>" for the headline currency, "0 <code>" when the
/// player holds none of it.
pub(crate) fn fact_text(headline: &str, balances: &[Balance]) -> String {
    format!("{} {headline}", grouped(balance_of(balances, headline)))
}

/// The Wallet tab's tiles: one per catalog currency in the catalog's order, labelled
/// "<display name> (<code>)", the player's balance or "0". No delta, sparkline or link.
pub(crate) fn tiles_body(currencies: &[Currency], balances: &[Balance]) -> PanelBody {
    let tiles = currencies
        .iter()
        .map(|c| KpiTile {
            label: format!("{} ({})", c.display_name, c.code),
            value: grouped(balance_of(balances, &c.code)),
            unit: None,
            delta: None,
            series: Vec::new(),
            spark: Palette::Chart1,
            capacity: None,
            link: None,
        })
        .collect();
    PanelBody::new(Primitive::KpiTiles(tiles))
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

fn ledger_row(e: &LedgerEntry) -> Row {
    let tone = match e.delta {
        d if d < 0 => Some(Tone::Err),
        d if d > 0 => Some(Tone::Ok),
        _ => None,
    };
    Row {
        entity: None,
        cells: vec![
            text(CellKind::Mono, e.at.clone()),
            text(CellKind::Ref, e.currency.clone()),
            Cell { tone, ..text(CellKind::Mono, signed(e.delta)) },
            text(CellKind::Mono, grouped(e.balance_after)),
            text(CellKind::Dim, e.reason.clone()),
            text(CellKind::MonoMuted, e.id.clone()),
        ],
        highlight: false,
        item: None,
        actions: Vec::new(),
    }
}

/// The Transactions panel: one page of the player's movements newest first (Time, Currency,
/// Amount, Balance, Reason, Tx), no toolbar, a keyset pager whose cursor is the last drawn
/// row's `seq`. Zero rows draws the portal's empty state.
pub(crate) fn transactions_body(page: &LedgerPage) -> PanelBody {
    let table = DataTable {
        columns: vec![
            column("at", "Time", CellKind::Mono, Align::Left),
            column("currency", "Currency", CellKind::Ref, Align::Left),
            column("amount", "Amount", CellKind::Mono, Align::Right),
            column("balance", "Balance", CellKind::Mono, Align::Right),
            Column { wrap: true, ..column("reason", "Reason", CellKind::Dim, Align::Left) },
            column("tx", "Tx", CellKind::MonoMuted, Align::Left),
        ],
        rows: page.rows.iter().map(ledger_row).collect(),
        page: PageInfo {
            next_cursor: page.next_after.map(|seq| seq.to_string()),
            size: LEDGER_PAGE_SIZE as u32,
            total: None,
        },
        filters: Vec::new(),
        search_placeholder: None,
        selectable: false,
        list_columns_for: None,
        list_columns_at: 0,
        item_noun: None,
    };
    PanelBody::new(Primitive::DataTable(table))
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
        tab_label: "Wallet".to_string(),
        columns: 1,
    };
    let tiles = Point::EntityTabTile {
        kind: KIND.to_string(),
        tab: TAB.to_string(),
    };
    Manifest {
        module: MODULE.to_string(),
        sections: Vec::new(),
        entries: Vec::new(),
        pages: Vec::new(),
        entity_kinds: Vec::new(),
        contributions: vec![
            contribution(TRANSACTIONS, tab, 30, "Transactions", ""),
            contribution(TILES, tiles, 0, "Wallet", ""),
            contribution(FACT_BALANCE, Point::EntitySummary(KIND.to_string()), 50, "Wallet", "coins"),
        ],
        actions: Vec::new(),
        search: Vec::new(),
        writes: false,
    }
}

#[async_trait]
impl Provider for WalletAdmin {
    fn manifest(&self) -> Manifest {
        manifest()
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, Error> {
        match req.contribution.as_str() {
            TRANSACTIONS => self.transactions(&req).await,
            TILES => self.tiles(&req).await,
            FACT_BALANCE => self.fact(&req).await,
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
