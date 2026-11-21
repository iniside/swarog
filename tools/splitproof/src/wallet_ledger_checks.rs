//! [LG1] The Economy Ledger (wallet's `ledger` panel) draws accounts' handle in its Player
//! column. Wallet emits the player's id with an entity link; the portal's label pass swaps the
//! text for accounts' `EntityLabel{player}`. In the split the portal is admin-svc and accounts
//! is accounts-svc, so the label crosses the internal edge: a label that never arrives draws the
//! raw uuid as the cell text, which this check refuses.

use anyhow::{bail, Context, Result};
use sqlx::PgPool;

use crate::characters_admin_checks::{attr, attr_values, cut, get, table, text};
use crate::{extract_form_fields, pass_nonce, poll_count, register_capture, Ctx, Proof};

/// The amount granted here: distinct from the configured starter grant (100), so the ledger row
/// of this grant is found by its delta alone.
const GRANT: i64 = 35;

/// Whether `cell`, a Player cell's raw inner html, draws `handle` as its text and carries
/// `pid` only in its one `/admin/e/player/<pid>` link (attributes are read decoded: the portal
/// escapes `/` in them). A cell with no label draws the plain id as its text (core admin
/// `ref_cell`, `labels::drawn_ref`), failing the last two terms.
pub(super) fn player_cell_ok(cell: &str, pid: &str, handle: &str) -> bool {
    let drawn = text(cell);
    attr_values(cell, "href") == [format!("/admin/e/player/{pid}")]
        && !without_attr(cell, "href").contains(pid)
        && drawn.ends_with(handle)
        && !drawn.contains(pid)
}

/// `html` with every ` {key}="…"` attribute cut out.
fn without_attr(html: &str, key: &str) -> String {
    let pat = format!(" {key}=\"");
    let mut out = String::new();
    let mut rest = html;
    while let Some(at) = rest.find(&pat) {
        out.push_str(&rest[..at]);
        let start = at + pat.len();
        let Some(len) = rest[start..].find('"') else { return out };
        rest = &rest[start + len + 1..];
    }
    out.push_str(rest);
    out
}

/// One operator grant of `amount` gold to `player` through the Economy page's Grant-or-revoke
/// panel form, posted as the browser posts it: a fresh render's form instance (`_form`, whose id
/// is the movement's request id) and csrf, the Grant mode, then the `ledger.grant` action. The
/// portal answers a granted movement with its 303 PRG. Returns the status and the body.
pub(super) async fn panel_grant(
    admin: &reqwest::Client,
    front: &str,
    player: &str,
    amount: i64,
    reason: &str,
) -> Result<(u16, String)> {
    let (code, form) = get(admin, &format!("{front}/admin/_panel/wallet/grant")).await?;
    let fields = extract_form_fields(&form);
    let pick = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).unwrap_or_default();
    let (csrf, form_id) = (pick("_csrf"), pick("_form"));
    if code != 200 || csrf.is_empty() || form_id.is_empty() {
        bail!("render the grant form: status {code} csrf={} form={}: {}", !csrf.is_empty(), !form_id.is_empty(), cut(&form));
    }
    let amount = amount.to_string();
    let r = admin
        .post(format!("{front}/admin/_action/wallet/ledger.grant"))
        .form(&[
            ("_csrf", csrf.as_str()),
            ("_form", form_id.as_str()),
            ("_mode", "grant"),
            ("player", player),
            ("amount", amount.as_str()),
            ("amount__currency", "gold"),
            ("reason", reason),
        ])
        .send()
        .await?;
    Ok((r.status().as_u16(), r.text().await.unwrap_or_default()))
}

async fn checks(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    let nonce = pass_nonce(tag);
    let (pid, _) = register_capture(ctx, front, &format!("lg-{nonce}@test.local"), "Ledger").await?;
    let handle: String =
        sqlx::query_scalar("SELECT display_name || '#' || discriminator FROM accounts.players WHERE id = $1::uuid")
            .bind(&pid)
            .fetch_one(pool)
            .await
            .context("read the registered player's handle")?;

    let (grant_code, grant_body) = panel_grant(admin, front, &pid, GRANT, &format!("splitproof ledger {nonce}")).await?;
    let landed = poll_count(
        pool,
        &format!("SELECT count(*) FROM wallet.ledger WHERE player_id::text = $1 AND delta = {GRANT}"),
        &pid,
        1,
    )
    .await;
    if grant_code != 303 || !landed {
        bail!("fixture: grant to {pid} answered {grant_code}, ledger row landed={landed}: {}", cut(&grant_body));
    }
    let tx: String = sqlx::query_scalar("SELECT id::text FROM wallet.ledger WHERE player_id::text = $1 AND delta = $2")
        .bind(&pid)
        .bind(GRANT)
        .fetch_one(pool)
        .await
        .context("read the grant's ledger id")?;

    // The search narrows the page to this player's movements; the uuid then sits in the search
    // box too, so the "only in the link" half is asserted per cell, inside the table body.
    let (code, panel) = get(admin, &format!("{front}/admin/_panel/wallet/ledger?q={pid}")).await?;
    let (headers, rows) = table(&panel);
    let player_col = headers.iter().position(|h| h == "Player");
    let tx_col = headers.iter().position(|h| h == "Tx");
    let cell = |i: Option<usize>, cells: &[String]| i.and_then(|i| cells.get(i)).cloned().unwrap_or_default();
    let granted = rows.iter().find(|(_, cells)| text(&cell(tx_col, cells)) == tx);
    let drawn: Vec<String> = rows.iter().map(|(_, cells)| text(&cell(player_col, cells))).collect();
    let first = rows.first().map(|(_, cells)| cell(player_col, cells)).unwrap_or_default();
    p.check(
        &format!("[LG1{tag}] economy ledger row for an operator grant draws accounts' player label, uuid only in its link"),
        code == 200
            && attr(&panel, "data-state") == "ok"
            && granted.is_some()
            && rows.iter().all(|(_, cells)| player_cell_ok(&cell(player_col, cells), &pid, &handle)),
        format!("status={code} headers={headers:?} handle={handle:?} pid={pid} tx={tx} found={} player_cells={drawn:?} first_cell={first:?} body={}",
            granted.is_some(), cut(&panel)),
    );
    Ok(())
}

/// The economy ledger's player label, drawn by the portal `admin` holds a session on. In the
/// split that portal is admin-svc and the label comes from accounts-svc over the internal edge.
pub(super) async fn assertions(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    if let Err(e) = checks(ctx, pool, p, front, admin, tag).await {
        p.check(&format!("[LG0{tag}] economy ledger label pass completes"), false, format!("{e:#}"));
    }
    Ok(())
}
