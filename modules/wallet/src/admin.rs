//! The "Wallet" admin page under `Economy & Store`: the currency catalog, a per-player
//! drill-down (`?player=<uuid>` — balances + the newest ledger movements) and one editable
//! form covering the three operator actions.
//!
//! There is no mockup for this view, so the page is COMPOSED from the shipped `adminapi`
//! widget vocabulary — no new widget, no new template construct. Every value rendered is
//! read from `wallet.currencies` / `wallet.balances` / `wallet.ledger`; nothing here is
//! decorative.
//!
//! Two rendering paths share [`build_content`] and two write paths share [`apply_submit`],
//! so LOCAL (monolith, in-process closure) and REMOTE (admin-svc over the mTLS edge via
//! `admin.adminSubmit`) cannot diverge.
//!
//! The money actions go through [`crate::Service::credit`]/`debit` — the module's one
//! movement authority — never through fresh balance SQL, so the admin path inherits the
//! ledger, the idempotency gate and the durable `wallet.changed` emit unchanged.

use std::sync::Arc;

use walletapi::{Movement, Wallet};

use crate::store::{CatalogEntry, LedgerEntry};
use crate::{is_uuid_text, Service, IDEMPOTENCY_CONFLICT};

pub(crate) const ADMIN_ITEM_ID: &str = "wallet";
/// The sidebar group `UILayout/GameOps Admin.dc.html` names for currency/store surfaces.
pub(crate) const ADMIN_SECTION: &str = "Economy & Store";
pub(crate) const ADMIN_LABEL: &str = "Wallet";

/// The drill-down param. Its value arrives as the `PLAYERS_ROW_MENU` entity ref
/// (`player:<uuid>`); a bare uuid is accepted too, so a hand-typed link works.
const PARAM_PLAYER: &str = "player";
const PLAYER_REF_PREFIX: &str = "player:";

/// How many ledger rows the drill-down asks for; the store clamps to its own hard ceiling.
const LEDGER_PAGE: i64 = 50;

const ACTION_FIELD: &str = "_action";
const ACTION_CREATE_CURRENCY: &str = "create-currency";
const ACTION_GRANT: &str = "grant";
const ACTION_REVOKE: &str = "revoke";

const CODE_FIELD: &str = "code";
const DISPLAY_NAME_FIELD: &str = "display_name";
const KIND_FIELD: &str = "kind";
const DECIMALS_FIELD: &str = "decimals";
const PLAYER_FIELD: &str = "player_id";
const CURRENCY_FIELD: &str = "currency";
const AMOUNT_FIELD: &str = "amount";
const REASON_FIELD: &str = "reason";

/// The idempotency keys minted at RENDER time, one per movement action, round-tripped as
/// hidden inputs. Minting at submit time would give every resubmit of one rendered form a
/// fresh key, so a double-click would move money twice — the exact defect the key prevents.
const IDEM_GRANT_FIELD: &str = "_idem_grant";
const IDEM_REVOKE_FIELD: &str = "_idem_revoke";

// ============================================================================
// Read view
// ============================================================================

/// Wallet's cross-page contributions: a "View Wallet" drill-down on each Players row.
/// The link interpolates `{id}` — a key `PLAYERS_ROW_MENU` DECLARES (`{player_id}` is not
/// one, and admincheck rejects an unfillable placeholder). LOCAL `Item::with_extensions`
/// and REMOTE `ItemData::extensions` carry this SAME vec, so the two cannot drift.
pub(crate) fn extension_entries() -> Vec<adminapi::ExtensionEntry> {
    vec![adminapi::ExtensionEntry {
        point: accountsapi::admin::PLAYERS_ROW_MENU.id.into(),
        label: "View Wallet".into(),
        icon: "wallet".into(),
        link: format!("{ADMIN_ITEM_ID}?{PARAM_PLAYER}={{id}}"),
        present: adminapi::Present::Navigate,
        priority: 0,
    }]
}

/// The page, LOCAL and REMOTE alike: the catalog, or one player's holdings when the
/// drill-down param is present.
///
/// A malformed or foreign `player` renders an error card, NEVER an `Err` — the portal
/// forwards every page's params to every provider, so an `Err` here would poison an
/// unrelated page in a split.
pub(crate) async fn build_content(
    svc: &Service,
    params: &adminapi::Params,
) -> anyhow::Result<adminapi::Content> {
    let catalog = svc.store.list_catalog().await?;
    let raw = adminapi::param(params, PARAM_PLAYER).trim();
    if raw.is_empty() {
        return Ok(catalog_content(&catalog));
    }
    let player_id = raw.strip_prefix(PLAYER_REF_PREFIX).unwrap_or(raw);
    if !is_uuid_text(player_id) {
        return Ok(error_content("Invalid player — expected a uuid."));
    }
    let balances = svc.store.list_balances(player_id).await?;
    let ledger = svc.store.recent_ledger(player_id, LEDGER_PAGE).await?;
    Ok(player_content(player_id, &catalog, &balances, &ledger))
}

/// The catalog view: how many currencies exist, the catalog table, and the action form.
fn catalog_content(catalog: &[CatalogEntry]) -> adminapi::Content {
    let mut table = adminapi::Table {
        columns: vec![
            "CODE".into(),
            "NAME".into(),
            "KIND".into(),
            "DECIMALS".into(),
            "CREATED".into(),
        ],
        rows: Vec::with_capacity(catalog.len()),
        ..Default::default()
    };
    for c in catalog {
        table.rows.push(vec![
            adminapi::Cell::mono(&c.code),
            adminapi::Cell::text(&c.display_name),
            adminapi::Cell {
                text: c.kind.clone(),
                badge: kind_badge(&c.kind).into(),
                ..Default::default()
            },
            adminapi::Cell::text(c.decimals.to_string()),
            adminapi::Cell::text(short_ts(&c.created_at)),
        ]);
    }
    adminapi::Content {
        kpis: vec![adminapi::Kpi {
            label: "Currencies".into(),
            value: catalog.len().to_string(),
            sub: String::new(),
        }],
        table: Some(table),
        form: Some(build_form(catalog, "")),
        ..Default::default()
    }
}

/// The player drill-down: a context header, one KPI tile per held currency, the newest
/// movements as the page's table, and the action form prefilled with this player.
///
/// Balances ride the KPI row rather than a second table because a [`adminapi::Content`]
/// carries exactly ONE table and the ledger is the row-shaped half; a balance is a
/// label/value pair, which is what a KPI tile is.
fn player_content(
    player_id: &str,
    catalog: &[CatalogEntry],
    balances: &[walletapi::Balance],
    ledger: &[LedgerEntry],
) -> adminapi::Content {
    let short = short_uuid(player_id);
    let kpis = if balances.is_empty() {
        vec![adminapi::Kpi {
            label: "Balances".into(),
            value: "none".into(),
            sub: "no currency held".into(),
        }]
    } else {
        balances
            .iter()
            .map(|b| adminapi::Kpi {
                label: display_name(catalog, &b.currency),
                value: b.amount.to_string(),
                sub: b.currency.clone(),
            })
            .collect()
    };

    let mut table = adminapi::Table {
        columns: vec![
            "WHEN".into(),
            "CURRENCY".into(),
            "DELTA".into(),
            "BALANCE AFTER".into(),
            "REASON".into(),
        ],
        rows: Vec::with_capacity(ledger.len()),
        ..Default::default()
    };
    for e in ledger {
        table.rows.push(vec![
            adminapi::Cell::text(short_ts(&e.at)),
            adminapi::Cell::mono(&e.currency),
            adminapi::Cell {
                // Signed: the ledger stores the direction in the sign, and a bare number
                // would render a debit as if it were a credit.
                text: format!("{:+}", e.delta),
                badge: if e.delta < 0 { "red".into() } else { "green".into() },
                ..Default::default()
            },
            adminapi::Cell::text(e.balance_after.to_string()),
            adminapi::Cell::text(&e.reason),
        ]);
    }

    adminapi::Content {
        header: Some(adminapi::ContextHeader {
            avatar_text: short.chars().next().unwrap_or('?').to_uppercase().to_string(),
            avatar_color_key: palette(player_id),
            title: short.to_string(),
            subtitle_mono: format!("{PLAYER_REF_PREFIX}{player_id}"),
            right_note: format!("{} movement(s) shown", ledger.len()),
        }),
        kpis,
        table: Some(table),
        form: Some(build_form(catalog, player_id)),
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

/// A single-choice dropdown with a leading blank, so "nothing chosen" is representable and
/// rejected explicitly instead of defaulting to whatever sorts first.
fn select_field(name: &str, label: &str, mut options: Vec<adminapi::FieldOption>) -> adminapi::Field {
    options.insert(
        0,
        adminapi::FieldOption {
            value: String::new(),
            label: "— choose —".into(),
            checked: true,
        },
    );
    adminapi::Field {
        name: name.into(),
        label: label.into(),
        value: String::new(),
        kind: adminapi::FieldKind::Select,
        options,
    }
}

/// One form, three actions on an explicit `_action` discriminant. `player_prefill` is the
/// drill-down's player (empty on the catalog view), so granting from a player's page needs
/// no copy-paste.
fn build_form(catalog: &[CatalogEntry], player_prefill: &str) -> adminapi::Form {
    let currency_options = catalog
        .iter()
        .map(|c| adminapi::FieldOption {
            value: c.code.clone(),
            label: format!("{} — {}", c.code, c.display_name),
            checked: false,
        })
        .collect();

    let fields = vec![
        adminapi::Field {
            name: ACTION_FIELD.into(),
            label: "Action".into(),
            value: String::new(),
            kind: adminapi::FieldKind::Select,
            options: vec![
                adminapi::FieldOption {
                    value: String::new(),
                    label: "— choose an action —".into(),
                    checked: true,
                },
                adminapi::FieldOption {
                    value: ACTION_CREATE_CURRENCY.into(),
                    label: "Create/update a currency".into(),
                    checked: false,
                },
                adminapi::FieldOption {
                    value: ACTION_GRANT.into(),
                    label: "Grant (credit a player)".into(),
                    checked: false,
                },
                adminapi::FieldOption {
                    value: ACTION_REVOKE.into(),
                    label: "Revoke (debit a player)".into(),
                    checked: false,
                },
            ],
        },
        text_field(CODE_FIELD, "Currency code (create-currency)", ""),
        text_field(DISPLAY_NAME_FIELD, "Display name (create-currency)", ""),
        text_field(KIND_FIELD, "Kind (create-currency) — e.g. soft, hard", ""),
        text_field(
            DECIMALS_FIELD,
            "Decimals (create-currency) — a DISPLAY hint; amounts are always minor units",
            "",
        ),
        text_field(PLAYER_FIELD, "Player id (grant / revoke)", player_prefill),
        select_field(CURRENCY_FIELD, "Currency (grant / revoke)", currency_options),
        text_field(AMOUNT_FIELD, "Amount (grant / revoke) — positive minor units", ""),
        text_field(REASON_FIELD, "Reason (grant / revoke)", ""),
    ];

    adminapi::Form {
        action: String::new(),
        fields,
        // Minted HERE, per render, per action: a resubmit of this rendered form replays the
        // SAME key, so the movement authority collapses it to a no-op instead of moving
        // money twice. One key per action, so choosing the other action on a form whose
        // first key was already spent is not a false conflict.
        hidden: vec![
            adminapi::HiddenField {
                name: IDEM_GRANT_FIELD.into(),
                value: mint_idempotency_key(ACTION_GRANT),
            },
            adminapi::HiddenField {
                name: IDEM_REVOKE_FIELD.into(),
                value: mint_idempotency_key(ACTION_REVOKE),
            },
        ],
        submit: None,
    }
}

/// A fresh movement key. `OsRng`, not a clock: two deliberate grants rendered within one
/// clock tick must not collide into a silent "duplicate" that pays only the first.
fn mint_idempotency_key(action: &str) -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let mut hex = String::with_capacity(32);
    for b in bytes {
        hex.push_str(&format!("{b:02x}"));
    }
    format!("admin-{action}-{hex}")
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
    /// The posted form can no longer be applied as written — the operator must reload (a
    /// fresh render mints fresh idempotency keys).
    Stale,
    /// The operator's input, or the domain's verdict on it (insufficient funds, unknown
    /// currency, an oversized code): 400/409-class, and the MESSAGE is the point.
    Rejected(String),
    Internal(String),
}

impl Rejection {
    /// LOCAL: `Stale` is the portal's 409 stale-form card; every other verdict rides
    /// `Other` so its message reaches the operator instead of a generic conflict page.
    fn into_local(self) -> adminapi::SubmitError {
        match self {
            Rejection::Stale => adminapi::SubmitError::Conflict,
            Rejection::Rejected(msg) => adminapi::SubmitError::Other(anyhow::anyhow!(msg)),
            Rejection::Internal(msg) => adminapi::SubmitError::Other(anyhow::anyhow!(msg)),
        }
    }

    /// REMOTE: the same three verdicts as typed statuses. NEVER `NotFound` — the edge makes
    /// it indistinguishable from `UnknownMethod`, which would degrade this page to
    /// read-only and hide a real domain error.
    fn into_ops(self) -> opsapi::Error {
        match self {
            Rejection::Stale => opsapi::Error::conflict(
                "wallet: the submitted form is stale — reload the page and try again",
            ),
            Rejection::Rejected(msg) => opsapi::Error::invalid(msg),
            Rejection::Internal(msg) => opsapi::Error::internal(msg),
        }
    }
}

/// Maps a movement verdict from the authority onto the admin's error space. The one
/// `Stale` case is the idempotency conflict: the rendered key already records a DIFFERENT
/// movement, so the remedy really is "reload" — a fresh render mints a fresh key. Every
/// other verdict (insufficient funds, unknown currency, a rejected amount) keeps its
/// message, which is the only thing that tells the operator what to change.
fn movement_rejection(e: opsapi::Error) -> Rejection {
    if e.msg == IDEMPOTENCY_CONFLICT {
        return Rejection::Stale;
    }
    match e.status {
        opsapi::Status::Internal => Rejection::Internal(e.msg),
        _ => Rejection::Rejected(e.msg),
    }
}

fn required<'a>(values: &'a adminapi::Params, field: &str, action: &str) -> Result<&'a str, Rejection> {
    let v = adminapi::param(values, field).trim();
    if v.is_empty() {
        return Err(Rejection::Rejected(format!(
            "wallet: action {action:?} requires a non-empty {field:?}"
        )));
    }
    Ok(v)
}

/// The render-time key for `field`. Absent means the posted form did not come from a
/// render of THIS page — applying it would mint no dedup at all, so it is refused.
fn rendered_key(values: &adminapi::Params, field: &str) -> Result<String, Rejection> {
    let key = adminapi::param(values, field).trim();
    if key.is_empty() {
        return Err(Rejection::Stale);
    }
    Ok(key.to_string())
}

fn movement(values: &adminapi::Params, action: &str, idem_field: &str) -> Result<Movement, Rejection> {
    let amount = required(values, AMOUNT_FIELD, action)?;
    let amount: i64 = amount.parse().map_err(|_| {
        Rejection::Rejected(format!("wallet: amount {amount:?} is not a whole number"))
    })?;
    Ok(Movement {
        idempotency_key: rendered_key(values, idem_field)?,
        player_id: required(values, PLAYER_FIELD, action)?.to_string(),
        currency: required(values, CURRENCY_FIELD, action)?.to_string(),
        amount,
        reason: required(values, REASON_FIELD, action)?.to_string(),
    })
}

/// THE submit authority for this page, run by BOTH topologies: the local closure calls it
/// in-process and `AdminSubmit::admin_submit` calls it server-side after the edge hop, so
/// monolith and split apply identical rules.
///
/// Grant/revoke go through [`crate::Service::credit`]/`debit` — the movement authority —
/// so the ledger row, the balance CHECK and the durable `wallet.changed` emit are the same
/// ones every other caller gets.
pub(crate) async fn apply_submit(
    svc: &Service,
    values: adminapi::Params,
) -> Result<adminapi::SubmitOutcome, Rejection> {
    let action = adminapi::param(&values, ACTION_FIELD).trim().to_string();
    match action.as_str() {
        ACTION_CREATE_CURRENCY => {
            let code = required(&values, CODE_FIELD, ACTION_CREATE_CURRENCY)?.to_string();
            let display_name =
                required(&values, DISPLAY_NAME_FIELD, ACTION_CREATE_CURRENCY)?.to_string();
            let kind = required(&values, KIND_FIELD, ACTION_CREATE_CURRENCY)?.to_string();
            let decimals = required(&values, DECIMALS_FIELD, ACTION_CREATE_CURRENCY)?;
            let decimals: i32 = decimals.parse().map_err(|_| {
                Rejection::Rejected(format!("wallet: decimals {decimals:?} is not a whole number"))
            })?;
            if decimals < 0 {
                return Err(Rejection::Rejected(
                    "wallet: decimals must be zero or more".into(),
                ));
            }
            // The catalog's own byte cap, checked here so an over-long code names ITSELF in
            // the message; `currencies_code_len_check` below is the class fail-safe, not a
            // second policy.
            if !crate::currency_code_within_cap(&code) {
                return Err(Rejection::Rejected(format!(
                    "wallet: currency code exceeds {} bytes",
                    walletapi::MAX_CURRENCY_CODE_BYTES
                )));
            }
            let mut conn = svc
                .store
                .pool
                .acquire()
                .await
                .map_err(|e| Rejection::Internal(e.to_string()))?;
            svc.store
                .upsert_currency_tx(&mut conn, &code, &display_name, &kind, decimals)
                .await
                .map_err(catalog_rejection)?;
            Ok(adminapi::SubmitOutcome::default())
        }
        ACTION_GRANT => {
            let m = movement(&values, ACTION_GRANT, IDEM_GRANT_FIELD)?;
            svc.credit(m).await.map_err(movement_rejection)?;
            Ok(adminapi::SubmitOutcome::default())
        }
        ACTION_REVOKE => {
            let m = movement(&values, ACTION_REVOKE, IDEM_REVOKE_FIELD)?;
            svc.debit(m).await.map_err(movement_rejection)?;
            Ok(adminapi::SubmitOutcome::default())
        }
        "" => Err(Rejection::Rejected(
            "wallet: no action selected — pick an action from the dropdown".into(),
        )),
        other => Err(Rejection::Rejected(format!("wallet: unknown action {other:?}"))),
    }
}

/// A catalog write's verdict. An over-long code is the named CHECK — operator input, so a
/// 400-class rejection carrying the reason, never an internal error.
fn catalog_rejection(e: sqlx::Error) -> Rejection {
    let over_long = e.as_database_error().is_some_and(|db| {
        db.code().as_deref() == Some("23514") && db.constraint() == Some("currencies_code_len_check")
    });
    if over_long {
        return Rejection::Rejected(format!(
            "wallet: currency code exceeds {} bytes",
            walletapi::MAX_CURRENCY_CODE_BYTES
        ));
    }
    Rejection::Internal(e.to_string())
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
    /// server-side, where the store and the movement authority are local — the submit
    /// closure never marshals. `id` is the page slug (ignored — this Service serves exactly
    /// the wallet page).
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

/// The catalog's display name for a held currency; the code itself when the catalog row is
/// gone (a balance outlives nothing today — the FK holds — but the read is two statements,
/// so the fallback is real).
fn display_name(catalog: &[CatalogEntry], code: &str) -> String {
    catalog
        .iter()
        .find(|c| c.code == code)
        .map(|c| c.display_name.clone())
        .unwrap_or_else(|| code.to_string())
}

/// Status-pill colour for a catalog `kind`. `kind` is free-form, so anything the domain
/// does not name gets the neutral chip.
fn kind_badge(kind: &str) -> &'static str {
    match kind {
        "soft" => "blue",
        "hard" => "amber",
        _ => "grey",
    }
}

/// A raw `timestamptz::text` truncated to minutes for display.
fn short_ts(ts: &str) -> &str {
    ts.get(..16).unwrap_or(ts)
}

/// The first hex group of a uuid — the short form the header shows (wallet knows no
/// display names).
fn short_uuid(uuid: &str) -> &str {
    uuid.split('-').next().unwrap_or(uuid)
}

/// Deterministic avatar-palette key, so one player always draws the same colour.
fn palette(seed: &str) -> String {
    format!("av-{}", seed.bytes().map(|b| b as usize).sum::<usize>() % 6)
}
