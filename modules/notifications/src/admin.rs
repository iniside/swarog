//! The "Inbox" admin page under `Player Support`: the newest messages across every player,
//! a per-player drill-down (`?player=<uuid>` — that player's inbox and its unread count)
//! and one editable form for the single operator action, sending a 1:1 message.
//!
//! There is no inbox mockup, so the page is COMPOSED from the shipped `adminapi` widget
//! vocabulary in the shape `UILayout/GameOps Admin.dc.html` gives the Players page — a table
//! plus a row drill-down, no new widget. Every value rendered is read from
//! `notifications.messages`; nothing here is decorative.
//!
//! Two rendering paths share [`build_content`] and two write paths share [`apply_submit`],
//! so LOCAL (monolith, in-process closure) and REMOTE (admin-svc over the mTLS edge via
//! `admin.adminSubmit`) cannot diverge.
//!
//! Sending goes through [`crate::Service::send_operator_mail`] onto
//! [`crate::Service::deliver_on`] — the module's one insert authority — so operator mail
//! inherits the same input policy and the same dedup index as the durable fan-in.

use std::sync::Arc;

use crate::service::{
    is_operator_dedup_key, is_uuid_text, NewNotification, Sent, OPERATOR_DEDUP_HEX,
    OPERATOR_DEDUP_PREFIX,
};
use crate::store::InboxRow;
use crate::Service;

pub(crate) const ADMIN_ITEM_ID: &str = "notifications";
/// The URL this page answers on. The portal derives a page's slug from `slugify(LABEL)`, NOT
/// from the item id (`admin::resolve_items`), so every self-link must be built from THIS —
/// a link built from the id 404s whenever the two differ, as they do here.
pub(crate) const ADMIN_SLUG: &str = "inbox";
/// A NEW sidebar section: the shipped ones are Platform / Identity / Game Content /
/// Economy & Store, and operator mail belongs to none of them.
pub(crate) const ADMIN_SECTION: &str = "Player Support";
pub(crate) const ADMIN_LABEL: &str = "Inbox";

/// The drill-down param. Its value arrives as the `PLAYERS_ROW_MENU` entity ref
/// (`player:<uuid>`); a bare uuid is accepted too, so a hand-typed link works.
const PARAM_PLAYER: &str = "player";
const PLAYER_REF_PREFIX: &str = "player:";

/// How many messages either view lists. Both views are unpaged — the operator narrows by
/// drilling into one player, not by walking the table.
const PAGE: i64 = 50;

/// How much of a message body the table shows, in characters.
const PREVIEW_CHARS: usize = 90;

const ACTION_FIELD: &str = "_action";
const ACTION_SEND_MAIL: &str = "send-mail";

const PLAYER_FIELD: &str = "player_id";
const TITLE_FIELD: &str = "title";
const BODY_FIELD: &str = "body";

/// The idempotency key minted at RENDER time and round-tripped as a hidden input. Minting it
/// at submit time would give every resubmit of one rendered form a fresh key, so a
/// double-click would send the message twice — the exact defect the key prevents.
const IDEM_SEND_FIELD: &str = "_idem_send";

/// Operator mail's `kind`, distinct from every fan-in kind so the table can say who sent it.
pub(crate) const KIND_OPERATOR_MAIL: &str = "operator.mail";

// ============================================================================
// Read view
// ============================================================================

/// This module's cross-page contribution: a "View Inbox" drill-down on each Players row.
/// The link interpolates `{id}` — a key `PLAYERS_ROW_MENU` DECLARES. LOCAL
/// `Item::with_extensions` and REMOTE `ItemData::extensions` carry this SAME vec, so the two
/// cannot drift.
pub(crate) fn extension_entries() -> Vec<adminapi::ExtensionEntry> {
    vec![adminapi::ExtensionEntry {
        point: accountsapi::admin::PLAYERS_ROW_MENU.id.into(),
        label: "View Inbox".into(),
        icon: "mail".into(),
        link: format!("{ADMIN_SLUG}?{PARAM_PLAYER}={{id}}"),
        present: adminapi::Present::Navigate,
        priority: 0,
    }]
}

/// The page, LOCAL and REMOTE alike: the cross-player overview, or one player's inbox when
/// the drill-down param is present.
///
/// A malformed `player` renders an error card, NEVER an `Err` — the portal forwards every
/// page's params to every provider and resolves every item on every request, so in a split an
/// `Err` raised by ANOTHER page's param would degrade this item to an error card in its
/// sidebar. The strict uuid check is the READ path's: a rendered drill-down link always
/// carries the DB-canonical spelling, while the send form deliberately stays tolerant.
pub(crate) async fn build_content(
    svc: &Service,
    params: &adminapi::Params,
) -> anyhow::Result<adminapi::Content> {
    let raw = adminapi::param(params, PARAM_PLAYER).trim();
    if raw.is_empty() {
        // One row past the page decides "older exist" — the same idiom `Player::list` uses,
        // and the reason this view carries no counts.
        let mut rows = svc.store.recent(PAGE + 1).await?;
        let truncated = rows.len() as i64 > PAGE;
        rows.truncate(PAGE as usize);
        return Ok(overview_content(&rows, truncated));
    }
    let player_id = raw.strip_prefix(PLAYER_REF_PREFIX).unwrap_or(raw);
    if !is_uuid_text(player_id) {
        return Ok(error_content("Invalid player — expected a uuid."));
    }
    let (total, unread) = svc.store.player_stats(player_id).await?;
    let messages = svc.store.page_by_player(player_id, None, PAGE).await?;
    Ok(player_content(player_id, total, unread, &messages))
}

/// The cross-player overview: the newest messages and the send form.
///
/// Its KPIs count the LISTED rows only, and say so. A true unread total would be a
/// whole-table aggregate, and `admin::resolve_items` re-fetches every item on every portal
/// request — the per-player totals live on the drill-down, where an index covers them.
fn overview_content(rows: &[InboxRow], truncated: bool) -> adminapi::Content {
    let mut table = adminapi::Table {
        columns: vec![
            "WHEN".into(),
            "PLAYER".into(),
            "KIND".into(),
            "TITLE".into(),
            "STATUS".into(),
        ],
        rows: Vec::with_capacity(rows.len()),
        ..Default::default()
    };
    for r in rows {
        table.rows.push(vec![
            adminapi::Cell::text(short_ts(&r.message.created_at)),
            adminapi::Cell {
                text: short_uuid(&r.player_id).into(),
                mono: true,
                link: format!("{ADMIN_SLUG}?{PARAM_PLAYER}={}", r.player_id),
                ..Default::default()
            },
            kind_cell(&r.message.kind),
            adminapi::Cell::text(&r.message.title),
            status_cell(&r.message.read_at),
        ]);
    }

    adminapi::Content {
        kpis: vec![
            adminapi::Kpi {
                label: "Listed".into(),
                value: rows.len().to_string(),
                sub: page_note(truncated).into(),
            },
            adminapi::Kpi {
                label: "Unread here".into(),
                value: rows
                    .iter()
                    .filter(|r| r.message.read_at.is_empty())
                    .count()
                    .to_string(),
                sub: "of the listed rows".into(),
            },
        ],
        table: Some(table),
        form: Some(build_form("")),
        ..Default::default()
    }
}

/// One player's inbox: a context header, the read/unread split as KPIs, that player's newest
/// messages, and the send form prefilled with them.
fn player_content(
    player_id: &str,
    total: i64,
    unread: i64,
    messages: &[notificationsapi::Notification],
) -> adminapi::Content {
    let short = short_uuid(player_id);

    let mut table = adminapi::Table {
        columns: vec![
            "WHEN".into(),
            "KIND".into(),
            "TITLE".into(),
            "MESSAGE".into(),
            "STATUS".into(),
        ],
        rows: Vec::with_capacity(messages.len()),
        ..Default::default()
    };
    for m in messages {
        table.rows.push(vec![
            adminapi::Cell::text(short_ts(&m.created_at)),
            kind_cell(&m.kind),
            adminapi::Cell::text(&m.title),
            adminapi::Cell::text(preview(&m.body)),
            status_cell(&m.read_at),
        ]);
    }

    adminapi::Content {
        header: Some(adminapi::ContextHeader {
            avatar_text: short.chars().next().unwrap_or('?').to_uppercase().to_string(),
            avatar_color_key: palette(player_id),
            title: short.to_string(),
            subtitle_mono: format!("{PLAYER_REF_PREFIX}{player_id}"),
            right_note: page_note(total > messages.len() as i64).into(),
        }),
        kpis: vec![
            adminapi::Kpi {
                label: "Messages".into(),
                value: total.to_string(),
                sub: String::new(),
            },
            adminapi::Kpi {
                label: "Unread".into(),
                value: unread.to_string(),
                sub: String::new(),
            },
        ],
        table: Some(table),
        form: Some(build_form(player_id)),
        ..Default::default()
    }
}

/// Renders a single message as an error card, so a bad drill-down param is a clean card
/// rather than an error the portal must interpret.
fn error_content(msg: &str) -> adminapi::Content {
    adminapi::Content {
        kpis: vec![adminapi::Kpi {
            label: "Error".into(),
            value: msg.into(),
            sub: String::new(),
        }],
        ..Default::default()
    }
}

// ============================================================================
// The form
// ============================================================================

fn text_field(name: &str, label: &str, value: &str) -> adminapi::Field {
    adminapi::Field {
        name: name.into(),
        label: label.into(),
        value: value.into(),
        kind: adminapi::FieldKind::Text,
        options: Vec::new(),
    }
}

/// The one operator action. `player_prefill` is the drill-down's player (empty on the
/// overview), so writing to the player whose inbox is on screen needs no copy-paste.
///
/// `player_id` is a free-text field on purpose and is NOT shape-checked here: the column's
/// `$1::uuid` cast folds a pasted braced/uppercase spelling onto the one player, and a real
/// typo comes back as a visible rejection from [`apply_submit`].
fn build_form(player_prefill: &str) -> adminapi::Form {
    adminapi::Form {
        action: String::new(),
        fields: vec![
            text_field(PLAYER_FIELD, "Player id", player_prefill),
            text_field(TITLE_FIELD, "Title", ""),
            text_field(BODY_FIELD, "Message", ""),
        ],
        hidden: vec![
            adminapi::HiddenField {
                name: ACTION_FIELD.into(),
                value: ACTION_SEND_MAIL.into(),
            },
            adminapi::HiddenField {
                name: IDEM_SEND_FIELD.into(),
                value: mint_idempotency_key(),
            },
        ],
        submit: None,
    }
}

/// A fresh dedup key, in the exact shape [`is_operator_dedup_key`] admits. `OsRng`, not a
/// clock: two deliberate messages rendered within one clock tick must not collide into a
/// silent "duplicate" that delivers only the first.
fn mint_idempotency_key() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; OPERATOR_DEDUP_HEX / 2];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let mut hex = String::with_capacity(OPERATOR_DEDUP_HEX);
    for b in bytes {
        hex.push_str(&format!("{b:02x}"));
    }
    format!("{OPERATOR_DEDUP_PREFIX}{hex}")
}

// ============================================================================
// Render paths
// ============================================================================

/// The LOCAL editable content: the shared shape plus the in-process submit closure.
pub(crate) async fn admin_content_local(
    svc: &Arc<Service>,
    params: &adminapi::Params,
) -> anyhow::Result<adminapi::Content> {
    let mut content = build_content(svc, params).await?;
    if let Some(form) = content.form.as_mut() {
        let closure_svc = svc.clone();
        form.submit = Some(Arc::new(move |values: adminapi::Params| {
            let svc = closure_svc.clone();
            Box::pin(async move { apply_submit(&svc, values).await.map_err(Rejection::into_local) })
        }));
    }
    Ok(content)
}

/// The synchronous LOCAL render: the store reads are async, the `RenderFn` contract is not,
/// so it bridges via `block_in_place` (requires the multi-thread rt).
pub(crate) fn admin_render(
    svc: &Arc<Service>,
    params: &adminapi::Params,
) -> anyhow::Result<adminapi::Content> {
    let svc = svc.clone();
    let params = params.clone();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(admin_content_local(&svc, &params))
    })
}

// ============================================================================
// Submit — one dispatcher, two topologies
// ============================================================================

/// Why a submit failed, WITH its class, so the local card and the wire status say the same
/// thing. `adminapi::SubmitError` alone cannot carry the class ([`Rejection::Rejected`] and
/// [`Rejection::Internal`] both collapse into `Other`), and the wire needs the distinction:
/// an operator's bad input is not a server fault.
pub(crate) enum Rejection {
    /// The posted form cannot be applied as written: its dedup key is not one this page
    /// minted, or that key has already produced a DIFFERENT message. Both remedies are the
    /// same reload — a fresh render mints a fresh key — and both must NOT read as sent.
    Stale,
    /// The operator's input, or the insert authority's verdict on it (an unparseable player
    /// id, an over-long title or body): 400-class, and the MESSAGE is the point.
    Rejected(String),
    Internal(String),
}

impl Rejection {
    /// LOCAL: `Stale` is the portal's 409 stale-form card; every other verdict rides `Other`
    /// so its message reaches the operator instead of a generic conflict page.
    fn into_local(self) -> adminapi::SubmitError {
        match self {
            Rejection::Stale => adminapi::SubmitError::Conflict,
            Rejection::Rejected(msg) => adminapi::SubmitError::Other(anyhow::anyhow!(msg)),
            Rejection::Internal(msg) => adminapi::SubmitError::Other(anyhow::anyhow!(msg)),
        }
    }

    /// REMOTE: the same three verdicts as typed statuses. NEVER `NotFound` — the edge makes
    /// it indistinguishable from `UnknownMethod`, which would degrade this page to read-only
    /// and hide a real domain error.
    fn into_ops(self) -> opsapi::Error {
        match self {
            Rejection::Stale => opsapi::Error::conflict(
                "notifications: the submitted form is stale — reload the page and try again",
            ),
            Rejection::Rejected(msg) => opsapi::Error::invalid(msg),
            Rejection::Internal(msg) => opsapi::Error::internal(msg),
        }
    }
}

/// Maps the insert authority's verdict onto the admin's error space. `Internal` is the only
/// server fault; everything else — including the `22P02` a mistyped player id raises — keeps
/// its message, which is the only thing that tells the operator what to change.
fn mail_rejection(e: opsapi::Error) -> Rejection {
    match e.status {
        opsapi::Status::Internal => Rejection::Internal(e.msg),
        _ => Rejection::Rejected(e.msg),
    }
}

fn required<'a>(
    values: &'a adminapi::Params,
    field: &str,
    action: &str,
) -> Result<&'a str, Rejection> {
    let v = adminapi::param(values, field).trim();
    if v.is_empty() {
        return Err(Rejection::Rejected(format!(
            "notifications: action {action:?} requires a non-empty {field:?}"
        )));
    }
    Ok(v)
}

/// The render-time dedup key. Anything but the exact minted shape means the posted form did
/// not come from a render of THIS page, and the remedy is a reload — which is why the same
/// rule maps to `Stale` here and to `Status::Invalid` in the authority that re-checks it: the
/// operator gets "reload", a programmatic caller gets a rejection.
fn rendered_key(values: &adminapi::Params) -> Result<&str, Rejection> {
    let key = adminapi::param(values, IDEM_SEND_FIELD).trim();
    if !is_operator_dedup_key(key) {
        return Err(Rejection::Stale);
    }
    Ok(key)
}

/// THE submit authority for this page, run by BOTH topologies: the local closure calls it
/// in-process and `AdminSubmit::admin_submit` calls it server-side after the edge hop, so
/// monolith and split apply identical rules.
pub(crate) async fn apply_submit(
    svc: &Service,
    values: adminapi::Params,
) -> Result<adminapi::SubmitOutcome, Rejection> {
    let action = adminapi::param(&values, ACTION_FIELD).trim();
    match action {
        ACTION_SEND_MAIL => {
            let player_id = required(&values, PLAYER_FIELD, ACTION_SEND_MAIL)?;
            let title = required(&values, TITLE_FIELD, ACTION_SEND_MAIL)?;
            let body = required(&values, BODY_FIELD, ACTION_SEND_MAIL)?;
            let dedup = rendered_key(&values)?;
            let sent = svc
                .send_operator_mail(&NewNotification {
                    player_id,
                    kind: KIND_OPERATOR_MAIL,
                    title,
                    body,
                    source_event_id: dedup,
                })
                .await
                .map_err(mail_rejection)?;
            match sent {
                Sent::Appended | Sent::Duplicate => Ok(adminapi::SubmitOutcome::default()),
                // An edited form resubmitted under its old key: the correction was NOT
                // written, so the operator is told to reload rather than shown a success.
                Sent::KeyReused => Err(Rejection::Stale),
            }
        }
        "" => Err(Rejection::Rejected(
            "notifications: no action posted — reload the page and try again".into(),
        )),
        other => Err(Rejection::Rejected(format!(
            "notifications: unknown action {other:?}"
        ))),
    }
}

#[async_trait::async_trait]
impl adminapi::AdminData for Service {
    /// The admin fan-out READ (`admin.adminData`): the same page a local render produces,
    /// with `form.submit == None` — the write is driven remotely through `admin.adminSubmit`.
    async fn admin_data(
        &self,
        params: adminapi::Params,
    ) -> Result<adminapi::ItemData, opsapi::Error> {
        let content = build_content(self, &params)
            .await
            .map_err(|e| opsapi::Error::internal(e.to_string()))?;
        Ok(adminapi::ItemData {
            id: ADMIN_ITEM_ID.into(),
            section: ADMIN_SECTION.into(),
            label: ADMIN_LABEL.into(),
            content,
            extensions: extension_entries(),
        })
    }
}

#[async_trait::async_trait]
impl adminapi::AdminSubmit for Service {
    /// The opt-in remote WRITE (`admin.adminSubmit`): runs the SAME [`apply_submit`]
    /// server-side, where the store and the insert authority are local — the submit closure
    /// never marshals. `id` is the page slug (ignored — this Service serves exactly the
    /// inbox page).
    async fn admin_submit(
        &self,
        _id: String,
        params: adminapi::Params,
    ) -> Result<adminapi::SubmitOutcome, opsapi::Error> {
        apply_submit(self, params)
            .await
            .map_err(Rejection::into_ops)
    }
}

// ============================================================================
// Presentation helpers
// ============================================================================

/// A capped table MUST say it is capped: the page has no cursor, so a row count beside 50
/// rows otherwise reads as the whole story.
fn page_note(truncated: bool) -> &'static str {
    if truncated {
        "newest only — older rows not shown"
    } else {
        "all of them"
    }
}

fn status_cell(read_at: &str) -> adminapi::Cell {
    if read_at.is_empty() {
        adminapi::Cell {
            text: "Unread".into(),
            badge: "amber".into(),
            ..Default::default()
        }
    } else {
        adminapi::Cell {
            text: "Read".into(),
            badge: "grey".into(),
            ..Default::default()
        }
    }
}

/// `kind` is free-form on the wire, so anything this module does not itself produce gets the
/// neutral chip rather than being asserted as a known class.
fn kind_cell(kind: &str) -> adminapi::Cell {
    let badge = match kind {
        KIND_OPERATOR_MAIL => "blue",
        crate::projection::KIND_WALLET_CREDIT => "green",
        crate::projection::KIND_ACCOUNT_PROMOTED => "amber",
        _ => "grey",
    };
    adminapi::Cell {
        text: kind.into(),
        badge: badge.into(),
        ..Default::default()
    }
}

/// The first [`PREVIEW_CHARS`] characters of a body, on a char boundary.
fn preview(body: &str) -> String {
    match body.char_indices().nth(PREVIEW_CHARS) {
        Some((cut, _)) => format!("{}…", &body[..cut]),
        None => body.to_string(),
    }
}

/// A rendered RFC3339 timestamp truncated to minutes for display.
fn short_ts(ts: &str) -> &str {
    ts.get(..16).unwrap_or(ts)
}

/// The first hex group of a uuid — the short form the header and the overview show (this
/// module knows no display names).
fn short_uuid(uuid: &str) -> &str {
    uuid.split('-').next().unwrap_or(uuid)
}

/// Deterministic avatar-palette key, so one player always draws the same colour.
fn palette(seed: &str) -> String {
    format!("av-{}", seed.bytes().map(|b| b as usize).sum::<usize>() % 6)
}
