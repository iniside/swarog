use anyhow::{bail, Context, Result};
use sqlx::PgPool;

use crate::characters_admin_checks::{attr, attr_values, cut, get, table, text};
use crate::{count_of, create_character, friends_call, pass_nonce, poll_count, register_capture, Ctx, Proof};

/// The item the pass grants the player directly through the dev grant route.
const PLAYER_ITEM: &str = "coin";
const PLAYER_QTY: i64 = 3;

/// One drawn holding row: (item, type, qty, owner) as the operator reads them.
type Drawn = (String, String, String, String);

/// The inventory provider's Inventory tab on the player entity page, drawn by the portal that
/// `admin` holds a session on, over a player holding one item directly and its character one
/// through its `character.created` starter grant. In the split the portal is admin-svc, which
/// reaches inventory only through its `inventory` peer and labels the character owner through
/// its `characters` peer; the character row exists only once `character.created` crossed from
/// characters-svc to inventory-svc and filled `inventory.character_owners`; and the Owner select
/// narrows the rows only when the tab's params survive the internal edge to inventory-svc's
/// panel query.
pub(super) async fn assertions(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    if let Err(e) = checks(ctx, pool, p, front, admin, tag).await {
        p.check(&format!("[IV0{tag}] inventory admin pass completes"), false, format!("{e:#}"));
    }
    Ok(())
}

async fn checks(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    let nonce = pass_nonce(tag);
    let (pid, token) = register_capture(ctx, front, &format!("iv-{nonce}@test.local"), "IvA").await?;
    let name = format!("IvChar-{nonce}");
    let cid = create_character(ctx, front, &token, &name).await.context("create the character")?;
    let (code, body) = friends_call(
        ctx,
        front,
        &token,
        reqwest::Method::POST,
        "/inventory/me/grant",
        Some(serde_json::json!({ "item_id": PLAYER_ITEM, "qty": PLAYER_QTY })),
    )
    .await?;
    if !(200..300).contains(&code) {
        bail!("fixture: grant {PLAYER_ITEM} -> {code} {body}");
    }
    // character.created is durable and consumed by inventory: wait for both of its effects (the
    // owner record the tab's query joins and the starter holding) before reading the panel.
    let owner_rec = poll_count(pool, "SELECT count(*) FROM inventory.character_owners WHERE character_id::text = $1", &cid, 1).await;
    let starter = poll_count(
        pool,
        "SELECT count(*) FROM inventory.holdings WHERE owner_type = 'character' AND owner_id::text = $1",
        &cid,
        1,
    )
    .await;
    if !(owner_rec && starter) {
        bail!("fixture: character.created not applied by inventory for {cid}: owner_record={owner_rec} starter_holding={starter}");
    }
    let mine = count_of(pool, "SELECT count(*) FROM inventory.holdings WHERE owner_type = 'player' AND owner_id::text = $1", &pid).await;
    if mine != 1 {
        bail!("fixture: player {pid} holds {mine} items directly, expected 1");
    }
    // The rows as the DB holds them, drawn by name; the starter item is asserted by owner only,
    // since [C2] can retarget it.
    let item_of = |sql: &'static str, id: String| async move {
        sqlx::query_as::<_, (String, String, i64)>(sql).bind(id).fetch_one(pool).await
    };
    let (p_name, p_kind, p_qty) = item_of(
        "SELECT i.name, i.kind, h.quantity::bigint FROM inventory.holdings h JOIN inventory.items i ON i.id = h.item_id \
          WHERE h.owner_type = 'player' AND h.owner_id::text = $1",
        pid.clone(),
    )
    .await
    .context("read the player holding")?;
    let (c_name, c_kind, c_qty) = item_of(
        "SELECT i.name, i.kind, h.quantity::bigint FROM inventory.holdings h JOIN inventory.items i ON i.id = h.item_id \
          WHERE h.owner_type = 'character' AND h.owner_id::text = $1",
        cid.clone(),
    )
    .await
    .context("read the character holding")?;
    if p_qty != PLAYER_QTY {
        bail!("fixture: player holding qty {p_qty}, expected {PLAYER_QTY}");
    }
    // The owner cell of a character's holding draws the character's name: characters' EntityLabel
    // for the character id inventory answered.
    let player_row: Drawn = (p_name, p_kind, p_qty.to_string(), "player".to_string());
    let char_row: Drawn = (c_name, c_kind, c_qty.to_string(), name.clone());

    // [IV1] the entity page slots the Inventory tab panel and its count; the unfiltered panel
    // draws the character row (key order: owner type 'character' before 'player') and the
    // player row.
    let entity = format!("entity=player%3A{pid}");
    let want = |c: &str| format!("/admin/_panel/inventory/{c}?{entity}");
    let (page_code, page) = get(admin, &format!("{front}/admin/e/player/{pid}?tab=inventory")).await?;
    let hx = attr_values(&page, "hx-get");
    let (slot, count_slot) = (hx.iter().any(|u| *u == want("inventory")), hx.iter().any(|u| *u == want("tab.count")));
    let (all_code, all, all_head, all_rows) = panel(admin, &format!("{front}{}", want("inventory"))).await?;
    p.check(&format!("[IV1{tag}] Inventory tab draws the player's own row and its character's starter row, owner cell labelled"),
        page_code == 200 && slot && count_slot && all_code == 200 && attr(&all, "data-state") == "ok"
            && all_head == ["Item", "Type", "Qty", "Owner", "Source", "Acquired", ""]
            && all_rows == [char_row.clone(), player_row.clone()]
            && text(&all).contains(&meta(2, &pid)),
        format!("page={page_code} slot={slot} count_slot={count_slot} status={all_code} state={:?} headers={all_head:?} rows={all_rows:?} want={:?} body={}",
            attr(&all, "data-state"), [&char_row, &player_row], cut(&all)));

    // [IV2] owner=character narrows to the character's row, and the meta counts the filtered set.
    let (ch_code, ch, _, ch_rows) = panel(admin, &format!("{front}{}&owner=character", want("inventory"))).await?;
    p.check(&format!("[IV2{tag}] owner=character narrows the tab to the character's row, meta total 1"),
        ch_code == 200 && attr(&ch, "data-state") == "ok" && ch_rows == [char_row.clone()] && text(&ch).contains(&meta(1, &pid)),
        format!("status={ch_code} state={:?} rows={ch_rows:?} body={}", attr(&ch, "data-state"), cut(&ch)));

    // [IV3] owner=player narrows to the player's own row.
    let (pl_code, pl, _, pl_rows) = panel(admin, &format!("{front}{}&owner=player", want("inventory"))).await?;
    p.check(&format!("[IV3{tag}] owner=player narrows the tab to the player's own row, meta total 1"),
        pl_code == 200 && attr(&pl, "data-state") == "ok" && pl_rows == [player_row.clone()] && text(&pl).contains(&meta(1, &pid)),
        format!("status={pl_code} state={:?} rows={pl_rows:?} body={}", attr(&pl, "data-state"), cut(&pl)));

    // [IV4] the unfiltered tab count counts both holdings.
    let (count_code, count) = get(admin, &format!("{front}{}", want("tab.count"))).await?;
    p.check(&format!("[IV4{tag}] Inventory tab count is 2 (player and character holdings)"),
        count_code == 200 && attr(&count, "data-state") == "ok" && count.contains(r#"<span class="count">2</span>"#),
        format!("status={count_code} body={:?}", cut(&count)));
    Ok(())
}

/// The panel meta the tab draws over `n` holdings of player `pid`.
fn meta(n: u64, pid: &str) -> String {
    format!("{n} {} · owner player:{pid}", if n == 1 { "item" } else { "items" })
}

/// GET one Inventory tab panel: (status, body, header labels, rows as drawn text).
async fn panel(admin: &reqwest::Client, url: &str) -> Result<(u16, String, Vec<String>, Vec<Drawn>)> {
    let (code, body) = get(admin, url).await?;
    let (headers, rows) = table(&body);
    let rows = rows
        .iter()
        .map(|(_, c)| {
            let cell = |i: usize| c.get(i).map(|x| text(x)).unwrap_or_default();
            (cell(0), cell(1), cell(2), cell(3))
        })
        .collect();
    Ok((code, body, headers, rows))
}
