//! The `wallet` control-plane provider: the player entity's Wallet tab (a tile per catalog
//! currency over the Transactions table), the player's summary fact (the balance of the
//! headline currency), and the operator's grant and revoke: the player row's "Grant
//! currency…" / "Revoke currency…" modals and the Economy page's Grant-or-revoke form. The
//! Economy page also lists the catalog with each currency's supply and holders (Currencies)
//! and every movement (Ledger), reached from its entry in the fixed Economy nav section. Both grant and revoke
//! paths are one credit or debit through [`Service`] under the key
//! [`ADMIN_KEY_PREFIX`] + the form instance's request id, so `writes: true`. An empty
//! catalog draws the Currencies panel's "New currency" tool, one catalog insert that refuses
//! a code the catalog already holds. The headline currency also names the players list's
//! balance column and the dashboard's 24 h spent tile; the dashboard's attention list carries
//! the purge backlog.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use controlplane::{
    ActionDecl, ActionPoint, Align, Cell, CellKind, Column, ContributionDecl, DataTable, Emphasis, EntityRef,
    Field, FieldKind, FieldValue, FilterDef, FilterKind, FormMode, FormSpec, InvokeCall, InvokeOutcome, KpiTile, Layout, Link,
    Manifest, NavEntry, Noun, PageDecl, PageInfo, Palette, PanelBody, PanelQuery, Point, PrepareCall, Primitive, Provider,
    Role, Row, SearchHit, SearchQuery, Then, Tone, DEFAULT_PANEL_DEADLINE_MS, MAX_PANEL_REFS,
};
use controlplane::order;
use opsapi::{Error, Status};
use walletapi::{
    Balance, Currency, Movement, Wallet as _, MAX_CURRENCY_CODE_BYTES, MAX_MOVEMENT_AMOUNT, MAX_REASON_BYTES,
};

use crate::service::{
    CURRENCY_EXISTS, MALFORMED_PLAYER_ID, MAX_CURRENCY_DECIMALS, MAX_CURRENCY_DISPLAY_NAME_BYTES, MAX_CURRENCY_KIND_BYTES,
    OUT_OF_RANGE, PLAYER_DELETED, UNKNOWN_CURRENCY,
};
use crate::store::{CurrencyStats, LedgerEntry, LedgerFilter, LedgerPage, LEDGER_PAGE_SIZE};
use crate::Service;

const MODULE: &str = "wallet";
/// The entity kind every contribution attaches to; accounts declares it.
const KIND: &str = "player";
const TAB: &str = "wallet";

const TRANSACTIONS: &str = "wallet";
const TILES: &str = "wallet.tiles";
const FACT_BALANCE: &str = "fact.balance";
/// The players list's headline-currency column.
const COLUMN_BALANCE: &str = "column.balance";
/// The dashboard's headline-currency-spent tile.
const TILE_SPENT: &str = "tile.spent";
const ATTENTION_PURGES: &str = "attention.purges";

const PAGE: &str = "economy";
/// The fixed `controlplane::SECTIONS` entry the Economy page's nav entry sits in.
const SECTION: &str = "economy";
/// The Economy page's catalog table.
const CURRENCIES_PANEL: &str = "currencies";
/// The Economy page's Grant-or-revoke form.
const GRANT_PANEL: &str = "grant";
/// The Economy page's table of every movement.
const LEDGER_PANEL: &str = "ledger";
/// The Ledger's toolbar selects, page URL keys that reach the panel as `PanelQuery::params`.
const PARAM_CURRENCY: &str = "currency";
const PARAM_REASON: &str = "reason";
/// The Reason select's option for an operator's grant or revoke, whose reason is free text:
/// it matches the movement's idempotency key prefix [`ADMIN_KEY_PREFIX`].
pub(crate) const REASON_ADMIN: &str = "admin";

/// The player row's modals (`Then::Form`, prepared with the live catalog codes).
const CURRENCY_GRANT: &str = "currency.grant";
const CURRENCY_REVOKE: &str = "currency.revoke";
/// The modes of the Grant-or-revoke form.
pub(crate) const LEDGER_GRANT: &str = "ledger.grant";
const LEDGER_REVOKE: &str = "ledger.revoke";
/// The Currencies panel's tool (`when_empty`: the portal draws it in every state, the empty catalog included).
pub(crate) const CURRENCY_CREATE: &str = "currency.create";

/// The namespace of every movement key the portal mints: `admin:<request id>`. Never under
/// the reserved `starter:` (`walletapi::Movement::idempotency_key`).
pub(crate) const ADMIN_KEY_PREFIX: &str = "admin:";

pub(crate) const FIELD_PLAYER: &str = "player";
pub(crate) const FIELD_AMOUNT: &str = "amount";
pub(crate) const FIELD_REASON: &str = "reason";
const FIELD_REQUEST_ID: &str = "request_id";
pub(crate) const FIELD_CODE: &str = "code";
pub(crate) const FIELD_NAME: &str = "name";
pub(crate) const FIELD_KIND: &str = "kind";
pub(crate) const FIELD_DECIMALS: &str = "decimals";

/// The Code field's refusal of a code the catalog already holds.
pub(crate) const CODE_TAKEN: &str = "A currency with this code already exists";
const KIND_HELP: &str = "soft or hard";

/// A grant or revoke naming a uuid no player holds: no ghost balance from a typo.
pub(crate) const NO_SUCH_PLAYER: &str = "No such player.";

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

    /// The players list's balance column for one table page: every ref in `req.refs`, read
    /// in one statement. More than [`MAX_PANEL_REFS`] refs, or a ref that is not a uuid, is
    /// refused before any SQL: the join need not cast a ref when no balance row matches, so
    /// the database alone would draw a malformed ref as 0 on some plans.
    async fn column(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        if req.refs.len() > MAX_PANEL_REFS {
            return Err(Error::invalid(format!(
                "{MODULE}: {} refs, over the cap of {MAX_PANEL_REFS}",
                req.refs.len()
            )));
        }
        if !req.refs.iter().all(|r| crate::service::is_uuid_text(r)) {
            return Err(Error::invalid(MALFORMED_PLAYER_ID));
        }
        let balances =
            self.svc.store.balances_by_player(&req.refs, &self.headline).await.map_err(crate::internal)?;
        Ok(balance_cells(&req.refs, &balances))
    }

    async fn spent(&self) -> Result<PanelBody, Error> {
        let digits = self.svc.store.spent_last_day(&self.headline).await.map_err(crate::internal)?;
        Ok(spent_body(&self.headline, &digits))
    }

    /// The purge backlog as the dashboard's attention item.
    async fn purges(&self) -> Result<PanelBody, Error> {
        let (pending, oldest) = self.svc.store.pending_purges().await.map_err(crate::internal)?;
        Ok(purges_body(pending, &oldest))
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
        let filter = LedgerFilter { player: Some(id), ..LedgerFilter::default() };
        let page = self.svc.store.ledger(&filter, after).await.map_err(crate::internal)?;
        Ok(transactions_body(&page))
    }

    /// The Economy Ledger: every player's movements, narrowed by the search (a player or tx
    /// id) and the Currency and Reason selects, counted for the foot.
    async fn ledger(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let after = req.cursor.as_deref().map(parse_cursor).transpose()?;
        let param = |key: &str| req.params.get(key).map(String::as_str);
        let (reason, key_prefix) = reason_filter(param(PARAM_REASON))?;
        let filter = LedgerFilter {
            player: None,
            player_or_tx: req.search.as_deref().map(str::trim).filter(|s| !s.is_empty()),
            currency: param(PARAM_CURRENCY),
            reason,
            key_prefix,
        };
        let store = &self.svc.store;
        let (page, total, currencies) =
            tokio::try_join!(store.ledger(&filter, after), store.ledger_count(&filter), store.list_currencies())
                .map_err(crate::internal)?;
        let codes: Vec<String> = currencies.into_iter().map(|c| c.code).collect();
        Ok(ledger_body(&page, total, &codes))
    }

    /// The catalog's codes, the options of every Money field.
    async fn codes(&self) -> Result<Vec<String>, Error> {
        let currencies = self.svc.store.list_currencies().await.map_err(crate::internal)?;
        Ok(currencies.into_iter().map(|c| c.code).collect())
    }

    /// The one move path of both the row modals and the panel form: validate the fields,
    /// check with the accounts directory that the player exists, then credit or debit under `admin:<request id>`, so a repeat of one submit finds
    /// its own ledger row and a resubmit of it with a changed field is the service's 409.
    /// `from_row` is a row action, whose modal has no Player field: a refusal of its player
    /// is `NotFound` rather than a field error.
    async fn move_currency(
        &self,
        dir: Direction,
        player: &str,
        from_row: bool,
        req: &InvokeCall,
    ) -> Result<InvokeOutcome, Error> {
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
        // Asked BEFORE the write, so a uuid naming no player writes nothing. A directory
        // outage is the service's `Unavailable`, passed through, never "no such player".
        match self.svc.player_exists(player).await {
            Ok(true) => {}
            Ok(false) => return player_refusal(from_row, NO_SUCH_PLAYER.to_string()),
            Err(e) => return refusal(e, from_row),
        }
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
            Err(e) => refusal(e, from_row),
        }
    }

    /// The Currencies panel's create: the four fields checked, then one catalog insert. A code
    /// the catalog already holds is the Code field's error, never an overwrite or a skip.
    async fn create_currency(&self, req: &InvokeCall) -> Result<InvokeOutcome, Error> {
        let new = match parse_new_currency(&req.input) {
            Ok(new) => new,
            Err(errors) => return Ok(InvokeOutcome::FieldErrors(errors)),
        };
        match self.svc.create_currency(&new.code, &new.display_name, &new.kind, new.decimals).await {
            Ok(()) => Ok(InvokeOutcome::Ok {
                message: format!("Created currency {}", new.code),
                reveal: None,
                redirect: None,
            }),
            Err(e) if e.status == Status::Conflict && e.msg == CURRENCY_EXISTS => {
                Ok(InvokeOutcome::FieldErrors(BTreeMap::from([(FIELD_CODE.to_string(), CODE_TAKEN.to_string())])))
            }
            Err(e) => Err(e),
        }
    }
}

/// A service refusal as the operator sees it: its field's error, a player refusal on a row
/// action as `NotFound`, and anything else passed through.
fn refusal(e: Error, from_row: bool) -> Result<InvokeOutcome, Error> {
    match movement_field_error(&e) {
        Some((FIELD_PLAYER, msg)) => player_refusal(from_row, msg),
        Some((key, msg)) => Ok(InvokeOutcome::FieldErrors(BTreeMap::from([(key.to_string(), msg)]))),
        None => Err(e),
    }
}

fn player_refusal(from_row: bool, msg: String) -> Result<InvokeOutcome, Error> {
    if from_row {
        Err(Error::not_found(msg))
    } else {
        Ok(InvokeOutcome::FieldErrors(BTreeMap::from([(FIELD_PLAYER.to_string(), msg)])))
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
    if raw.is_empty() {
        return Err("Amount is required".to_string());
    }
    let (amount, currency) = controlplane::decode_money(raw)
        .map_err(|controlplane::MoneyShapeError| "Amount must name an amount and a currency".to_string())?;
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
        // The tombstone landed between the directory check and the write: the player's own
        // refusal, not the portal's "changed since you opened it" banner a 409 draws.
        (Status::Conflict, PLAYER_DELETED) => Some((FIELD_PLAYER, "Player has been deleted.".to_string())),
        _ => None,
    }
}

/// A catalog row as the create form names it, every field within its cap.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NewCurrency {
    pub(crate) code: String,
    pub(crate) display_name: String,
    pub(crate) kind: String,
    pub(crate) decimals: i32,
}

/// The create form's four fields, or each field's error: Code, Name and Kind trimmed and
/// non-empty within their byte caps (the `currencies_*` CHECKs mirror them), Decimals a whole
/// number from 0 to [`MAX_CURRENCY_DECIMALS`].
pub(crate) fn parse_new_currency(input: &BTreeMap<String, String>) -> Result<NewCurrency, BTreeMap<String, String>> {
    let raw = |key: &str| input.get(key).map_or("", String::as_str);
    let code = bounded_text(raw(FIELD_CODE), "Code", MAX_CURRENCY_CODE_BYTES);
    let display_name = bounded_text(raw(FIELD_NAME), "Name", MAX_CURRENCY_DISPLAY_NAME_BYTES);
    let kind = bounded_text(raw(FIELD_KIND), "Kind", MAX_CURRENCY_KIND_BYTES);
    let decimals = parse_decimals(raw(FIELD_DECIMALS));
    match (code, display_name, kind, decimals) {
        (Ok(code), Ok(display_name), Ok(kind), Ok(decimals)) => Ok(NewCurrency { code, display_name, kind, decimals }),
        (code, display_name, kind, decimals) => {
            let mut errors = BTreeMap::new();
            for (key, result) in [(FIELD_CODE, code.err()), (FIELD_NAME, display_name.err()), (FIELD_KIND, kind.err())] {
                if let Some(e) = result {
                    errors.insert(key.to_string(), e);
                }
            }
            if let Err(e) = decimals {
                errors.insert(FIELD_DECIMALS.to_string(), e);
            }
            Err(errors)
        }
    }
}

/// `raw` trimmed, or the field's error when that is empty or longer than `cap` bytes.
fn bounded_text(raw: &str, label: &str, cap: usize) -> Result<String, String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(format!("{label} is required"));
    }
    if value.len() > cap {
        return Err(format!("{label} is longer than {cap} bytes"));
    }
    Ok(value.to_string())
}

/// The Int field's raw string, decoded by the form codec, as a decimals count.
fn parse_decimals(raw: &str) -> Result<i32, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("Decimals is required".to_string());
    }
    let range = || format!("Decimals must be a whole number from 0 to {MAX_CURRENCY_DECIMALS}");
    match controlplane::decode_field(&FieldKind::Int, raw) {
        Ok(FieldValue::Int(n)) => {
            i32::try_from(n).ok().filter(|d| (0..=MAX_CURRENCY_DECIMALS).contains(d)).ok_or_else(range)
        }
        _ => Err(range()),
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
        reason: controlplane::ReasonReq::None,
        reason_label: None,
        reason_hint: None,
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
        reason: controlplane::ReasonReq::None,
        reason_label: None,
        reason_hint: None,
    }
}

/// The Currencies panel's "New currency" modal: Code, Name, Kind and Decimals.
pub(crate) fn create_form() -> FormSpec {
    FormSpec {
        fields: vec![
            Field { mono: true, ..field(FIELD_CODE, "Code", FieldKind::String, true) },
            field(FIELD_NAME, "Name", FieldKind::String, true),
            Field { help: Some(KIND_HELP.to_string()), ..field(FIELD_KIND, "Kind", FieldKind::String, true) },
            field(FIELD_DECIMALS, "Decimals", FieldKind::Int, true),
        ],
        hidden: BTreeMap::new(),
        submit_label: "Create".to_string(),
        note: None,
        revision: None,
        title: Some("New currency".to_string()),
        modes: Vec::new(),
        reason: controlplane::ReasonReq::None,
        reason_label: None,
        reason_hint: None,
    }
}

/// The Reason select's options: the reason the starter grant mints, and [`REASON_ADMIN`].
pub(crate) fn reason_options() -> Vec<String> {
    vec![crate::projection::REASON.to_string(), REASON_ADMIN.to_string()]
}

/// The Reason select's value as `(reason, idempotency key prefix)`: a system reason matches
/// the reason, [`REASON_ADMIN`] the key prefix; no value narrows nothing.
pub(crate) fn reason_filter(value: Option<&str>) -> Result<(Option<&str>, Option<&str>), Error> {
    match value {
        None => Ok((None, None)),
        Some(REASON_ADMIN) => Ok((None, Some(ADMIN_KEY_PREFIX))),
        Some(r) if r == crate::projection::REASON => Ok((Some(crate::projection::REASON), None)),
        Some(other) => Err(Error::invalid(format!("wallet: no ledger reason {other:?}"))),
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
    let digits = group_digits(&n.unsigned_abs().to_string());
    if n < 0 {
        format!("-{digits}")
    } else {
        digits
    }
}

/// A run of decimal digits with a comma before every third from the right: "91300" is
/// "91,300". The digits of a sum too large for `i64` group the same way.
pub(crate) fn group_digits(digits: &str) -> String {
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
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

/// The currency kind drawn in the accent tone: the premium, paid-for kind.
const HARD_KIND: &str = "hard";
/// The Cap column's value: wallet holds no per-currency cap.
const NO_CAP: &str = "—";

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

/// The headline currency's display label in a static manifest: its code with the first
/// letter upper-cased ("gems" is "Gems"). The catalog's display name is operator data a
/// manifest cannot read.
pub(crate) fn headline_label(code: &str) -> String {
    let mut chars = code.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The dashboard tile's title and label: "Gems spent (24 h)".
fn spent_title(headline: &str) -> String {
    format!("{} spent (24 h)", headline_label(headline))
}

/// The balance list column: one number cell per ref, keyed by the ref as the table gave it,
/// "0" for a ref holding none of the headline currency, so every row draws a balance.
pub(crate) fn balance_cells(refs: &[String], balances: &BTreeMap<String, i64>) -> PanelBody {
    let cells = refs
        .iter()
        .map(|r| (r.clone(), text(CellKind::Number, grouped(balances.get(r).copied().unwrap_or(0)))))
        .collect();
    PanelBody::new(Primitive::ColumnCells(cells))
}

/// The dashboard's spent tile: the grouped 24 h total. No delta, sparkline or link (the
/// mockup designs none).
pub(crate) fn spent_body(headline: &str, digits: &str) -> PanelBody {
    PanelBody::new(Primitive::KpiTile(KpiTile {
        label: spent_title(headline),
        value: group_digits(digits),
        unit: None,
        delta: None,
        series: Vec::new(),
        spark: Palette::Chart1,
        capacity: None,
        link: None,
    }))
}

/// The "Wallet purges pending" attention item over wallet's purge sweep backlog.
pub(crate) fn purges_body(pending: i64, oldest: &str) -> PanelBody {
    controlplane::purge_backlog_body("Wallet purges pending", pending, oldest, crate::projection::PURGE_SWEEP_SUB.id)
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

/// A movement's Time, Currency, Amount, Balance, Reason and Tx cells.
fn movement_cells(e: &LedgerEntry) -> Vec<Cell> {
    let tone = match e.delta {
        d if d < 0 => Some(Tone::Err),
        d if d > 0 => Some(Tone::Ok),
        _ => None,
    };
    vec![
        text(CellKind::Mono, e.at.clone()),
        text(CellKind::Ref, e.currency.clone()),
        Cell { tone, ..text(CellKind::Mono, signed(e.delta)) },
        text(CellKind::Mono, grouped(e.balance_after)),
        text(CellKind::Dim, e.reason.clone()),
        text(CellKind::MonoMuted, e.id.clone()),
    ]
}

fn row(cells: Vec<Cell>) -> Row {
    Row { entity: None, cells, highlight: false, item: None, actions: Vec::new() }
}

/// The movement columns of [`movement_cells`], in its order.
fn movement_columns() -> Vec<Column> {
    vec![
        column("at", "Time", CellKind::Mono, Align::Left),
        column("currency", "Currency", CellKind::Ref, Align::Left),
        column("amount", "Amount", CellKind::Mono, Align::Right),
        column("balance", "Balance", CellKind::Mono, Align::Right),
        Column { wrap: true, ..column("reason", "Reason", CellKind::Dim, Align::Left) },
        column("tx", "Tx", CellKind::MonoMuted, Align::Left),
    ]
}

/// The Transactions panel: one page of the player's movements newest first (Time, Currency,
/// Amount, Balance, Reason, Tx), no toolbar, a keyset pager whose cursor is the last drawn
/// row's `seq`. Zero rows draws the portal's empty state.
pub(crate) fn transactions_body(page: &LedgerPage) -> PanelBody {
    let table = DataTable {
        columns: movement_columns(),
        rows: page.rows.iter().map(|e| row(movement_cells(e))).collect(),
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

/// The Ledger panel: one page of every player's movements newest first (Time, Player,
/// Currency, Amount, Balance, Reason, Tx), a keyset pager on `seq` as on Transactions, the
/// search box and the Currency (`codes`) and Reason selects, and a foot counting `total`
/// transactions. The Player cell links the player and draws accounts' label for the id.
pub(crate) fn ledger_body(page: &LedgerPage, total: i64, codes: &[String]) -> PanelBody {
    let mut columns = movement_columns();
    columns.insert(1, column("player", "Player", CellKind::User, Align::Left));
    let rows = page
        .rows
        .iter()
        .map(|e| {
            let player = EntityRef { kind: KIND.to_string(), id: e.player_id.clone() };
            let mut cells = movement_cells(e);
            cells.insert(1, Cell { link: Some(Link::Entity(player)), ..text(CellKind::User, e.player_id.clone()) });
            row(cells)
        })
        .collect();
    let table = DataTable {
        columns,
        rows,
        page: PageInfo {
            next_cursor: page.next_after.map(|seq| seq.to_string()),
            size: LEDGER_PAGE_SIZE as u32,
            total: Some(total.max(0) as u64),
        },
        filters: vec![
            FilterDef {
                key: PARAM_CURRENCY.to_string(),
                label: "Currency".to_string(),
                kind: FilterKind::Select(codes.to_vec()),
            },
            FilterDef {
                key: PARAM_REASON.to_string(),
                label: "Reason".to_string(),
                kind: FilterKind::Select(reason_options()),
            },
        ],
        search_placeholder: Some("Player or tx id".to_string()),
        selectable: false,
        list_columns_for: None,
        list_columns_at: 0,
        item_noun: Some(Noun::new("transaction", "transactions")),
    };
    PanelBody::new(Primitive::DataTable(table))
}

/// The Currencies panel: every catalog currency (Code, Name, Kind, Supply, Holders, Cap), no
/// toolbar and no pager, since the operator-curated catalog bounds it. Kind is a plain badge,
/// accented on [`HARD_KIND`]; Supply and Holders are exact grouped integers in minor units;
/// Cap is [`NO_CAP`] on every row. An empty catalog draws the portal's empty state.
pub(crate) fn currencies_body(stats: &[CurrencyStats]) -> PanelBody {
    let rows: Vec<Row> = stats
        .iter()
        .map(|s| Row {
            entity: None,
            cells: vec![
                text(CellKind::Ref, s.code.clone()),
                text(CellKind::Text, s.display_name.clone()),
                Cell { tone: (s.kind == HARD_KIND).then_some(Tone::Accent), ..text(CellKind::Label, s.kind.clone()) },
                text(CellKind::Number, group_digits(&s.supply)),
                text(CellKind::Number, grouped(s.holders)),
                text(CellKind::Dim, NO_CAP),
            ],
            highlight: false,
            item: None,
            actions: Vec::new(),
        })
        .collect();
    let table = DataTable {
        columns: vec![
            column("code", "Code", CellKind::Ref, Align::Left),
            column("name", "Name", CellKind::Text, Align::Left),
            column("kind", "Kind", CellKind::Label, Align::Left),
            column("supply", "Supply", CellKind::Number, Align::Right),
            column("holders", "Holders", CellKind::Number, Align::Right),
            column("cap", "Cap", CellKind::Dim, Align::Left),
        ],
        page: PageInfo {
            next_cursor: None,
            size: rows.len() as u32,
            total: None,
        },
        rows,
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

/// An operator's write, in the player row menu's wallet group unless it says otherwise.
fn action(id: &str, point: ActionPoint, label: &str, icon: &str, then: Then) -> ActionDecl {
    ActionDecl {
        id: id.to_string(),
        point,
        label: label.to_string(),
        icon: icon.to_string(),
        order: order::player_row_action::WALLET,
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

/// The provider's manifest; `headline` is the catalog code [`WalletAdmin`] headlines, which
/// names the balance column and the spent tile.
pub(crate) fn manifest(headline: &str) -> Manifest {
    let tab = Point::EntityTab {
        kind: KIND.to_string(),
        tab: TAB.to_string(),
        tab_label: "Wallet".to_string(),
        columns: 1,
        params: Vec::new(),
    };
    let tiles = Point::EntityTabTile {
        kind: KIND.to_string(),
        tab: TAB.to_string(),
    };
    let list_column = Point::EntityListColumn {
        kind: KIND.to_string(),
        label: headline_label(headline),
        align: Align::Right,
    };
    let row = || ActionPoint::RowAction(KIND.to_string());
    let form = || ActionPoint::PanelForm(GRANT_PANEL.to_string());
    Manifest {
        module: MODULE.to_string(),
        // The Economy section is a fixed `controlplane::SECTIONS` entry ("Economy", 30); the
        // shell draws a section only once an entry sits in it, and this is its one entry.
        sections: Vec::new(),
        entries: vec![NavEntry {
            section: SECTION.to_string(),
            id: PAGE.to_string(),
            label: "Currencies & ledger".to_string(),
            icon: "coins".to_string(),
            order: 10,
            page: PAGE.to_string(),
            counter: None,
        }],
        pages: vec![PageDecl {
            id: PAGE.to_string(),
            title: "Currencies & ledger".to_string(),
            subtitle: "wallet module".to_string(),
            head_actions: Vec::new(),
            tiles: Vec::new(),
            layout: Layout::Rows(vec![
                Layout::Grid21(vec![CURRENCIES_PANEL.to_string()], vec![GRANT_PANEL.to_string()]),
                Layout::Stack(vec![LEDGER_PANEL.to_string()]),
            ]),
            params: vec![PARAM_CURRENCY.to_string(), PARAM_REASON.to_string()],
        }],
        entity_kinds: Vec::new(),
        contributions: vec![
            contribution(TRANSACTIONS, tab, order::player_tab::WALLET, "Transactions", ""),
            contribution(TILES, tiles, 0, "Wallet", ""),
            contribution(
                FACT_BALANCE,
                Point::EntitySummary(KIND.to_string()),
                order::player_fact::WALLET,
                "Wallet",
                "coins",
            ),
            contribution(CURRENCIES_PANEL, Point::Page(PAGE.to_string()), 10, "Currencies", ""),
            contribution(GRANT_PANEL, Point::Page(PAGE.to_string()), 20, "Grant or revoke", ""),
            contribution(LEDGER_PANEL, Point::Page(PAGE.to_string()), 30, "Ledger", ""),
            contribution(COLUMN_BALANCE, list_column, order::player_column::WALLET, &headline_label(headline), ""),
            // A 24 h total tolerates a minute of staleness, as accounts' and mail's tiles.
            ContributionDecl {
                cache_ttl_s: Some(60),
                ..contribution(
                    TILE_SPENT,
                    Point::DashboardTile,
                    order::dashboard_tile::WALLET,
                    &spent_title(headline),
                    "",
                )
            },
            contribution(
                ATTENTION_PURGES,
                Point::DashboardAttention,
                order::dashboard_attention::WALLET_PURGES,
                "Wallet purges pending",
                "",
            ),
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
            // NEW (D6): the mockup has no create opener. A seeded catalog never draws it.
            ActionDecl {
                order: 10,
                when_empty: true,
                ..action(
                    CURRENCY_CREATE,
                    ActionPoint::PanelTool(CURRENCIES_PANEL.to_string()),
                    "New currency",
                    "plus",
                    Then::Form(create_form()),
                )
            },
        ],
        search: Vec::new(),
        writes: true,
    }
}

#[async_trait]
impl Provider for WalletAdmin {
    fn manifest(&self) -> Manifest {
        manifest(&self.headline)
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, Error> {
        match req.contribution.as_str() {
            TRANSACTIONS => self.transactions(&req).await,
            TILES => self.tiles(&req).await,
            FACT_BALANCE => self.fact(&req).await,
            COLUMN_BALANCE => self.column(&req).await,
            TILE_SPENT => self.spent().await,
            ATTENTION_PURGES => self.purges().await,
            CURRENCIES_PANEL => {
                let stats = self.svc.store.currency_stats().await.map_err(crate::internal)?;
                Ok(currencies_body(&stats))
            }
            GRANT_PANEL => Ok(PanelBody::new(Primitive::Form(panel_form(&self.codes().await?)))),
            LEDGER_PANEL => self.ledger(&req).await,
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
        if req.action == CURRENCY_CREATE {
            return self.create_currency(&req).await;
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
            return self.move_currency(dir, &player, true, &req).await;
        }
        match req.input.get(FIELD_PLAYER).map(|p| p.trim()).filter(|p| !p.is_empty()) {
            Some(player) => self.move_currency(dir, player, false, &req).await,
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
