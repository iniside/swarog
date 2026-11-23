//! The `notifications` control-plane provider: the player entity's Inbox tab (one keyset page
//! of the player's messages as a timeline, newest first), its tab count and the dashboard's
//! "Notifications purges pending" attention item; and the operator's Send notification form,
//! opened from the player head, the player row menu and the Inbox tab.

use async_trait::async_trait;
use controlplane::order;
use controlplane::{
    ActionDecl, ActionPoint, Cell, CellKind, ContributionDecl, Emphasis, EntityRef, Field, FieldKind, FormSpec, InvokeCall,
    InvokeOutcome, Manifest, PanelBody, PanelQuery, Point, Primitive, Provider, Role, SearchHit, SearchQuery, Then, Timeline,
    TimelineEntry, Tone, DEFAULT_PANEL_DEADLINE_MS,
};
use notificationsapi::{Notification, MAX_BODY_BYTES, MAX_TITLE_BYTES};
use opsapi::Error;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use crate::internal;
use crate::projection::{KIND_FRIEND_ACCEPTED, KIND_FRIEND_REQUESTED, KIND_WALLET_CREDIT};
use crate::service::{
    body_within_cap, decode_cursor, encode_cursor, is_uuid_text, title_within_cap, NewNotification, Sent, Service,
    OPERATOR_DEDUP_HEX, OPERATOR_DEDUP_PREFIX,
};

const MODULE: &str = "notifications";
/// The entity kind the tab attaches to; accounts declares it.
const KIND: &str = "player";
const TAB: &str = "inbox";
/// Entries per page of the Inbox tab.
pub(crate) const PAGE_SIZE: usize = 25;

pub(crate) const INBOX: &str = "inbox";
pub(crate) const TAB_COUNT: &str = "inbox.count";
pub(crate) const ATTENTION_PURGES: &str = "attention.purges";

/// The kind of an operator-sent message.
pub(crate) const KIND_OPERATOR_MAIL: &str = "operator.mail";

/// The player head's Notify, the row menu's Send notification… and the Inbox tab's tool: three
/// openers of one form and one invoke arm.
pub(crate) const NOTIFY: &str = "notify";
pub(crate) const SEND: &str = "send";
pub(crate) const INBOX_SEND: &str = "inbox.send";

pub(crate) const FIELD_TITLE: &str = "title";
pub(crate) const FIELD_BODY: &str = "body";
const FIELD_REQUEST_ID: &str = "request_id";

pub(crate) const SENT_MESSAGE: &str = "Notification sent";
pub(crate) const KEY_REUSED: &str = "this form was already sent with different text";

pub(crate) struct NotificationsAdmin {
    svc: Arc<Service>,
}

impl NotificationsAdmin {
    pub(crate) fn new(svc: Arc<Service>) -> NotificationsAdmin {
        NotificationsAdmin { svc }
    }

    /// One operator send to the bound player. A retry of one form instance derives the same
    /// key and is a `Duplicate`; the same instance with edited text is refused.
    async fn send(&self, req: &InvokeCall) -> Result<InvokeOutcome, Error> {
        let player = entity_player(req.entity.as_ref())?;
        let request_id = req
            .request_id
            .as_deref()
            .filter(|r| !r.is_empty())
            .ok_or_else(|| Error::invalid(format!("{MODULE}: a send needs a request id")))?;
        let (title, body) = match send_fields(&req.input) {
            Ok(fields) => fields,
            Err(errors) => return Ok(InvokeOutcome::FieldErrors(errors)),
        };
        let key = operator_dedup_key(request_id);
        let n = NewNotification {
            player_id: player,
            kind: KIND_OPERATOR_MAIL,
            title,
            body,
            source_event_id: &key,
        };
        match self.svc.send_operator_mail(&n).await? {
            Sent::Appended | Sent::Duplicate => Ok(InvokeOutcome::Ok {
                message: SENT_MESSAGE.to_string(),
                reveal: None,
                redirect: None,
            }),
            Sent::KeyReused => Err(Error::conflict(KEY_REUSED)),
        }
    }

    /// The Inbox tab: one page older than `before`, newest first, and the meta over every
    /// message the player holds.
    async fn inbox(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let id = player_id(req)?;
        let before = decode_cursor(req.before.as_deref().unwrap_or(""))?;
        let before = before.as_ref().map(|(at, id)| (at.as_str(), id.as_str()));
        let (total, unread) = self.svc.store.player_stats(id).await.map_err(internal)?;
        let rows = self.svc.store.page_by_player(id, before, PAGE_SIZE as i64 + 1).await.map_err(internal)?;
        Ok(inbox_body(&rows, total, unread))
    }

    /// The tab count: every message the player holds, read or not.
    async fn count(&self, req: &PanelQuery) -> Result<i64, Error> {
        let id = player_id(req)?;
        let (total, _) = self.svc.store.player_stats(id).await.map_err(internal)?;
        Ok(total)
    }

    /// The purge backlog as the dashboard's attention item.
    async fn purges(&self) -> Result<PanelBody, Error> {
        let (pending, oldest) = self.svc.store.pending_purges().await.map_err(internal)?;
        Ok(purges_body(pending, &oldest))
    }
}

/// The entity id of a player panel's request.
fn player_id(req: &PanelQuery) -> Result<&str, Error> {
    entity_player(req.entity.as_ref())
}

/// The player id an entity names; any other entity, or none, is refused.
fn entity_player(entity: Option<&EntityRef>) -> Result<&str, Error> {
    match entity {
        Some(entity) if entity.kind == KIND && is_uuid_text(&entity.id) => Ok(&entity.id),
        Some(entity) if entity.kind == KIND => Err(Error::invalid(format!("{MODULE}: malformed id"))),
        _ => Err(Error::invalid(format!("{MODULE}: this panel needs a player"))),
    }
}

fn input_within_cap(input: &BTreeMap<String, String>) -> bool {
    input.values().all(|v| v.len() <= controlplane::MAX_INPUT_VALUE_BYTES)
}

/// The operator dedup key of one form instance: [`OPERATOR_DEDUP_PREFIX`] and the first
/// [`OPERATOR_DEDUP_HEX`] hex digits of the SHA-256 of its request id, the only key shape
/// `Service::send_operator_mail` admits.
pub(crate) fn operator_dedup_key(request_id: &str) -> String {
    let digest = Sha256::digest(request_id.as_bytes());
    let mut key = String::from(OPERATOR_DEDUP_PREFIX);
    for b in digest.iter().take(OPERATOR_DEDUP_HEX / 2) {
        let _ = write!(key, "{b:02x}");
    }
    key
}

/// Title and Message, each required and within its column's cap, or the fields' errors.
pub(crate) fn send_fields(input: &BTreeMap<String, String>) -> Result<(&str, &str), BTreeMap<String, String>> {
    let title = input.get(FIELD_TITLE).map_or("", String::as_str);
    let body = input.get(FIELD_BODY).map_or("", String::as_str);
    let mut errors = BTreeMap::new();
    if title.trim().is_empty() {
        errors.insert(FIELD_TITLE.to_string(), "Title is required".to_string());
    } else if !title_within_cap(title) {
        errors.insert(FIELD_TITLE.to_string(), format!("Title is longer than {MAX_TITLE_BYTES} bytes"));
    }
    if body.trim().is_empty() {
        errors.insert(FIELD_BODY.to_string(), "Message is required".to_string());
    } else if !body_within_cap(body) {
        errors.insert(FIELD_BODY.to_string(), format!("Message is longer than {MAX_BODY_BYTES} bytes"));
    }
    if errors.is_empty() {
        Ok((title, body))
    } else {
        Err(errors)
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

/// The Send notification modal every opener draws; the player is the bound entity.
pub(crate) fn send_form() -> FormSpec {
    FormSpec {
        fields: vec![
            field(FIELD_TITLE, "Title", FieldKind::String, true, Some(format!("At most {MAX_TITLE_BYTES} bytes"))),
            field(FIELD_BODY, "Message", FieldKind::Text, true, Some(format!("At most {MAX_BODY_BYTES} bytes"))),
            Field { mono: true, ..field(FIELD_REQUEST_ID, "Request id", FieldKind::RequestId, false, None) },
        ],
        hidden: BTreeMap::new(),
        submit_label: "Send".to_string(),
        note: None,
        revision: None,
        title: Some("Send notification".to_string()),
        modes: Vec::new(),
        reason: controlplane::ReasonReq::None,
        reason_label: None,
        reason_hint: None,
    }
}

fn send_action(id: &str, point: ActionPoint, label: &str, order: i32) -> ActionDecl {
    ActionDecl {
        id: id.to_string(),
        point,
        label: label.to_string(),
        icon: "send".to_string(),
        order,
        danger: false,
        multi: false,
        min_role: Role::Operator,
        then: Then::Form(send_form()),
        implemented: true,
        emphasis: Emphasis::Default,
        trailing_icon: false,
        icon_only: false,
        when_empty: false,
        prepare: false,
    }
}

/// The tab's meta: "{n} · {u} unread" over every message the player holds.
pub(crate) fn meta_text(total: i64, unread: i64) -> String {
    format!("{total} · {unread} unread")
}

/// The entry icon of a message kind.
pub(crate) fn kind_icon(kind: &str) -> &'static str {
    match kind {
        KIND_WALLET_CREDIT => "gift",
        KIND_FRIEND_REQUESTED | KIND_FRIEND_ACCEPTED => "heart",
        KIND_OPERATOR_MAIL => "mail",
        // `account.promoted` and every other kind.
        _ => "bell",
    }
}

/// One message as a timeline entry: the bold title, the body after it, and the kind with the
/// read state under it; an unread message is accent-toned.
pub(crate) fn entry(n: &Notification) -> TimelineEntry {
    let unread = n.read_at.is_empty();
    TimelineEntry {
        at: n.created_at.clone(),
        actor: None,
        title: Some(n.title.clone()),
        verb: String::new(),
        target: None,
        summary: n.body.clone(),
        badges: Vec::new(),
        detail: Some(format!("{} · {}", n.kind, if unread { "unread" } else { "read" })),
        icon: kind_icon(&n.kind).to_string(),
        tone: unread.then_some(Tone::Accent),
    }
}

/// The Inbox tab panel from up to `PAGE_SIZE + 1` rows, newest first: the surplus row only
/// decides that an older page exists, and `cursor_before` is then the last drawn row.
pub(crate) fn inbox_body(rows: &[Notification], total: i64, unread: i64) -> PanelBody {
    let page = &rows[..rows.len().min(PAGE_SIZE)];
    let cursor_before = match page.last() {
        Some(last) if rows.len() > PAGE_SIZE => Some(encode_cursor(&last.created_at, &last.id)),
        _ => None,
    };
    PanelBody {
        meta: Some(meta_text(total, unread)),
        ..PanelBody::new(Primitive::Timeline(Timeline {
            entries: page.iter().map(entry).collect(),
            cursor_before,
            live: false,
            days: true,
            date_only: false,
        }))
    }
}

/// The tab count: a number cell for every count, 0 included.
pub(crate) fn count_cell(n: i64) -> Cell {
    Cell {
        kind: CellKind::Number,
        value: n.to_string(),
        tone: None,
        link: None,
        bar: None,
        more: Vec::new(),
        row: false,
        label: None,
    }
}

/// The "Notifications purges pending" attention item over the purge sweep backlog.
pub(crate) fn purges_body(pending: i64, oldest: &str) -> PanelBody {
    controlplane::purge_backlog_body(
        "Notifications purges pending",
        pending,
        oldest,
        crate::projection::PURGE_SWEEP_SUB.id,
    )
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
        tab_label: "Inbox".to_string(),
        columns: 1,
        params: Vec::new(),
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
        entity_kinds: Vec::new(),
        contributions: vec![
            contribution(INBOX, tab, order::player_tab::NOTIFICATIONS, "Inbox"),
            contribution(TAB_COUNT, tab_count, 0, "Inbox"),
            contribution(
                ATTENTION_PURGES,
                Point::DashboardAttention,
                order::dashboard_attention::NOTIFICATIONS_PURGES,
                "Notifications purges pending",
            ),
        ],
        actions: vec![
            send_action(
                NOTIFY,
                ActionPoint::HeaderAction(KIND.to_string()),
                "Notify",
                order::player_header_action::NOTIFICATIONS_NOTIFY,
            ),
            send_action(
                SEND,
                ActionPoint::RowAction(KIND.to_string()),
                "Send notification…",
                order::player_row_action::NOTIFICATIONS,
            ),
            ActionDecl {
                when_empty: true,
                ..send_action(INBOX_SEND, ActionPoint::PanelTool(INBOX.to_string()), "Send notification", 0)
            },
        ],
        search: Vec::new(),
        writes: true,
    }
}

#[async_trait]
impl Provider for NotificationsAdmin {
    fn manifest(&self) -> Manifest {
        manifest()
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, Error> {
        match req.contribution.as_str() {
            INBOX => self.inbox(&req).await,
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
            NOTIFY | SEND | INBOX_SEND => self.send(&req).await,
            other => Err(Error::not_found(format!("{MODULE} has no action {other:?}"))),
        }
    }
}
