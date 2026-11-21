//! The `wallet` control-plane provider: the player entity's Wallet tab (a tile per catalog
//! currency over the Transactions table), the player's summary fact (the balance of the
//! headline currency), and the operator's grant and revoke: the player row's "Grant
//! currency…" / "Revoke currency…" modals and the Economy page's Grant-or-revoke form. Both
//! paths are one credit or debit through [`Service`] under the key
//! [`ADMIN_KEY_PREFIX`] + the form instance's request id, so `writes: true`.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use controlplane::{
    ActionDecl, ActionPoint, Align, Cell, CellKind, Column, ContributionDecl, DataTable, Emphasis, EntityRef, Field,
    FieldKind, FormMode, FormSpec, InvokeCall, InvokeOutcome, KpiTile, Layout, Manifest, PageDecl, PageInfo, Palette,
    PanelBody, PanelQuery, Point, PrepareCall, Primitive, Provider, Role, Row, SearchHit, SearchQuery, Then, Tone,
    DEFAULT_PANEL_DEADLINE_MS,
};
use opsapi::{Error, Status};
use walletapi::{
    Balance, Currency, Movement, Wallet as _, MAX_CURRENCY_CODE_BYTES, MAX_MOVEMENT_AMOUNT, MAX_REASON_BYTES,
};

use crate::service::{MALFORMED_PLAYER_ID, OUT_OF_RANGE, UNKNOWN_CURRENCY};
use crate::store::{LedgerEntry, LedgerPage, LEDGER_PAGE_SIZE};
use crate::Service;

const MODULE: &str = "wallet";
/// The entity kind every contribution attaches to; accounts declares it.
const KIND: &str = "player";
const TAB: &str = "wallet";

const TRANSACTIONS: &str = "wallet";
const TILES: &str = "wallet.tiles";
const FACT_BALANCE: &str = "fact.balance";

const PAGE: &str = "economy";
/// The Economy page's Grant-or-revoke form.
const GRANT_PANEL: &str = "grant";

/// The player row's modals (`Then::Form`, prepared with the live catalog codes).
const CURRENCY_GRANT: &str = "currency.grant";
const CURRENCY_REVOKE: &str = "currency.revoke";
/// The modes of the Grant-or-revoke form.
const LEDGER_GRANT: &str = "ledger.grant";
const LEDGER_REVOKE: &str = "ledger.revoke";

/// The namespace of every movement key the portal mints: `admin:<request id>`. Never under
/// the reserved `starter:` (`walletapi::Movement::idempotency_key`).
pub(crate) const ADMIN_KEY_PREFIX: &str = "admin:";

const FIELD_PLAYER: &str = "player";
const FIELD_AMOUNT: &str = "amount";
const FIELD_REASON: &str = "reason";
const FIELD_REQUEST_ID: &str = "request_id";

const FORM_NOTE: &str = "Idempotent per request id.";
/// The reason is the ledger row's text and rides `wallet.changed`, which audit sinks; the
/// operator's login stays in the audit's `admin.action` only (it reaches no player inbox).
const REASON_HELP: &str = "Shown in the ledger and audit log.";

/// Which way an operator moves currency: a grant is a credit, a revoke a debit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Grant,
    Revoke,
}

impl Direction {
    fn submit_label(self) -> &'static str {
        match self {
            Direction::Grant => "Grant",
            Direction::Revoke => "Revoke",
        }
    }

    fn done_verb(self) -> &'static str {
        match self {
            Direction::Grant => "Granted",
            Direction::Revoke => "Revoked",
        }
    }
}

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

    /// The catalog's codes, the options of every Money field.
    async fn codes(&self) -> Result<Vec<String>, Error> {
        let currencies = self.svc.store.list_currencies().await.map_err(crate::internal)?;
        Ok(currencies.into_iter().map(|c| c.code).collect())
    }

    /// The one move path of both the row modals and the panel form: validate the fields,
    /// then credit or debit under `admin:<request id>`, so a repeat of one submit finds
    /// its own ledger row and a resubmit of it with a changed field is the service's 409.
    async fn move_currency(&self, dir: Direction, player: &str, req: &InvokeCall) -> Result<InvokeOutcome, Error> {
        let request_id = req
            .request_id
            .as_deref()
            .filter(|r| !r.is_empty())
            .ok_or_else(|| Error::invalid("wallet: a grant or revoke needs a request id"))?;
        let amount = parse_amount(req.input.get(FIELD_AMOUNT).map_or("", String::as_str));
        let reason = parse_reason(req.input.get(FIELD_REASON).map_or("", String::as_str));
        let (amount, currency, reason) = match (amount, reason) {
            (Ok((amount, currency)), Ok(reason)) => (amount, currency, reason),
            (amount, reason) => {
                let mut errors = BTreeMap::new();
                if let Err(e) = amount {
                    errors.insert(FIELD_AMOUNT.to_string(), e);
                }
                if let Err(e) = reason {
                    errors.insert(FIELD_REASON.to_string(), e);
                }
                return Ok(InvokeOutcome::FieldErrors(errors));
            }
        };
        let movement = Movement {
            idempotency_key: format!("{ADMIN_KEY_PREFIX}{request_id}"),
            player_id: player.to_string(),
            currency: currency.clone(),
            amount,
            reason,
        };
        let moved = match dir {
            Direction::Grant => self.svc.credit(movement).await,
            Direction::Revoke => self.svc.debit(movement).await,
        };
        match moved {
            Ok(balance) => Ok(InvokeOutcome::Ok {
                message: format!("{} {} {currency}; balance {}", dir.done_verb(), grouped(amount), grouped(balance)),
                reveal: None,
                redirect: None,
            }),
            Err(e) => match movement_field_error(&e) {
                Some((key, msg)) => Ok(InvokeOutcome::FieldErrors(BTreeMap::from([(key.to_string(), msg)]))),
                None => Err(e),
            },
        }
    }
}

/// Every input value is at most [`controlplane::MAX_INPUT_VALUE_BYTES`].
fn input_within_cap(input: &BTreeMap<String, String>) -> bool {
    input.values().all(|v| v.len() <= controlplane::MAX_INPUT_VALUE_BYTES)
}

/// The Money field's `{"amount": "<decimal>", "currency": "<code>"}` as a whole amount in
/// `1..=MAX_MOVEMENT_AMOUNT` and a code, or the field's error. Membership in the catalog
/// is the service's verdict ([`movement_field_error`]), read in the movement's own
/// transaction rather than from a catalog read here.
pub(crate) fn parse_amount(raw: &str) -> Result<(i64, String), String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Money {
        amount: String,
        currency: String,
    }
    if raw.is_empty() {
        return Err("Amount is required".to_string());
    }
    let Money { amount, currency } =
        serde_json::from_str(raw).map_err(|_| "Amount must name an amount and a currency".to_string())?;
    if currency.is_empty() {
        return Err("Amount must name a currency".to_string());
    }
    if currency.len() > MAX_CURRENCY_CODE_BYTES {
        return Err("Currency is not in the catalog".to_string());
    }
    let whole = || format!("Amount must be a whole number from 1 to {}", grouped(MAX_MOVEMENT_AMOUNT));
    if amount.is_empty() || !amount.bytes().all(|b| b.is_ascii_digit()) {
        return Err(whole());
    }
    match amount.parse::<i64>() {
        Ok(n) if (1..=MAX_MOVEMENT_AMOUNT).contains(&n) => Ok((n, currency)),
        _ => Err(whole()),
    }
}

/// The reason trimmed: the ledger row's text as the operator wrote it, nothing added.
pub(crate) fn parse_reason(raw: &str) -> Result<String, String> {
    let reason = raw.trim();
    if reason.is_empty() {
        return Err("Reason is required".to_string());
    }
    if reason.len() > MAX_REASON_BYTES {
        return Err(format!("Reason is longer than {MAX_REASON_BYTES} bytes"));
    }
    Ok(reason.to_string())
}

/// The field a service refusal belongs to, with its text; `None` passes the error through
/// (an idempotency conflict is redrawn by the portal as a locked form).
pub(crate) fn movement_field_error(e: &Error) -> Option<(&'static str, String)> {
    match (e.status, e.msg.as_str()) {
        (Status::Conflict, OUT_OF_RANGE) => {
            Some((FIELD_AMOUNT, "Amount would take the balance below zero or past its ceiling".to_string()))
        }
        (Status::Invalid, UNKNOWN_CURRENCY) => Some((FIELD_AMOUNT, "Currency is not in the catalog".to_string())),
        (Status::Invalid, MALFORMED_PLAYER_ID) => Some((FIELD_PLAYER, "Player is not a valid player id".to_string())),
        _ => None,
    }
}

fn field(key: &str, label: &str, kind: FieldKind, required: bool) -> Field {
    Field {
        key: key.to_string(),
        label: label.to_string(),
        kind,
        required,
        help: None,
        default: None,
        readonly: false,
        mono: false,
        placeholder: None,
    }
}

/// Amount, Reason and Request id: the fields both forms share.
fn movement_fields(codes: &[String]) -> Vec<Field> {
    vec![
        field(FIELD_AMOUNT, "Amount", FieldKind::Money(codes.to_vec()), true),
        Field { help: Some(REASON_HELP.to_string()), ..field(FIELD_REASON, "Reason", FieldKind::String, true) },
        Field { mono: true, ..field(FIELD_REQUEST_ID, "Request id", FieldKind::RequestId, false) },
    ]
}

/// A player row's modal: the player is the row, so no Player field and no modes.
pub(crate) fn row_form(dir: Direction, codes: &[String]) -> FormSpec {
    let title = match dir {
        Direction::Grant => "Grant currency",
        Direction::Revoke => "Revoke currency",
    };
    FormSpec {
        fields: movement_fields(codes),
        hidden: BTreeMap::new(),
        submit_label: dir.submit_label().to_string(),
        note: Some(FORM_NOTE.to_string()),
        revision: None,
        title: Some(title.to_string()),
        modes: Vec::new(),
    }
}

fn mode(dir: Direction, action: &str) -> FormMode {
    let id = match dir {
        Direction::Grant => "grant",
        Direction::Revoke => "revoke",
    };
    FormMode {
        id: id.to_string(),
        label: dir.submit_label().to_string(),
        action: action.to_string(),
        submit_label: dir.submit_label().to_string(),
    }
}

/// The Economy page's Grant-or-revoke form: a Player field first, then the shared fields;
/// its modes run `ledger.grant` and `ledger.revoke`, Grant first.
pub(crate) fn panel_form(codes: &[String]) -> FormSpec {
    let mut fields = vec![field(FIELD_PLAYER, "Player", FieldKind::Ref(KIND.to_string()), true)];
    fields.extend(movement_fields(codes));
    FormSpec {
        fields,
        hidden: BTreeMap::new(),
        submit_label: Direction::Grant.submit_label().to_string(),
        note: Some(FORM_NOTE.to_string()),
        revision: None,
        title: None,
        modes: vec![mode(Direction::Grant, LEDGER_GRANT), mode(Direction::Revoke, LEDGER_REVOKE)],
    }
}

/// The entity id of a player panel's request.
fn player_id(req: &PanelQuery) -> Result<&str, Error> {
    row_player(req.entity.as_ref())
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

/// An operator's write, at order 40 (the player row menu's wallet group).
fn action(id: &str, point: ActionPoint, label: &str, icon: &str, then: Then) -> ActionDecl {
    ActionDecl {
        id: id.to_string(),
        point,
        label: label.to_string(),
        icon: icon.to_string(),
        order: 40,
        danger: false,
        multi: false,
        min_role: Role::Operator,
        then,
        implemented: true,
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
        tab_label: "Wallet".to_string(),
        columns: 1,
    };
    let tiles = Point::EntityTabTile {
        kind: KIND.to_string(),
        tab: TAB.to_string(),
    };
    let row = || ActionPoint::RowAction(KIND.to_string());
    let form = || ActionPoint::PanelForm(GRANT_PANEL.to_string());
    Manifest {
        module: MODULE.to_string(),
        sections: Vec::new(),
        entries: Vec::new(),
        pages: vec![PageDecl {
            id: PAGE.to_string(),
            title: "Currencies & ledger".to_string(),
            subtitle: "wallet module".to_string(),
            head_actions: Vec::new(),
            tiles: Vec::new(),
            layout: Layout::Stack(vec![GRANT_PANEL.to_string()]),
            params: Vec::new(),
        }],
        entity_kinds: Vec::new(),
        contributions: vec![
            contribution(TRANSACTIONS, tab, 30, "Transactions", ""),
            contribution(TILES, tiles, 0, "Wallet", ""),
            contribution(FACT_BALANCE, Point::EntitySummary(KIND.to_string()), 50, "Wallet", "coins"),
            contribution(GRANT_PANEL, Point::Page(PAGE.to_string()), 20, "Grant or revoke", ""),
        ],
        // The row pair is `prepare: true` because a Money field's options are the live
        // catalog codes; the declared forms fix only the variant and carry none.
        actions: vec![
            ActionDecl {
                prepare: true,
                ..action(CURRENCY_GRANT, row(), "Grant currency…", "gift", Then::Form(row_form(Direction::Grant, &[])))
            },
            ActionDecl {
                prepare: true,
                ..action(
                    CURRENCY_REVOKE,
                    row(),
                    "Revoke currency…",
                    "coins",
                    Then::Form(row_form(Direction::Revoke, &[])),
                )
            },
            action(LEDGER_GRANT, form(), "Grant", "", Then::Invoke),
            action(LEDGER_REVOKE, form(), "Revoke", "", Then::Invoke),
        ],
        search: Vec::new(),
        writes: true,
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
            GRANT_PANEL => Ok(PanelBody::new(Primitive::Form(panel_form(&self.codes().await?)))),
            other => Err(Error::not_found(format!(
                "{MODULE} has no contribution {other:?}"
            ))),
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
        let (dir, from_row) = match req.action.as_str() {
            CURRENCY_GRANT => (Direction::Grant, true),
            CURRENCY_REVOKE => (Direction::Revoke, true),
            LEDGER_GRANT => (Direction::Grant, false),
            LEDGER_REVOKE => (Direction::Revoke, false),
            other => return Err(Error::not_found(format!("{MODULE} has no action {other:?}"))),
        };
        if from_row {
            let player = row_player(req.entity.as_ref())?.to_string();
            return self.move_currency(dir, &player, &req).await;
        }
        match req.input.get(FIELD_PLAYER).map(|p| p.trim()).filter(|p| !p.is_empty()) {
            Some(player) => self.move_currency(dir, player, &req).await,
            None => Ok(InvokeOutcome::FieldErrors(BTreeMap::from([(
                FIELD_PLAYER.to_string(),
                "Player is required".to_string(),
            )]))),
        }
    }

    async fn prepare(&self, req: PrepareCall) -> Result<Then, Error> {
        let dir = match req.action.as_str() {
            CURRENCY_GRANT => Direction::Grant,
            CURRENCY_REVOKE => Direction::Revoke,
            other => return Err(Error::not_found(format!("{MODULE} prepares no action {other:?}"))),
        };
        row_player(req.entity.as_ref())?;
        Ok(Then::Form(row_form(dir, &self.codes().await?)))
    }
}

/// The player a row action is bound to.
fn row_player(entity: Option<&EntityRef>) -> Result<&str, Error> {
    match entity {
        Some(entity) if entity.kind == KIND => Ok(&entity.id),
        _ => Err(Error::invalid("wallet: this needs a player")),
    }
}

#[cfg(test)]
#[path = "admin_tests.rs"]
mod tests;
