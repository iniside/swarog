//! The "Mail" admin page under `Platform`: the outbox's four counts, the newest rows, and
//! the four operator verbs the channel needs — requeue one parked row, requeue every parked
//! row, cancel a pending one, and send a test message.
//!
//! `UILayout/GameOps Admin.dc.html` has no outbox panel, so the page is COMPOSED from the
//! shipped widget vocabulary in the shape that file uses elsewhere: a context header, a KPI
//! row, a table, one form. Nothing here is decorative — every value is read from
//! `mail.outbox`.
//!
//! **No cell ever carries `body`.** A rendered body is a verification link or a reset token
//! (`mailevents`' accepted risk), and an operator table is a screen, a screenshot and a
//! browser cache. `crate::store::OutboxListRow` does not even select the column.
//!
//! Two rendering paths share [`build_content`] and two write paths share [`apply_submit`],
//! so LOCAL (monolith, in-process closure) and REMOTE (admin-svc over the mTLS edge via
//! `admin.adminSubmit`) cannot diverge.

use std::sync::Arc;

use crate::service::{is_test_key, NewMail, TEST_KEY_HEX, TEST_KEY_PREFIX};
use crate::store::{
    Enqueued, OutboxListRow, OutboxStats, STATES, STATE_CANCELLED, STATE_PARKED, STATE_PENDING,
    STATE_SENT,
};
use crate::Service;

pub(crate) const ADMIN_ITEM_ID: &str = "mail";
pub(crate) const ADMIN_SECTION: &str = "Platform";
pub(crate) const ADMIN_LABEL: &str = "Mail";

/// The state filter. It exists because the table is capped: a parked row older than the
/// newest [`PAGE`] rows is otherwise neither visible nor selectable, which would leave the
/// `PARKED` count pointing at rows the operator cannot act on one at a time.
const PARAM_STATE: &str = "state";

/// How many rows the table lists. Unpaged — the operator narrows by state, not by walking.
const PAGE: i64 = 50;

/// How many parked rows one `requeue-all-parked` submit moves. A misconfigured relay parks
/// every queued message on its first attempt, so the bulk verb is the recovery path; the
/// cap keeps one submit's transaction bounded, and the reported count tells the operator
/// whether to submit again.
const MAX_BULK_REQUEUE: i64 = 1_000;

/// How much of a `last_error` a cell shows, in characters.
const ERROR_CHARS: usize = 80;

pub(crate) const ACTION_FIELD: &str = "_action";
pub(crate) const ACTION_REQUEUE: &str = "requeue";
const ACTION_REQUEUE_ALL: &str = "requeue-all-parked";
const ACTION_CANCEL: &str = "cancel";
pub(crate) const ACTION_SEND_TEST: &str = "send-test";

pub(crate) const MAIL_ID_FIELD: &str = "mail_id";
pub(crate) const TEST_TO_FIELD: &str = "test_to";

/// The idempotency key minted at RENDER time and round-tripped as a hidden input. Minting
/// it at submit time would give every resubmit of one rendered form a fresh key, so a
/// double-click would enqueue the test message twice — the exact defect the key prevents.
pub(crate) const IDEM_TEST_FIELD: &str = "_idem_test";

/// The `kind` an operator test send carries, so the table can name it. Producers choose
/// their own `kind` freely, so this names the sender by convention, not by reservation.
const KIND_ADMIN_TEST: &str = "admin.test";
const TEST_SUBJECT: &str = "GameOps test message";
const TEST_BODY: &str = "This message was sent from the GameOps admin portal to prove the \
                         outbound mail channel end to end. Nothing else happened.";

// ============================================================================
// Read view
// ============================================================================

/// The page, LOCAL and REMOTE alike.
///
/// A malformed `state` renders an error card, NEVER an `Err` — the portal forwards every
/// page's params to every provider and resolves every item on every request, so in a split
/// an `Err` raised on ANOTHER page's param would degrade this item to an error card in its
/// sidebar.
pub(crate) async fn build_content(
    svc: &Service,
    params: &adminapi::Params,
) -> anyhow::Result<adminapi::Content> {
    let raw = adminapi::param(params, PARAM_STATE).trim();
    let state = match raw {
        "" => None,
        value if STATES.contains(&value) => Some(value),
        other => {
            return Ok(error_content(&format!(
                "Unknown state {other:?} — the outbox states are {STATES:?}."
            )))
        }
    };
    let stats = svc.outbox_stats().await?;
    let mut rows = svc.recent_outbox(state, PAGE + 1).await?;
    let truncated = rows.len() as i64 > PAGE;
    rows.truncate(PAGE as usize);
    Ok(outbox_content(&stats, state, &rows, truncated))
}

fn outbox_content(
    stats: &OutboxStats,
    state: Option<&str>,
    rows: &[OutboxListRow],
    truncated: bool,
) -> adminapi::Content {
    let mut table = adminapi::Table {
        columns: vec![
            "WHEN".into(),
            "TO".into(),
            "KIND".into(),
            "STATE".into(),
            "ATTEMPTS".into(),
            "LAST ERROR".into(),
        ],
        rows: Vec::with_capacity(rows.len()),
        ..Default::default()
    };
    for r in rows {
        table.rows.push(vec![
            adminapi::Cell::text(short_ts(&r.created_at)),
            adminapi::Cell::text(&r.recipient),
            adminapi::Cell::text(&r.kind),
            state_cell(&r.state),
            adminapi::Cell::text(r.attempts.to_string()),
            adminapi::Cell::text(preview(&r.last_error)),
        ]);
    }

    adminapi::Content {
        header: Some(adminapi::ContextHeader {
            avatar_text: "M".into(),
            avatar_color_key: "av-2".into(),
            title: "Outbox".into(),
            subtitle_mono: match state {
                Some(state) => format!("{PARAM_STATE}={state}"),
                None => "all states".into(),
            },
            right_note: page_note(state, truncated),
        }),
        kpis: vec![
            kpi("PENDING", stats.pending.to_string(), "queued to send"),
            kpi("PARKED", stats.parked.to_string(), "waiting on an operator"),
            kpi("SENT 24H", stats.sent_24h.to_string(), "delivered to a relay"),
            kpi(
                "OLDEST PENDING",
                age(stats.oldest_pending_overdue_secs),
                "overdue at the head of the queue",
            ),
        ],
        table: Some(table),
        form: Some(build_form(rows)),
        ..Default::default()
    }
}

/// Renders a bad filter as a card, so a malformed param is a clean page rather than an
/// error the portal must interpret.
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

/// The four verbs on the LISTED rows. The row selector offers exactly what the table shows,
/// so narrowing by state is also how an old parked row becomes selectable.
fn build_form(rows: &[OutboxListRow]) -> adminapi::Form {
    let action_options = vec![
        blank_option("— choose an action —"),
        option(ACTION_REQUEUE, "Requeue the selected parked message"),
        option(
            ACTION_REQUEUE_ALL,
            "Requeue EVERY parked message (up to 1000 per submit)",
        ),
        option(
            ACTION_CANCEL,
            "Cancel the selected pending message (best effort — a message the relay \
             already accepted cannot be recalled)",
        ),
        option(ACTION_SEND_TEST, "Send a test message"),
    ];
    let mut row_options = vec![blank_option("— choose a message —")];
    row_options.extend(rows.iter().map(|r| {
        option(
            &r.id,
            &format!("{} · {} · {}", short_id(&r.id), r.state, r.recipient),
        )
    }));

    adminapi::Form {
        action: String::new(),
        fields: vec![
            adminapi::Field {
                name: ACTION_FIELD.into(),
                label: "Action".into(),
                value: String::new(),
                kind: adminapi::FieldKind::Select,
                options: action_options,
            },
            adminapi::Field {
                name: MAIL_ID_FIELD.into(),
                label: "Message (Requeue / Cancel)".into(),
                value: String::new(),
                kind: adminapi::FieldKind::Select,
                options: row_options,
            },
            adminapi::Field {
                name: TEST_TO_FIELD.into(),
                label: "Recipient (Send a test message)".into(),
                value: String::new(),
                kind: adminapi::FieldKind::Text,
                options: Vec::new(),
            },
        ],
        hidden: vec![
            adminapi::HiddenField {
                name: IDEM_TEST_FIELD.into(),
                value: mint_test_key(),
            },
        ],
        submit: None,
    }
}

/// The leading "nothing chosen" entry, so an unmade choice is representable and explicitly
/// rejected by [`apply_submit`] rather than defaulting to a verb.
fn blank_option(label: &str) -> adminapi::FieldOption {
    adminapi::FieldOption {
        value: String::new(),
        label: label.into(),
        checked: true,
    }
}

fn option(value: &str, label: &str) -> adminapi::FieldOption {
    adminapi::FieldOption {
        value: value.into(),
        label: label.into(),
        checked: false,
    }
}

/// A fresh test-send key, in the exact shape [`crate::service::is_test_key`] admits.
/// `OsRng`, not a clock: two test sends rendered within one clock tick must not collide
/// into a silent "duplicate" that enqueues only the first.
fn mint_test_key() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; TEST_KEY_HEX / 2];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let mut hex = String::with_capacity(TEST_KEY_HEX);
    for b in bytes {
        hex.push_str(&format!("{b:02x}"));
    }
    format!("{TEST_KEY_PREFIX}{hex}")
}

/// The length of the canonical uuid spelling, hyphens included.
pub(crate) const OUTBOX_ID_BYTES: usize = 36;

/// The canonical uuid spelling the row selector renders. Checked before any statement so a
/// hand-edited value is a stated rejection rather than a `22P02` from the `$1::uuid` cast.
fn is_outbox_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == OUTBOX_ID_BYTES
        && bytes.iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
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
    /// The posted form cannot be applied as written: the chosen row left the state the verb
    /// requires, or the test key is not one this page minted. The remedy is the same reload
    /// in both cases, and neither must read as sent.
    Stale,
    /// The operator's input, or the enqueue authority's verdict on it (an unroutable
    /// address, an over-long field): 400-class, and the MESSAGE is the point.
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
                "mail: the submitted form is stale — reload the page and try again",
            ),
            Rejection::Rejected(msg) => opsapi::Error::invalid(msg),
            Rejection::Internal(msg) => opsapi::Error::internal(msg),
        }
    }
}

/// Maps a service verdict onto the admin's error space. `Internal` is the only server
/// fault; everything else keeps its message, which is the only thing that tells the
/// operator what to change.
fn service_rejection(e: opsapi::Error) -> Rejection {
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
            "mail: action {action:?} requires a non-empty {field:?}"
        )));
    }
    Ok(v)
}

/// The selected row id, in the spelling the selector rendered.
fn selected_id<'a>(values: &'a adminapi::Params, action: &str) -> Result<&'a str, Rejection> {
    let id = required(values, MAIL_ID_FIELD, action)?;
    if !is_outbox_id(id) {
        return Err(Rejection::Rejected(
            "mail: the selected message id is not an outbox id — reload the page".into(),
        ));
    }
    Ok(id)
}

/// The render-time test key. Anything but the exact minted shape means the posted form did
/// not come from a render of THIS page, and the remedy is a reload — which is why a fresh
/// render mints a fresh key rather than this path inventing one.
fn rendered_key(values: &adminapi::Params) -> Result<&str, Rejection> {
    let key = adminapi::param(values, IDEM_TEST_FIELD).trim();
    if !is_test_key(key) {
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
        ACTION_REQUEUE => {
            let id = selected_id(&values, ACTION_REQUEUE)?;
            match svc.requeue_parked(id).await.map_err(service_rejection)? {
                0 => Err(Rejection::Stale),
                _ => Ok(adminapi::SubmitOutcome::default()),
            }
        }
        ACTION_REQUEUE_ALL => {
            let (moved, still_parked) = svc
                .requeue_all_parked(MAX_BULK_REQUEUE)
                .await
                .map_err(service_rejection)?;
            Ok(adminapi::SubmitOutcome {
                notice: Some(bulk_report(moved, still_parked)),
                ..Default::default()
            })
        }
        ACTION_CANCEL => {
            let id = selected_id(&values, ACTION_CANCEL)?;
            match svc.cancel_pending(id).await.map_err(service_rejection)? {
                0 => Err(Rejection::Stale),
                _ => Ok(adminapi::SubmitOutcome::default()),
            }
        }
        ACTION_SEND_TEST => {
            let to = required(&values, TEST_TO_FIELD, ACTION_SEND_TEST)?;
            let key = rendered_key(&values)?;
            let enqueued = svc
                .enqueue_from_admin(&NewMail {
                    idempotency_key: key,
                    recipient: to,
                    subject: TEST_SUBJECT,
                    body: TEST_BODY,
                    kind: KIND_ADMIN_TEST,
                })
                .await
                .map_err(service_rejection)?;
            match enqueued {
                Enqueued::Inserted(_) | Enqueued::Duplicate => {
                    Ok(adminapi::SubmitOutcome::default())
                }
                // This form's key was minted for THIS message, so a different message
                // already holding it means the form is not the one that was rendered.
                Enqueued::Conflict => Err(Rejection::Stale),
            }
        }
        "" => Err(Rejection::Rejected(
            "mail: no action selected — pick an action from the dropdown".into(),
        )),
        other => Err(Rejection::Rejected(format!("mail: unknown action {other:?}"))),
    }
}

/// What one bulk requeue moved, reported against what it LEFT — not against the cap.
/// `SKIP LOCKED` and a concurrent operator can end a submit well below `MAX_BULK_REQUEUE`
/// with parked rows still there, so the cap answers nothing; the remaining count is the
/// only figure that tells the operator whether to submit again.
fn bulk_report(moved: i64, still_parked: i64) -> String {
    if still_parked > 0 {
        return format!("Requeued {moved} parked message(s); {still_parked} still parked.");
    }
    format!("Requeued {moved} parked message(s); none left parked.")
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
            ..Default::default()
        })
    }
}

#[async_trait::async_trait]
impl adminapi::AdminSubmit for Service {
    /// The opt-in remote WRITE (`admin.adminSubmit`): runs the SAME [`apply_submit`]
    /// server-side, where the outbox is local — the submit closure never marshals. `id`
    /// names the provider whose page was posted (ignored — this Service serves exactly the
    /// Mail page).
    async fn admin_submit(
        &self,
        _id: String,
        params: adminapi::Params,
    ) -> Result<adminapi::SubmitOutcome, opsapi::Error> {
        apply_submit(self, params).await.map_err(Rejection::into_ops)
    }
}

// ============================================================================
// Presentation helpers
// ============================================================================

fn kpi(label: &str, value: String, sub: &str) -> adminapi::Kpi {
    adminapi::Kpi {
        label: label.into(),
        value,
        sub: sub.into(),
    }
}

/// A capped table MUST say it is capped: the page has no cursor, so [`PAGE`] rows otherwise
/// read as the whole outbox — and the KPI counts above them are whole-table figures, which
/// makes an unstated cap read as a contradiction.
fn page_note(state: Option<&str>, truncated: bool) -> String {
    match (state, truncated) {
        (Some(state), true) => format!("newest {PAGE} {state} rows — older ones not shown"),
        (Some(state), false) => format!("every {state} row"),
        (None, true) => format!("newest {PAGE} rows — older ones not shown"),
        (None, false) => "every row".into(),
    }
}

/// The state chip, doubling as the filter link. The target is DERIVED with
/// `adminapi::slug` — the same authority the portal routes on — never from the item id and
/// never from a hand-written copy of the label: renaming the label then moves the page and
/// its links together instead of 404ing every chip.
fn state_cell(state: &str) -> adminapi::Cell {
    let badge = match state {
        STATE_PENDING => "blue",
        STATE_SENT => "green",
        STATE_PARKED => "red",
        STATE_CANCELLED => "grey",
        _ => "grey",
    };
    adminapi::Cell {
        text: state.into(),
        badge: badge.into(),
        link: format!("{}?{PARAM_STATE}={state}", adminapi::slug(ADMIN_LABEL)),
        ..Default::default()
    }
}

/// A duration in the coarsest unit that still says something: an operator reads "3h", not
/// "10847s".
fn age(secs: f64) -> String {
    let secs = secs.max(0.0) as u64;
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

/// The first [`ERROR_CHARS`] characters of a relay's answer, on a char boundary.
fn preview(value: &str) -> String {
    match value.char_indices().nth(ERROR_CHARS) {
        Some((cut, _)) => format!("{}…", &value[..cut]),
        None => value.to_string(),
    }
}

/// A rendered RFC3339 timestamp truncated to minutes for display.
fn short_ts(ts: &str) -> &str {
    ts.get(..16).unwrap_or(ts)
}

/// The first hex group of a uuid — enough to match a selector option to a table row.
fn short_id(id: &str) -> &str {
    id.split('-').next().unwrap_or(id)
}
