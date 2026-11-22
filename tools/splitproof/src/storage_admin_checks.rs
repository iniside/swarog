use anyhow::{bail, Context, Result};
use sqlx::PgPool;

use crate::characters_admin_checks::{attr, attr_values, cut, get, table, text};
use crate::{extract_form_fields, friends_call, friends_player, pass_nonce, Ctx, Proof};

/// The banner the Cloud saves tab draws, verbatim from `modules/storage/src/admin.rs`
/// `READONLY_TEXT` (the mockup's pages-core.js:139).
const READONLY_TEXT: &str =
    "Read-only: storage exposes saves for inspection and download; edits and deletes are not remotely supported.";

/// The storage provider's Cloud saves tab on the player entity page, its disabled downloads
/// and Delete, the row menu's "Download cloud saves" and the dashboard's "Cloud saves used"
/// tile, drawn and posted through the portal that `admin` holds a session on, over one
/// pass-minted player B who writes `saves/slot-1` through the public player API. In the split
/// the portal is admin-svc: the storage manifest (tab, actions, `implemented: false`), the tab
/// panel, the row menu group and the tile are all built in storage-svc and reach admin-svc only
/// through its `storage` peer.
///
/// "Storage purges pending" is not checked here: it shows only between a player's deletion and
/// the next purge sweep, so a portal read races the sweep; `modules/storage/src/admin_tests.rs`
/// proves it over the live tables instead.
///
/// Break run (a full splitproof run, then reverted):
/// - admin-svc without its `storage` peer (`cmd/admin-svc/src/lib.rs`): SA1-SA3 red as new in
///   the split, SA1m-SA3m green, 0 changed and 0 stale (2026-11-22, step 19t).
pub(super) async fn assertions(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    if let Err(e) = checks(ctx, pool, p, front, admin, tag).await {
        p.check(&format!("[SA0{tag}] storage admin pass completes"), false, format!("{e:#}"));
    }
    Ok(())
}

async fn checks(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    let nonce = pass_nonce(tag);
    let b = friends_player(ctx, front, &format!("sa-b-{nonce}@test.local"), &format!("SaB{nonce}")).await?;
    let (put_code, put_body) = friends_call(
        ctx,
        front,
        &b.token,
        reqwest::Method::POST,
        "/storage/saves/slot-1",
        Some(serde_json::json!({"value": "{\"level\":3}", "expected_version": 0, "acl": "private"})),
    )
    .await?;
    if put_code != 200 || put_body["version"] != 1 {
        bail!("fixture: {} putting saves/slot-1 -> {put_code} {put_body}", b.handle);
    }
    let version = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT version FROM storage.objects WHERE owner_id = $1::uuid AND collection = 'saves' AND key = 'slot-1'",
        )
        .bind(&b.player_id)
        .fetch_optional(pool)
        .await
        .context("read B's saves/slot-1")
    };
    if version().await? != Some(1) {
        bail!("fixture: {}'s saves/slot-1 is not stored at version 1", b.handle);
    }

    // [SA1] B's entity page draws the Cloud saves tab with its lock and slots the tab panel;
    // the panel is read-only with the banner and draws exactly B's one object: slot
    // saves/slot-1, version v1 and a size.
    let panel_url = format!("/admin/_panel/storage/saves?entity=player%3A{}", b.player_id);
    let (page_code, page) = get(admin, &format!("{front}/admin/e/player/{}?tab=saves", b.player_id)).await?;
    let tab = tab_button(&page, "Cloud saves");
    let tab_lock = tab.as_deref().is_some_and(|t| t.contains(r#"title="read-only""#));
    let slot = attr_values(&page, "hx-get").contains(&panel_url);
    let (s_code, s_body) = get(admin, &format!("{front}{panel_url}")).await?;
    let (headers, rows) = table(&s_body);
    let at = |label: &str| headers.iter().position(|h| h == label);
    let drawn: Vec<(String, String, String)> = match (at("Slot"), at("Size"), at("Version")) {
        (Some(si), Some(zi), Some(vi)) => rows
            .iter()
            .map(|(_, c)| {
                let cell = |i: usize| text(&c.get(i).cloned().unwrap_or_default());
                (cell(si), cell(zi), cell(vi))
            })
            .collect(),
        _ => Vec::new(),
    };
    let row_ok = matches!(drawn.as_slice(), [(s, z, v)] if s == "saves/slot-1" && !z.is_empty() && v == "v1");
    let banner = text(&s_body).contains(READONLY_TEXT);
    p.check(
        &format!("[SA1{tag}] Cloud saves tab draws locked and its read-only panel draws B's one slot saves/slot-1 at v1 with a size"),
        page_code == 200
            && tab_lock
            && slot
            && s_code == 200
            && attr(&s_body, "data-state") == "readonly"
            && banner
            && row_ok,
        format!(
            "page={page_code} tab={:?} lock={tab_lock} slot={slot} status={s_code} state={:?} banner={banner} headers={headers:?} rows={drawn:?} body={}",
            tab.as_deref().map(text),
            attr(&s_body, "data-state"),
            cut(&s_body)
        ),
    );

    // [SA2] every storage control draws disabled as not implemented: the panel's Download all
    // and Delete, the row's download and the row menu's "Download cloud saves". A direct POST
    // of the row download is refused by the portal's admission (403 "Not implemented yet", not
    // the provider's 404), and B's object is untouched.
    let refused = |html: &str| -> Vec<(String, String)> {
        attr_values(html, "data-refused-action").into_iter().zip(attr_values(html, "data-refusal")).collect()
    };
    let want = |id: &str| (format!("storage/{id}"), "not-implemented".to_string());
    let panel_refused = refused(&s_body);
    let panel_ok = ["saves.download-all", "saves.delete", "save.download"].iter().all(|id| panel_refused.contains(&want(id)));
    let (menu_code, menu) = get(admin, &format!("{front}/admin/_menu/player/{}", b.player_id)).await?;
    let menu_refused = refused(&menu);
    let menu_ok = menu_refused.contains(&want("player.download-saves")) && text(&menu).contains("Download cloud saves");
    let csrf = extract_form_fields(&page).into_iter().find(|(k, _)| k == "_csrf").map(|(_, v)| v).unwrap_or_default();
    if csrf.is_empty() {
        bail!("no _csrf on {}'s entity page: {}", b.handle, cut(&page));
    }
    let entity = format!("player:{}", b.player_id);
    let r = admin
        .post(format!("{front}/admin/_action/storage/save.download"))
        .form(&[("_csrf", csrf.as_str()), ("_entity", entity.as_str()), ("_item", "saves/slot-1")])
        .send()
        .await?;
    let post_code = r.status().as_u16();
    let post_text = text(&r.text().await.unwrap_or_default());
    let after = version().await?;
    p.check(
        &format!("[SA2{tag}] storage downloads, Delete and the row menu item draw not-implemented and a direct download POST is refused by admission"),
        s_code == 200
            && panel_ok
            && menu_code == 200
            && menu_ok
            && post_code == 403
            && post_text.contains("Not implemented yet")
            && after == Some(1),
        format!(
            "panel_refused={panel_refused:?} menu={menu_code} menu_refused={menu_refused:?} post={post_code} post_text={post_text:?} version_after={after:?}"
        ),
    );

    // [SA3] the dashboard slots the "Cloud saves used" tile, whose body draws the label and a
    // value with a non-zero leading digit (B's object is stored, so the sum is non-zero). The
    // tile sums every owner and is cached 300 s, so no exact value or lower bound is asserted.
    let (d_code, dash) = get(admin, &format!("{front}/admin")).await?;
    let tile_url = attr_values(&dash, "hx-get").into_iter().find(|u| u.starts_with("/admin/_panel/storage/tile.used"));
    let (t_code, tile) = match &tile_url {
        Some(u) => get(admin, &format!("{front}{u}")).await?,
        None => (0, String::new()),
    };
    let value = kpi_value(&tile);
    let value_ok = value.as_deref().is_some_and(|v| v.chars().next().is_some_and(|c| ('1'..='9').contains(&c)));
    p.check(
        &format!("[SA3{tag}] dashboard draws the Cloud saves used tile with a non-zero byte sum"),
        d_code == 200
            && t_code == 200
            && attr(&tile, "data-state") == "ok"
            && text(&tile).contains("Cloud saves used")
            && value_ok,
        format!(
            "dashboard={d_code} tile_url={tile_url:?} tile={t_code} state={:?} value={value:?} body={}",
            attr(&tile, "data-state"),
            cut(&tile)
        ),
    );
    Ok(())
}

/// The raw html of the entity page's tab button whose label starts with `label`, none when
/// the tab bar draws no such tab.
fn tab_button(html: &str, label: &str) -> Option<String> {
    let open = r#"<button type="button" class="tab"#;
    html.match_indices(open).find_map(|(at, _)| {
        let rest = &html[at..];
        let end = rest.find("</button>")?;
        let button = &rest[..end];
        text(button).starts_with(label).then(|| button.to_string())
    })
}

/// A tile body's value text (`<div class="kpi-value">`, its unit included), none when the
/// body draws no value.
fn kpi_value(html: &str) -> Option<String> {
    let open = r#"<div class="kpi-value">"#;
    let start = html.find(open)? + open.len();
    let len = html[start..].find("</div>")?;
    Some(text(&html[start..start + len]))
}
