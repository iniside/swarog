use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use sqlx::PgPool;

use crate::apikeys_admin_checks::{fetch, patient};
use crate::characters_admin_checks::{attr, attr_values, cut, table, text};
use crate::{accounts_me, extract_form_fields, pass_nonce, post_json, Ctx, Proof};

/// One login of the pass's player: its bearer, its refresh token and the refresh family both
/// belong to, as `accounts.refresh_tokens` holds it.
struct Login {
    token: String,
    refresh: String,
    family: String,
}

/// One drawn Revoke row button: (its `hx-post` url, its `hx-vals` as posted).
type Button = (String, BTreeMap<String, String>);

/// The accounts provider's two session writes, Revoke (one session, a `Then::Invoke` row button
/// on the Active sessions panel) and Revoke all (a `Then::Confirm` panel tool whose modal accounts
/// prepares), drawn and posted through the portal that `admin` holds a session on, over one
/// pass-registered player with three logins (register and two logins, each its own refresh
/// family). In the split the portal is admin-svc: the panel and its row actions, the prepared
/// modal and both invokes are built or run in accounts-svc and reach admin-svc only through its
/// `accounts` peer over `cp.*`; the effect is read where accounts keeps it (the family's rows in
/// `accounts.refresh_tokens` and `accounts.sessions`) and as a player meets it (the revoked refresh
/// token answers 401 at `/accounts/refresh`, the revoked bearer 401 at `/accounts/me`).
///
/// A repeated Revoke click inside the portal's click-repeat window (5 s) is answered by the
/// portal's own claim and never reaches accounts; [AR3] ages the consumed click row in
/// `admin.forms` past that window, as [A9] ages a consumed refresh row, so the repeat is a
/// deliberate second click that accounts must answer NoOp, not a race against the clock.
///
/// Break runs (each a full splitproof run, then reverted): see the admin-second-pass step 19a
/// commit.
pub(super) async fn assertions(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    if let Err(e) = checks(ctx, pool, p, front, admin, tag).await {
        p.check(&format!("[AR0{tag}] accounts admin writes pass completes"), false, format!("{e:#}"));
    }
    Ok(())
}

async fn checks(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    let nonce = pass_nonce(tag);
    let email = format!("ar-{nonce}@test.local");
    let register = serde_json::json!({ "email": email, "password": "pw", "displayName": "ArA" });
    let login = serde_json::json!({ "email": email, "password": "pw" });
    let (pid, a) = session(ctx, pool, front, "/accounts/register", register).await?;
    let (_, b) = session(ctx, pool, front, "/accounts/login", login.clone()).await?;
    let (_, c) = session(ctx, pool, front, "/accounts/login", login).await?;
    let (me_code, me) = accounts_me(ctx, front, &a.token).await?;
    let handle = me.get("handle").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    if me_code != 200 || !handle.starts_with("ArA#") {
        bail!("fixture: accounts/me for {pid} -> {me_code} handle {handle:?}");
    }
    let mut want = vec![a.family.clone(), b.family.clone(), c.family.clone()];
    want.sort_unstable();
    want.dedup();
    let held = families(pool, &pid).await?;
    if want.len() != 3 || held != want {
        bail!("fixture: {pid} holds families {held:?}, expected three distinct {want:?}");
    }
    let entity = format!("player:{pid}");
    let entity_q = format!("player%3A{pid}");

    // [AR1] the player's Profile tab slots the Active sessions panel; the panel draws one row
    // per family with meta 3, each row's Revoke an enabled button posting revoke-session bound
    // to the player and that row's family, and the Revoke all tool opens the player's
    // revoke-all modal. Nothing on the panel is drawn refused.
    let sessions_url = format!("/admin/_panel/accounts/sessions?entity={entity_q}");
    let revoke_all_url = format!("/admin/_action/accounts/revoke-all?entity={entity_q}");
    let (page_code, page) = fetch(admin, &format!("{front}/admin/e/player/{pid}?tab=profile")).await?;
    let slot = attr_values(&page, "hx-get").contains(&sessions_url);
    let (s_code, s_body) = fetch(admin, &format!("{front}{sessions_url}")).await?;
    let buttons = row_buttons(&s_body);
    let csrf_drawn = !buttons.is_empty() && buttons.iter().all(|(_, vals)| vals.get("_csrf").is_some_and(|v| !v.is_empty()));
    // The csrf value is the operator's, not the row's: compare every val but it.
    let mut drawn: Vec<Button> = buttons
        .into_iter()
        .map(|(url, mut vals)| {
            vals.remove("_csrf");
            (url, vals)
        })
        .collect();
    drawn.sort();
    let want_rows: Vec<Button> = want
        .iter()
        .map(|family| {
            let url = format!("/admin/_action/accounts/revoke-session?entity={entity_q}&item={family}");
            let vals = [("_entity", entity.clone()), ("_item", family.clone())];
            (url, vals.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
        })
        .collect();
    let tool = attr_values(&s_body, "hx-get").contains(&revoke_all_url);
    let refused = attr_values(&s_body, "data-refused-action");
    p.check(
        &format!("[AR1{tag}] Active sessions draws one enabled Revoke per family bound to it and the Revoke all tool"),
        page_code == 200
            && slot
            && s_code == 200
            && attr(&s_body, "data-state") == "ok"
            && meta(&s_body).as_deref() == Some("3")
            && drawn == want_rows
            && csrf_drawn
            && tool
            && refused.is_empty(),
        format!(
            "page={page_code} slot={slot} status={s_code} state={:?} meta={:?} drawn={drawn:?} want={want_rows:?} \
             csrf_drawn={csrf_drawn} tool={tool} refused={refused:?} body={}",
            attr(&s_body, "data-state"),
            meta(&s_body),
            cut(&s_body)
        ),
    );

    let csrf = extract_form_fields(&page).into_iter().find(|(k, _)| k == "_csrf").map(|(_, v)| v).unwrap_or_default();
    if csrf.is_empty() {
        bail!("no _csrf on {pid}'s entity page: {}", cut(&page));
    }
    let click = [("_csrf", csrf.as_str()), ("_entity", entity.as_str()), ("_item", b.family.as_str())];
    let refresh_url = format!("{front}/accounts/refresh");

    // [AR2] Revoke on B's row, posted as the row button posts it, answers "Session revoked":
    // B's family holds no row, B's refresh token and bearer answer 401, and A and C still hold
    // their rows and authenticate.
    let ar2 = async {
        let first = submit(admin, front, "revoke-session", &click).await?;
        let gone = rows_of(pool, &b.family).await?;
        let kept = (rows_of(pool, &a.family).await?, rows_of(pool, &c.family).await?);
        let b_refresh = post_json(ctx, &refresh_url, serde_json::json!({ "refresh_token": b.refresh })).await?.0;
        let b_me = accounts_me(ctx, front, &b.token).await?.0;
        let others_me = (accounts_me(ctx, front, &a.token).await?.0, accounts_me(ctx, front, &c.token).await?.0);
        let ok = first == (303, "Session revoked".to_string())
            && gone == 0
            && kept.0 > 0
            && kept.1 > 0
            && b_refresh == 401
            && b_me == 401
            && others_me == (200, 200);
        anyhow::Ok((
            ok,
            format!("post={first:?} b_rows={gone} a_c_rows={kept:?} b_refresh={b_refresh} b_me={b_me} a_c_me={others_me:?}"),
        ))
    }
    .await;
    let (ar2_ok, ar2_detail) = ar2.unwrap_or_else(|e| (false, format!("{e:#}")));
    p.check(
        &format!("[AR2{tag}] portal Revoke ends that one session: its rows gone, its refresh and bearer 401, the others live"),
        ar2_ok,
        ar2_detail,
    );

    // [AR3] the same Revoke clicked again past the click-repeat window reaches accounts and
    // answers NoOp "That session is no longer active", leaving A's and C's rows as they were.
    let ar3 = async {
        let before = (rows_of(pool, &a.family).await?, rows_of(pool, &c.family).await?);
        let aged = sqlx::query(
            "UPDATE admin.forms SET consumed_at = consumed_at - interval '10 seconds' \
              WHERE module = 'accounts' AND action = 'revoke-session' AND entity_id = $1 AND item = $2 \
                AND consumed_at IS NOT NULL",
        )
        .bind(&pid)
        .bind(&b.family)
        .execute(pool)
        .await
        .context("age the consumed revoke-session click")?
        .rows_affected();
        let second = submit(admin, front, "revoke-session", &click).await?;
        let after = (rows_of(pool, &a.family).await?, rows_of(pool, &c.family).await?);
        let ok = aged == 1
            && second == (303, "That session is no longer active".to_string())
            && before.0 > 0
            && before.1 > 0
            && after == before;
        anyhow::Ok((ok, format!("aged={aged} repeat={second:?} a_c_rows_before={before:?} after={after:?}")))
    }
    .await;
    let (ar3_ok, ar3_detail) = ar3.unwrap_or_else(|e| (false, format!("{e:#}")));
    p.check(&format!("[AR3{tag}] a repeated portal Revoke of the ended session is NoOp"), ar3_ok, ar3_detail);

    // [AR4] Revoke all: the prepared modal names the player's handle and counts its two live
    // sessions; confirmed, it answers "All sessions revoked", the player holds no family, and
    // A's and C's refresh tokens and bearers answer 401.
    let title = format!("Revoke all sessions of {handle}?");
    let ar4 = async {
        let (modal_title, impact, first) = revoke_all(admin, front, &revoke_all_url).await?;
        let left = families(pool, &pid).await?;
        let a_refresh = post_json(ctx, &refresh_url, serde_json::json!({ "refresh_token": a.refresh })).await?.0;
        let c_refresh = post_json(ctx, &refresh_url, serde_json::json!({ "refresh_token": c.refresh })).await?.0;
        let me = (accounts_me(ctx, front, &a.token).await?.0, accounts_me(ctx, front, &c.token).await?.0);
        let ok = modal_title == title
            && impact.as_deref() == Some("2")
            && first == (303, "All sessions revoked".to_string())
            && left.is_empty()
            && (a_refresh, c_refresh) == (401, 401)
            && me == (401, 401);
        anyhow::Ok((
            ok,
            format!(
                "modal_title={modal_title:?} want={title:?} impact={impact:?} post={first:?} families_left={left:?} \
                 a_c_refresh={:?} a_c_me={me:?}",
                (a_refresh, c_refresh)
            ),
        ))
    }
    .await;
    let (ar4_ok, ar4_detail) = ar4.unwrap_or_else(|e| (false, format!("{e:#}")));
    p.check(
        &format!("[AR4{tag}] portal Revoke all names the player, counts 2 sessions and ends every one of them"),
        ar4_ok,
        ar4_detail,
    );

    // [AR5] Revoke all again through a fresh modal: it counts 0 sessions and answers NoOp
    // "The player has no sessions"; the player still holds no family.
    let ar5 = async {
        let (modal_title, impact, second) = revoke_all(admin, front, &revoke_all_url).await?;
        let left = families(pool, &pid).await?;
        let ok = modal_title == title
            && impact.as_deref() == Some("0")
            && second == (303, "The player has no sessions".to_string())
            && left.is_empty();
        anyhow::Ok((ok, format!("modal_title={modal_title:?} impact={impact:?} repeat={second:?} families_left={left:?}")))
    }
    .await;
    let (ar5_ok, ar5_detail) = ar5.unwrap_or_else(|e| (false, format!("{e:#}")));
    p.check(&format!("[AR5{tag}] a repeated portal Revoke all is NoOp over a player with no sessions"), ar5_ok, ar5_detail);
    Ok(())
}

/// One register or login through the front door: (player id, the login). The family is read
/// off the refresh token's row, so a call that minted no family fails the fixture.
async fn session(ctx: &Ctx, pool: &PgPool, front: &str, path: &str, body: serde_json::Value) -> Result<(String, Login)> {
    let (code, got) = post_json(ctx, &format!("{front}{path}"), body).await?;
    let field = |k: &str| got.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let (pid, token, refresh) = (field("player_id"), field("token"), field("refresh_token"));
    if !(200..300).contains(&code) || pid.is_empty() || token.is_empty() || refresh.is_empty() {
        bail!("fixture: {path} -> {code} {got}");
    }
    let family: Option<String> = sqlx::query_scalar("SELECT family_id::text FROM accounts.refresh_tokens WHERE token = $1")
        .bind(&refresh)
        .fetch_optional(pool)
        .await
        .context("read a login's family")?;
    let family = family.with_context(|| format!("fixture: {path}'s refresh token has no row"))?;
    Ok((pid, Login { token, refresh, family }))
}

/// The refresh token and access session rows of one family.
async fn rows_of(pool: &PgPool, family: &str) -> Result<i64> {
    sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM accounts.refresh_tokens WHERE family_id::text = $1) \
              + (SELECT count(*) FROM accounts.sessions WHERE family_id::text = $1)",
    )
    .bind(family)
    .fetch_one(pool)
    .await
    .context("count a family's rows")
}

/// The families holding any refresh token or access session of a player, sorted.
async fn families(pool: &PgPool, pid: &str) -> Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT family_id::text FROM accounts.refresh_tokens WHERE player_id::text = $1 \
         UNION \
         SELECT family_id::text FROM accounts.sessions WHERE player_id::text = $1 \
         ORDER BY 1",
    )
    .bind(pid)
    .fetch_all(pool)
    .await
    .context("read a player's families")
}

/// One POST of `fields` to `accounts/{action}` as the browser posts it, then the PRG's `Location`
/// followed and its flash toast read. Answers (status, toast text); without a `Location` the
/// text is the answer's own.
async fn submit(admin: &reqwest::Client, front: &str, action: &str, fields: &[(&str, &str)]) -> Result<(u16, String)> {
    let r = patient(|| admin.post(format!("{front}/admin/_action/accounts/{action}")).form(fields)).await?;
    let status = r.status().as_u16();
    let location = r.headers().get("location").and_then(|v| v.to_str().ok()).map(str::to_string);
    let posted = r.text().await.unwrap_or_default();
    let Some(location) = location else {
        return Ok((status, text(&posted)));
    };
    let (_, landed) = fetch(admin, &format!("{front}{location}")).await?;
    Ok((status, toast(&landed)))
}

/// One Revoke all as the browser does it: GET the modal at `url` (the portal asks accounts to
/// prepare it), read its `_csrf` and `_form` and POST them. Answers (modal title, the Active
/// sessions impact value, (post status, toast text)).
async fn revoke_all(admin: &reqwest::Client, front: &str, url: &str) -> Result<(String, Option<String>, (u16, String))> {
    let (code, modal) = fetch(admin, &format!("{front}{url}")).await?;
    let fields = extract_form_fields(&modal);
    let pick = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).unwrap_or_default();
    let (csrf, form_id) = (pick("_csrf"), pick("_form"));
    if code != 200 || csrf.is_empty() || form_id.is_empty() {
        bail!("open the revoke-all modal: status {code} csrf={} form={}: {}", !csrf.is_empty(), !form_id.is_empty(), cut(&modal));
    }
    let title = section(&modal, "<h2>", "</h2>");
    let impact = impact(&modal, "Active sessions");
    let posted = submit(admin, front, "revoke-all", &[("_csrf", csrf.as_str()), ("_form", form_id.as_str())]).await?;
    Ok((title, impact, posted))
}

/// The value the modal's impact list draws for `label` (`<span>{label}</span>` then its `<b>`),
/// none when the modal draws no impact or no such entry.
fn impact(html: &str, label: &str) -> Option<String> {
    let list = &html[html.find(r#"<div class="impact">"#)?..];
    let entry = &list[list.find(&format!("<span>{label}</span>"))?..];
    let start = entry.find("<b>")? + "<b>".len();
    let len = entry[start..].find("</b>")?;
    Some(text(&entry[start..start + len]))
}

/// The text of `html` from after the first `open` up to the next `close`, empty when `open` is
/// not drawn.
fn section(html: &str, open: &str, close: &str) -> String {
    let Some(at) = html.find(open) else { return String::new() };
    let rest = &html[at + open.len()..];
    let end = rest.find(close).unwrap_or(rest.len());
    text(&rest[..end])
}

/// Every drawn row's `hx-post` button as (url, hx-vals). A row button drawn refused carries no
/// `hx-post`, so it is not here.
fn row_buttons(html: &str) -> Vec<Button> {
    let (_, rows) = table(html);
    rows.iter()
        .flat_map(|(_, cells)| cells.iter())
        .flat_map(|cell| {
            let vals = attr_values(cell, "hx-vals").into_iter().map(|v| serde_json::from_str(&v).unwrap_or_default());
            attr_values(cell, "hx-post").into_iter().zip(vals).collect::<Vec<_>>()
        })
        .collect()
}

/// The flash toast's text (`<div data-toast …>`), empty when the page draws none.
fn toast(html: &str) -> String {
    let Some(at) = html.find("<div data-toast") else { return String::new() };
    let rest = &html[at..];
    let end = rest.find("</div>").unwrap_or(rest.len());
    text(&rest[..end])
}

/// The panel head's meta text (`<span class="panel-meta">`), none when the panel draws no meta.
fn meta(html: &str) -> Option<String> {
    let open = r#"<span class="panel-meta">"#;
    let start = html.find(open)? + open.len();
    let len = html[start..].find("</span>")?;
    Some(text(&html[start..start + len]))
}
