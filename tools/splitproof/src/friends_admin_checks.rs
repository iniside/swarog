use anyhow::{bail, Context, Result};
use sqlx::PgPool;

use crate::characters_admin_checks::{attr, attr_values, cut, get, table, text};
use crate::wallet_ledger_checks::player_cell_ok;
use crate::{friends_call, friends_player, pass_nonce, Ctx, FriendPlayer, Proof};

/// One drawn Friends row: (user cell raw html, user cell text, state badge text).
type Drawn = (String, String, String);

/// The friends provider's Friends tab on the player entity page, drawn by the portal that
/// `admin` holds a session on, over three pass-minted players: A asked B and B accepted, A asked
/// C and C has not answered. In the split the portal is admin-svc: the tab panel and its count
/// are built in friends-svc and reach admin-svc only through its `friends` peer, and each user
/// cell carries only the other player's id, which admin-svc's label pass resolves to the handle
/// through its `accounts` peer (accounts-svc). The friendships are made through the public
/// player API, as the friends pass ([FR1]/[FR2]) makes them.
///
/// Break run (a full splitproof run, then reverted):
/// - admin-svc without its `friends` peer (`cmd/admin-svc/src/lib.rs`): FA1-FA3 red as new in the
///   split (the tab slot is not drawn and the panel and count URLs answer 404), FA1m-FA3m green,
///   0 changed and 0 stale (2026-11-22, step 17t).
pub(super) async fn assertions(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    if let Err(e) = checks(ctx, pool, p, front, admin, tag).await {
        p.check(&format!("[FA0{tag}] friends admin pass completes"), false, format!("{e:#}"));
    }
    Ok(())
}

async fn checks(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    let nonce = pass_nonce(tag);
    let a = friends_player(ctx, front, &format!("fa-a-{nonce}@test.local"), &format!("FaA{nonce}")).await?;
    let b = friends_player(ctx, front, &format!("fa-b-{nonce}@test.local"), &format!("FaB{nonce}")).await?;
    let c = friends_player(ctx, front, &format!("fa-c-{nonce}@test.local"), &format!("FaC{nonce}")).await?;
    let ab = request(ctx, front, &a, &b).await?;
    let (code, body) = friends_call(
        ctx,
        front,
        &b.token,
        reqwest::Method::POST,
        &format!("/friends/requests/{ab}/accept"),
        None,
    )
    .await?;
    if code != 204 {
        bail!("fixture: {} accepting {ab} -> {code} {body}", b.handle);
    }
    let ac = request(ctx, front, &a, &c).await?;
    // The rows as the DB holds them: the tab must draw exactly these two for A.
    for (edge, want) in [(&ab, "accepted"), (&ac, "pending")] {
        let state: Option<String> = sqlx::query_scalar("SELECT state FROM friends.edges WHERE id::text = $1")
            .bind(edge)
            .fetch_optional(pool)
            .await
            .context("read a friends edge")?;
        if state.as_deref() != Some(want) {
            bail!("fixture: edge {edge} is {state:?}, expected {want}");
        }
    }

    // [FA1] A's entity page slots the Friends tab panel and its count; the panel draws the
    // accepted row (B) before the pending one (C), each user cell labelled with the other
    // player's handle and linked to its entity page, the uuid nowhere in the drawn text.
    let want = |pid: &str, c: &str| format!("/admin/_panel/friends/{c}?entity=player%3A{pid}");
    let (page_code, page) = get(admin, &format!("{front}/admin/e/player/{}?tab=friends", a.player_id)).await?;
    let hx = attr_values(&page, "hx-get");
    let slot = hx.iter().any(|u| *u == want(&a.player_id, "friends"));
    let count_slot = hx.iter().any(|u| *u == want(&a.player_id, "tab.count"));
    let (a_code, a_body, a_rows) = panel(admin, &format!("{front}{}", want(&a.player_id, "friends"))).await?;
    let users_ok = a_rows.len() == 2
        && player_cell_ok(&a_rows[0].0, &b.player_id, &b.handle)
        && player_cell_ok(&a_rows[1].0, &c.player_id, &c.handle);
    p.check(
        &format!("[FA1{tag}] Friends tab draws the accepted and the pending row, each user cell labelled with the other player's handle"),
        page_code == 200 && slot && count_slot && a_code == 200 && attr(&a_body, "data-state") == "ok" && users_ok,
        format!(
            "page={page_code} slot={slot} count_slot={count_slot} status={a_code} state={:?} rows={:?} want=[{} {}, {} {}] body={}",
            attr(&a_body, "data-state"),
            shown(&a_rows),
            b.handle,
            b.player_id,
            c.handle,
            c.player_id,
            cut(&a_body)
        ),
    );

    // [FA2] the badge reads the relation as the viewed player sees it: "pending (outgoing)" on
    // the requester's tab, "pending (incoming)" on the addressee's, where the user cell names
    // the requester.
    let (c_code, c_body, c_rows) = panel(admin, &format!("{front}{}", want(&c.player_id, "friends"))).await?;
    let a_badges: Vec<&str> = a_rows.iter().map(|r| r.2.as_str()).collect();
    let c_badges: Vec<&str> = c_rows.iter().map(|r| r.2.as_str()).collect();
    p.check(
        &format!("[FA2{tag}] pending row reads pending (outgoing) on the requester's tab and pending (incoming) on the addressee's"),
        a_badges == ["accepted", "pending (outgoing)"]
            && c_code == 200
            && attr(&c_body, "data-state") == "ok"
            && c_badges == ["pending (incoming)"]
            && c_rows.first().is_some_and(|r| player_cell_ok(&r.0, &a.player_id, &a.handle)),
        format!(
            "requester_badges={a_badges:?} addressee_status={c_code} addressee_state={:?} addressee_rows={:?} want_user={} body={}",
            attr(&c_body, "data-state"),
            shown(&c_rows),
            a.handle,
            cut(&c_body)
        ),
    );

    // [FA3] the tab count counts both relations and the meta names the pending one.
    let (count_code, count) = get(admin, &format!("{front}{}", want(&a.player_id, "tab.count"))).await?;
    let meta = text(&a_body).contains("2 · 1 pending");
    p.check(
        &format!("[FA3{tag}] Friends tab count is 2 and the meta reads 2 · 1 pending"),
        count_code == 200
            && attr(&count, "data-state") == "ok"
            && count.contains(r#"<span class="count">2</span>"#)
            && meta,
        format!("count_status={count_code} count_body={:?} meta_ok={meta} panel_text={:?}", cut(&count), cut(&text(&a_body))),
    );
    Ok(())
}

/// `from` asks `to` by handle; answers the new edge's id.
async fn request(ctx: &Ctx, front: &str, from: &FriendPlayer, to: &FriendPlayer) -> Result<String> {
    let (code, body) = friends_call(
        ctx,
        front,
        &from.token,
        reqwest::Method::POST,
        "/friends/requests",
        Some(serde_json::json!({ "target_handle": to.handle })),
    )
    .await?;
    match body.get("edge_id").and_then(|v| v.as_str()) {
        Some(edge) if code == 201 => Ok(edge.to_string()),
        _ => bail!("fixture: {} asking {} -> {code} {body}", from.handle, to.handle),
    }
}

/// The drawn rows without their raw html, for a failure detail: (user text, href, badge).
fn shown(rows: &[Drawn]) -> Vec<(String, Vec<String>, String)> {
    rows.iter().map(|(cell, user, badge)| (user.clone(), attr_values(cell, "href"), badge.clone())).collect()
}

/// GET one Friends tab panel: (status, body, rows). The User and State cells are found by their
/// header labels, so a column added later cannot shift a badge into the user slot.
async fn panel(admin: &reqwest::Client, url: &str) -> Result<(u16, String, Vec<Drawn>)> {
    let (code, body) = get(admin, url).await?;
    let (headers, rows) = table(&body);
    let at = |label: &str| headers.iter().position(|h| h == label);
    let rows = match (at("User"), at("State")) {
        (Some(u), Some(s)) => rows
            .iter()
            .map(|(_, c)| {
                let user = c.get(u).cloned().unwrap_or_default();
                let drawn = text(&user);
                (user, drawn, c.get(s).map(|x| text(x)).unwrap_or_default())
            })
            .collect(),
        _ => Vec::new(),
    };
    Ok((code, body, rows))
}
