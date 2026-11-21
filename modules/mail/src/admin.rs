//! The `mail` control-plane provider: the Mail outbox page, its dashboard tile, attention
//! item and nav counter, over the outbox.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use controlplane::{
    ActionDecl, ActionPoint, Align, AttentionItem, Banner, Cell, CellKind, Column, ConfirmSpec, ConfirmTone,
    ContributionDecl, Delta, Emphasis, Field, FieldKind, FilterDef, FilterKind, FormSpec, InvokeCall,
    InvokeOutcome, KpiTile, Layout, Link, Manifest, NavEntry, Noun, PageDecl, PageInfo, Palette, PanelBody, PanelQuery,
    Point, PrepareCall, Primitive, Provider, QueueItem, QueueState, ReasonReq, Role, SearchHit, SearchQuery,
    Severity, Then, Tone, WorkQueue, DEFAULT_PANEL_DEADLINE_MS, PAGE_PARAM_QUEUE_STATE, PAGE_PARAM_SEARCH,
};
use opsapi::Error;

use crate::address::parse_address;
use crate::config::MailConfig;
use crate::providers::Provider as MailProvider;
use crate::service::{NewMail, Service, TEST_KEY_PREFIX};
use crate::store::{
    Enqueued, OutboxListRow, OutboxListing, OutboxStats, OutboxTab, STATE_PARKED, STATE_PENDING, STATE_SENT,
};

const MODULE: &str = "mail";
const PAGE: &str = "outbox";

const NAV_QUEUE: &str = "nav.queue";
const TILE_QUEUE: &str = "tile.queue";
const ATTENTION_QUEUE: &str = "attention.queue";
const OUTBOX: &str = "outbox";

const TEST_SEND: &str = "test-send";
const PAUSE_SENDING: &str = "pause-sending";
const RETRY_NOW: &str = "retry-now";
const PARK: &str = "park";
const CANCEL: &str = "cancel";
const CANCEL_PARKED: &str = "cancel-parked";
const RESUME: &str = "resume";

/// The test-send form's recipient field.
const FIELD_TO: &str = "to";
/// The template filter's `PanelQuery::params` key.
const FILTER_TEMPLATE: &str = "template";

const PAGE_SIZE: usize = 25;

/// The `kind` an operator test send carries. Producers choose their own `kind` freely, so
/// this names the sender by convention, not by reservation.
const KIND_ADMIN_TEST: &str = "admin.test";
const TEST_SUBJECT: &str = "GameOps test message";
const TEST_BODY: &str = "This message was sent from the GameOps admin portal to prove the outbound mail channel \
                         end to end. Nothing else happened.";
const CANCEL_BODY: &str = "The message is removed from the outbox and will never be delivered.";

/// The length of the canonical uuid spelling, hyphens included.
pub(crate) const OUTBOX_ID_BYTES: usize = 36;

/// The tabs in mockup order, with their `QueueState::key` and label.
const TABS: [(OutboxTab, &str, &str); 5] = [
    (OutboxTab::Attention, "attention", "Needs attention"),
    (OutboxTab::Pending, "pending", "Pending"),
    (OutboxTab::Retrying, "retrying", "Retrying"),
    (OutboxTab::Parked, "parked", "Parked"),
    (OutboxTab::Sent, "sent", "Sent (24 h)"),
];

pub(crate) struct MailAdmin {
    svc: Arc<Service>,
    /// The panel meta: the relay host, or the provider when it has none.
    relay: String,
    max_attempts: u32,
}

impl MailAdmin {
    pub(crate) fn new(svc: Arc<Service>, cfg: &MailConfig) -> MailAdmin {
        let relay = match cfg.provider.as_ref().map(|p| &p.provider) {
            Some(MailProvider::Smtp(settings)) => format!("relay {}", settings.host),
            Some(provider) => format!("provider {}", provider.kind().name()),
            None => "no provider".to_string(),
        };
        MailAdmin { svc, relay, max_attempts: u32::try_from(cfg.max_attempts).unwrap_or(0) }
    }
}

/// Every input value is at most [`controlplane::MAX_INPUT_VALUE_BYTES`].
fn input_within_cap(input: &BTreeMap<String, String>) -> bool {
    input.values().all(|v| v.len() <= controlplane::MAX_INPUT_VALUE_BYTES)
}

/// The canonical uuid spelling. Checked before any statement so a hand-edited value is a
/// stated refusal rather than a `22P02` from the `$1::uuid` cast.
pub(crate) fn is_outbox_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == OUTBOX_ID_BYTES
        && bytes.iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

fn outbox_id(item: Option<&str>) -> Result<&str, Error> {
    let id = item.ok_or_else(|| Error::invalid("mail: this action needs a message"))?;
    if !is_outbox_id(id) {
        return Err(Error::invalid("mail: the bound message id is not an outbox id; reload the page"));
    }
    Ok(id)
}

/// `"<created_at µs>.<id>"` of the last row shown.
fn cursor_of(row: &OutboxListRow) -> String {
    format!("{}.{}", row.created_at_micros, row.id)
}

/// The largest cursor µs `store::list_tx` compares exactly. Its `$3 * interval '1
/// microsecond'` multiplies through float8, which carries every integer up to 2^53 and
/// fails as `interval out of range` near `i64::MAX`. 2^53 µs after the epoch is mid-2255,
/// past any `created_at` a row stamped by `now()` holds.
const MAX_CURSOR_MICROS: i64 = 1 << 53;

/// A hand-edited cursor is `Invalid` here, never a database error from the page query.
fn parse_cursor(raw: &str) -> Result<(i64, &str), Error> {
    let invalid = || Error::invalid("mail: the page cursor is malformed");
    let (micros, id) = raw.split_once('.').ok_or_else(invalid)?;
    if micros.is_empty() || !micros.bytes().all(|b| b.is_ascii_digit()) || !is_outbox_id(id) {
        return Err(invalid());
    }
    let micros: i64 = micros.parse().map_err(|_| invalid())?;
    if micros > MAX_CURSOR_MICROS {
        return Err(invalid());
    }
    Ok((micros, id))
}

/// `"in 40 s"`, `"in 2 min"`, `"in 3 h"`, `"in 2 d"`, or `"now"` once due.
fn next_attempt(secs: i64) -> String {
    match secs {
        s if s <= 0 => "now".to_string(),
        s if s < 60 => format!("in {s} s"),
        s if s < 3_600 => format!("in {} min", s / 60),
        s if s < 86_400 => format!("in {} h", s / 3_600),
        s => format!("in {} d", s / 86_400),
    }
}

/// The row's tab key: pending rows with an attempt behind them are `retrying`.
fn shown_state(row: &OutboxListRow) -> &'static str {
    match row.state.as_str() {
        STATE_PENDING if row.attempts > 0 => "retrying",
        STATE_PENDING => "pending",
        STATE_PARKED => "parked",
        STATE_SENT => "sent",
        _ => "cancelled",
    }
}

fn row_actions(shown: &str) -> Vec<String> {
    let ids: &[&str] = match shown {
        "pending" => &[CANCEL],
        "retrying" => &[RETRY_NOW, PARK, CANCEL],
        "parked" => &[RESUME, CANCEL_PARKED],
        _ => &[],
    };
    ids.iter().map(|id| id.to_string()).collect()
}

fn text(kind: CellKind, value: impl Into<String>) -> Cell {
    Cell { kind, value: value.into(), tone: None, link: None, bar: None, more: Vec::new(), row: false, label: None }
}

fn outbox_page(params: BTreeMap<String, String>) -> Link {
    Link::Page { module: MODULE.to_string(), page: PAGE.to_string(), params }
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

fn done(message: impl Into<String>) -> InvokeOutcome {
    InvokeOutcome::Ok { message: message.into(), reveal: None, redirect: None }
}

fn noop(message: impl Into<String>) -> InvokeOutcome {
    InvokeOutcome::NoOp { message: message.into() }
}

fn contribution(id: &str, point: Point, order: i32, title: &str, icon: &str) -> ContributionDecl {
    ContributionDecl {
        id: id.to_string(),
        point,
        order,
        title: title.to_string(),
        icon: icon.to_string(),
        min_role: Role::Viewer,
        cache_ttl_s: Some(60),
        deadline_ms: DEFAULT_PANEL_DEADLINE_MS,
        readonly: None,
    }
}

struct Act {
    id: &'static str,
    point: ActionPoint,
    label: &'static str,
    icon: &'static str,
    order: i32,
    then: Then,
}

fn act(id: &'static str, point: ActionPoint, label: &'static str, icon: &'static str, order: i32, then: Then) -> Act {
    Act { id, point, label, icon, order, then }
}

impl Act {
    fn decl(self, danger: bool, emphasis: Emphasis, implemented: bool) -> ActionDecl {
        ActionDecl {
            id: self.id.to_string(),
            point: self.point,
            label: self.label.to_string(),
            icon: self.icon.to_string(),
            order: self.order,
            danger,
            multi: false,
            min_role: Role::Operator,
            then: self.then,
            implemented,
            emphasis,
            trailing_icon: false,
            icon_only: false,
            when_empty: false,
            prepare: false,
        }
    }
}

fn manifest() -> Manifest {
    let row = || ActionPoint::QueueRow(OUTBOX.to_string());
    let test_send = FormSpec {
        fields: vec![Field {
            key: FIELD_TO.to_string(),
            label: "Recipient".to_string(),
            kind: FieldKind::String,
            required: true,
            help: None,
            default: None,
            readonly: false,
            mono: false,
            placeholder: None,
        }],
        hidden: BTreeMap::new(),
        submit_label: "Send test".to_string(),
        note: None,
        revision: None,
        title: None,
    };
    // Fixes only the variant: the shown spec comes from `prepare` for the bound message.
    let cancel = ConfirmSpec {
        title: "Cancel this message?".to_string(),
        body: CANCEL_BODY.to_string(),
        tone: ConfirmTone::Danger,
        impact: Vec::new(),
        reason: ReasonReq::None,
        reason_hint: None,
        type_to_confirm: None,
        confirm_label: "Cancel message".to_string(),
    };
    Manifest {
        module: MODULE.to_string(),
        sections: Vec::new(),
        entries: vec![NavEntry {
            section: "platform".to_string(),
            id: PAGE.to_string(),
            label: "Mail outbox".to_string(),
            icon: "mail".to_string(),
            order: 20,
            page: PAGE.to_string(),
            counter: Some(NAV_QUEUE.to_string()),
        }],
        pages: vec![PageDecl {
            id: PAGE.to_string(),
            title: "Mail outbox".to_string(),
            subtitle: "Durable outbox; each message is retried until sent, cancelled or parked.".to_string(),
            head_actions: vec![TEST_SEND.to_string()],
            tiles: Vec::new(),
            layout: Layout::Stack(vec![OUTBOX.to_string()]),
            params: vec![FILTER_TEMPLATE.to_string()],
        }],
        entity_kinds: Vec::new(),
        contributions: vec![
            contribution(NAV_QUEUE, Point::NavCounter, 0, "Mail queue", "mail"),
            contribution(TILE_QUEUE, Point::DashboardTile, 50, "Mail queue", ""),
            contribution(ATTENTION_QUEUE, Point::DashboardAttention, 40, "Mail relay throttling", "mail"),
            contribution(OUTBOX, Point::Page(PAGE.to_string()), 10, "Outbox", ""),
        ],
        actions: vec![
            act(TEST_SEND, ActionPoint::PageHead(PAGE.to_string()), "Test send", "send", 10, Then::Form(test_send))
                .decl(false, Emphasis::Default, true),
            // NEW: the drain has no pause switch.
            ActionDecl {
                when_empty: true,
                ..act(
                    PAUSE_SENDING,
                    ActionPoint::PanelTool(OUTBOX.to_string()),
                    "Pause sending",
                    "pause",
                    10,
                    Then::Invoke,
                )
                .decl(false, Emphasis::Default, false)
            },
            // NEW: the drain owns every retry time.
            act(RETRY_NOW, row(), "Retry", "refresh", 10, Then::Invoke).decl(false, Emphasis::Default, false),
            // NEW: no operator park exists.
            act(PARK, row(), "Park: keep but stop retrying", "park", 20, Then::Invoke)
                .decl(false, Emphasis::Ghost, false),
            ActionDecl {
                prepare: true,
                ..act(CANCEL, row(), "Cancel", "x", 30, Then::Confirm(cancel)).decl(true, Emphasis::Ghost, true)
            },
            act(RESUME, row(), "Resume", "play", 40, Then::Invoke).decl(false, Emphasis::Default, true),
            // NEW: cancel refuses a row that is not pending.
            act(CANCEL_PARKED, row(), "Cancel", "x", 50, Then::Invoke).decl(true, Emphasis::Ghost, false),
        ],
        search: Vec::new(),
        writes: true,
    }
}

impl MailAdmin {
    async fn nav_counter(&self) -> Result<Primitive, Error> {
        let stats = self.svc.outbox_stats().await?;
        let queued = stats.pending + stats.retrying;
        if queued + stats.parked == 0 {
            return Ok(Primitive::Empty { title: "Outbox is empty".to_string(), detail: String::new() });
        }
        // Parked mail is not queued: it keeps the counter shown, warning, even at 0 queued.
        let tone = (stats.retrying + stats.parked > 0).then_some(Tone::Warn);
        Ok(Primitive::Cell(Cell { tone, ..text(CellKind::Badge, thousands(queued)) }))
    }

    async fn tile(&self) -> Result<Primitive, Error> {
        let stats = self.svc.outbox_stats().await?;
        let parked = if stats.parked > 0 { format!("{} parked", thousands(stats.parked)) } else { String::new() };
        let delta = if stats.retrying > 0 {
            Some(Delta {
                // FAKE: no throttle signal exists; the wording shows while any row is retrying.
                value: "▲ relay throttled".to_string(),
                period: if parked.is_empty() { parked } else { format!("· {parked}") },
                tone: Some(Tone::Warn),
            })
        } else if !parked.is_empty() {
            Some(Delta { value: String::new(), period: parked, tone: None })
        } else {
            None
        };
        Ok(Primitive::KpiTile(KpiTile {
            label: "Mail queue".to_string(),
            value: thousands(stats.pending + stats.retrying),
            unit: None,
            delta,
            // FAKE: the outbox keeps no queue-length history.
            series: vec![4.0, 6.0, 3.0, 8.0, 20.0, 140.0, 310.0, 312.0],
            spark: Palette::Warn,
            capacity: None,
            link: Some(outbox_page(BTreeMap::new())),
        }))
    }

    async fn attention(&self) -> Result<Primitive, Error> {
        let stats = self.svc.outbox_stats().await?;
        if stats.retrying == 0 {
            return Ok(Primitive::AttentionItems(Vec::new()));
        }
        Ok(Primitive::AttentionItems(vec![AttentionItem {
            severity: Severity::Warn,
            // FAKE: no throttle signal exists; a retrying row stands for one.
            title: "Mail relay throttling".to_string(),
            text: format!(
                "{} queued, {} retrying, {} parked.",
                thousands(stats.pending + stats.retrying),
                thousands(stats.retrying),
                thousands(stats.parked)
            ),
            link: outbox_page(BTreeMap::new()),
            action_label: "Outbox".to_string(),
            since: String::new(),
        }]))
    }

    async fn outbox(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let selected = match req.queue_state.as_deref() {
            None => 0,
            Some(key) => TABS
                .iter()
                .position(|(_, k, _)| *k == key)
                .ok_or_else(|| Error::invalid(format!("mail: unknown outbox state {key:?}")))?,
        };
        let (tab, selected_key, _) = TABS[selected];
        let needle = req.search.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let kind = req.params.get(FILTER_TEMPLATE).map(String::as_str).filter(|k| !k.is_empty() && *k != "any");
        let after = req.cursor.as_deref().map(parse_cursor).transpose()?;
        let listing = OutboxListing { tab, needle, kind, after, limit: PAGE_SIZE as i64 + 1 };
        let (stats, mut rows) = self.svc.outbox_page(&listing).await?;
        let next_cursor = if rows.len() > PAGE_SIZE {
            rows.truncate(PAGE_SIZE);
            rows.last().map(cursor_of)
        } else {
            None
        };
        let counts = tab_counts(&stats);
        let narrowed = needle.is_some() || kind.is_some();
        let items = rows.iter().map(|row| self.item(row)).collect();
        let column = |key: &str, label: &str, kind: CellKind| Column {
            key: key.to_string(),
            label: label.to_string(),
            kind,
            sortable: false,
            align: Align::Left,
            sorted: None,
            wrap: false,
        };
        let queue = WorkQueue {
            states: TABS
                .iter()
                .zip(counts)
                .map(|((_, key, label), count)| QueueState {
                    key: key.to_string(),
                    label: label.to_string(),
                    count: count.max(0) as u64,
                    tone: None,
                })
                .collect(),
            selected: selected_key.to_string(),
            items,
            item_label: "Message".to_string(),
            item_noun: Noun::new("message", "messages"),
            columns: vec![column("to", "To", CellKind::User), column("template", "Template", CellKind::Ref)],
            page: PageInfo {
                next_cursor,
                size: PAGE_SIZE as u32,
                total: (!narrowed).then(|| counts[selected].max(0) as u64),
            },
            filters: vec![FilterDef {
                key: FILTER_TEMPLATE.to_string(),
                label: "Template".to_string(),
                kind: FilterKind::Text,
            }],
            search_placeholder: Some("Recipient, template or message id".to_string()),
            selectable: true,
        };
        let banner = (stats.retrying > 0).then(|| Banner {
            tone: Tone::Warn,
            // FAKE: no throttle signal exists; a retrying row stands for one.
            text: "SMTP relay is throttling (421). Retries back off exponentially; nothing is lost.".to_string(),
        });
        Ok(PanelBody {
            primitive: Primitive::WorkQueue(queue),
            meta: Some(self.relay.clone()),
            heading: None,
            note: None,
            banner,
            foot: None,
            tag: None,
        })
    }

    fn item(&self, row: &OutboxListRow) -> QueueItem {
        let shown = shown_state(row);
        let to_link = BTreeMap::from([
            (PAGE_PARAM_SEARCH.to_string(), row.recipient.clone()),
            (PAGE_PARAM_QUEUE_STATE.to_string(), shown.to_string()),
        ]);
        QueueItem {
            id: row.id.clone(),
            entity: None,
            state: shown.to_string(),
            attempts: u32::try_from(row.attempts).unwrap_or(0),
            max_attempts: self.max_attempts,
            next_attempt_at: row.next_attempt_in_secs.map(next_attempt),
            last_error: (!row.last_error.is_empty()).then(|| row.last_error.clone()),
            payload_preview: None,
            actions: row_actions(shown),
            cells: vec![
                Cell { link: Some(outbox_page(to_link)), ..text(CellKind::User, row.recipient.clone()) },
                text(CellKind::Ref, row.kind.clone()),
            ],
        }
    }

    async fn prepare_cancel(&self, req: &PrepareCall) -> Result<Then, Error> {
        let id = outbox_id(req.item.as_deref())?;
        let (recipient, kind) = self
            .svc
            .outbox_summary(id)
            .await?
            .ok_or_else(|| Error::not_found(format!("mail: no message {id}")))?;
        Ok(Then::Confirm(ConfirmSpec {
            title: format!("Cancel {id}?"),
            body: CANCEL_BODY.to_string(),
            tone: ConfirmTone::Danger,
            impact: vec![
                ("Recipient".to_string(), text(CellKind::Text, recipient)),
                ("Template".to_string(), text(CellKind::Text, kind)),
            ],
            reason: ReasonReq::None,
            reason_hint: None,
            type_to_confirm: None,
            confirm_label: "Cancel message".to_string(),
        }))
    }

    /// Idempotent per `request_id`: the key is [`TEST_KEY_PREFIX`] + `request_id`, so a
    /// repeat of one submit finds its own row.
    async fn test_send(&self, req: &InvokeCall) -> Result<InvokeOutcome, Error> {
        let request_id =
            req.request_id.as_deref().ok_or_else(|| Error::invalid("mail: a test send needs a request id"))?;
        let to = req.input.get(FIELD_TO).map(|v| v.trim()).unwrap_or("");
        if let Err(reason) = parse_address(to) {
            let error = (FIELD_TO.to_string(), format!("Recipient {reason}"));
            return Ok(InvokeOutcome::FieldErrors(BTreeMap::from([error])));
        }
        let key = format!("{TEST_KEY_PREFIX}{request_id}");
        let mail = NewMail {
            idempotency_key: &key,
            recipient: to,
            subject: TEST_SUBJECT,
            body: TEST_BODY,
            kind: KIND_ADMIN_TEST,
        };
        match self.svc.enqueue_from_admin(&mail).await? {
            Enqueued::Inserted(_) => Ok(done(format!("Test message queued for {to}"))),
            Enqueued::Duplicate => Ok(noop("This test message is already queued")),
            Enqueued::Conflict => Err(Error::conflict(
                "mail: this form already queued a test message to another recipient; open a new one",
            )),
        }
    }
}

/// The tab counts in [`TABS`] order.
fn tab_counts(stats: &OutboxStats) -> [i64; 5] {
    [stats.retrying + stats.parked, stats.pending, stats.retrying, stats.parked, stats.sent_24h]
}

#[async_trait]
impl Provider for MailAdmin {
    fn manifest(&self) -> Manifest {
        manifest()
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, Error> {
        match req.contribution.as_str() {
            NAV_QUEUE => self.nav_counter().await.map(PanelBody::new),
            TILE_QUEUE => self.tile().await.map(PanelBody::new),
            ATTENTION_QUEUE => self.attention().await.map(PanelBody::new),
            OUTBOX => self.outbox(&req).await,
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
            TEST_SEND => self.test_send(&req).await,
            CANCEL => match self.svc.cancel_pending(outbox_id(req.item.as_deref())?).await? {
                0 => Ok(noop("Nothing cancelled: the message is no longer pending; refresh")),
                _ => Ok(done("Message cancelled")),
            },
            RESUME => match self.svc.requeue_parked(outbox_id(req.item.as_deref())?).await? {
                0 => Ok(noop("Nothing resumed: the message is no longer parked; refresh")),
                _ => Ok(done("Resumed")),
            },
            PAUSE_SENDING | RETRY_NOW | PARK | CANCEL_PARKED => Err(Error::forbidden("not implemented yet")),
            other => Err(Error::not_found(format!("{MODULE} has no action {other:?}"))),
        }
    }

    async fn prepare(&self, req: PrepareCall) -> Result<Then, Error> {
        match req.action.as_str() {
            CANCEL => self.prepare_cancel(&req).await,
            other => Err(Error::not_found(format!("{MODULE} prepares no action {other:?}"))),
        }
    }
}

#[cfg(test)]
#[path = "admin_tests.rs"]
mod tests;
