use anyhow::{bail, Context, Result};
use sqlx::PgPool;

use crate::characters_admin_checks::{attr, attr_values, cut, get, table, text};
use crate::{create_group, extract_form_fields, friends_call, friends_player, membership_row, pass_nonce, Ctx, Proof};

/// One drawn Groups row: (group cell raw html, role cell raw html, members text, every
/// `hx-get` the row carries).
type Drawn = (String, String, String, Vec<String>);

/// One drawn row for a failure detail: (group text, href, role text, members, hx-gets).
type Shown = (String, Vec<String>, String, String, Vec<String>);

/// The groups provider's Groups tab on the player entity page, its count, the `group` entity
/// page's header and the operator's `membership.promote`, drawn and posted through the portal
/// that `admin` holds a session on, over two pass-minted players: A creates the open group G
/// and the request-policy group R, B joins G (a member row) and asks to join R (a requested
/// row). In the split the portal is admin-svc: the tab panel, its count, the group kind and its
/// header, the promote's prepared modal (whose body names B through groups-svc's `accounts`
/// directory) and the invoke itself are all built in groups-svc and reach admin-svc only
/// through its `groups` peer. The memberships are made through the public player API, as the
/// groups pass ([GR1]/[GR2]) makes them.
///
/// Break run (a full splitproof run, then reverted):
/// - admin-svc without its `groups` peer (`cmd/admin-svc/src/lib.rs`): GA1-GA4 red as new in the
///   split (the tab slot is not drawn, the panel, count, promote modal and group header URLs
///   answer 404 and the group page draws its error card), GA1m-GA4m green, 0 changed and 0 stale
///   (2026-11-22, step 18t).
pub(super) async fn assertions(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    if let Err(e) = checks(ctx, pool, p, front, admin, tag).await {
        p.check(&format!("[GA0{tag}] groups admin pass completes"), false, format!("{e:#}"));
    }
    Ok(())
}

async fn checks(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    let nonce = pass_nonce(tag);
    let a = friends_player(ctx, front, &format!("ga-a-{nonce}@test.local"), &format!("GaA{nonce}")).await?;
    let b = friends_player(ctx, front, &format!("ga-b-{nonce}@test.local"), &format!("GaB{nonce}")).await?;
    let g_name = format!("GaOpen{nonce}");
    let r_name = format!("GaReq{nonce}");
    let (g_code, g) = create_group(ctx, front, &a.token, &g_name, "open").await?;
    let (r_code, r) = create_group(ctx, front, &a.token, &r_name, "request").await?;
    if g_code != 201 || g.is_empty() || r_code != 201 || r.is_empty() {
        bail!("fixture: creating {g_name} -> {g_code} {g:?}, {r_name} -> {r_code} {r:?}");
    }
    for group in [&g, &r] {
        let (code, body) = friends_call(
            ctx,
            front,
            &b.token,
            reqwest::Method::POST,
            &format!("/groups/{group}/join"),
            Some(serde_json::json!({})),
        )
        .await?;
        if !(200..300).contains(&code) {
            bail!("fixture: {} joining {group} -> {code} {body}", b.handle);
        }
    }
    // The rows as the DB holds them: the tab must draw exactly these two for B.
    for (group, want) in [(&g, ("member", "member")), (&r, ("requested", ""))] {
        let row = membership_row(pool, group, &b.player_id).await;
        let ok = match &row {
            Some((state, role)) => state == want.0 && (want.1.is_empty() || role == want.1),
            None => false,
        };
        if !ok {
            bail!("fixture: {}'s row in {group} is {row:?}, expected state {}", b.handle, want.0);
        }
    }

    // [GA1] B's entity page slots the Groups tab panel and its count; the panel draws G's row
    // with the name linked to the group's entity page, role member, 2 members and the Promote
    // row action bound to player B and item G.
    let want = |kind: &str| format!("/admin/_panel/groups/{kind}?entity=player%3A{}", b.player_id);
    let promote_url = format!("/admin/_action/groups/membership.promote?entity=player%3A{}&item={g}", b.player_id);
    let (page_code, page) = get(admin, &format!("{front}/admin/e/player/{}?tab=groups", b.player_id)).await?;
    let hx = attr_values(&page, "hx-get");
    let slot = hx.iter().any(|u| *u == want("groups"));
    let count_slot = hx.iter().any(|u| *u == want("tab.count"));
    let (b_code, b_body, b_rows) = panel(admin, &format!("{front}{}", want("groups"))).await?;
    let g_row = b_rows.iter().find(|row| text(&row.0) == g_name);
    let g_ok = g_row.is_some_and(|(group, role, members, acts)| {
        attr_values(group, "href") == [format!("/admin/e/group/{g}")]
            && text(role) == "member"
            && members == "2"
            && acts.contains(&promote_url)
    });
    p.check(
        &format!("[GA1{tag}] Groups tab draws the member row linked to the group page with role member, 2 members and Promote"),
        page_code == 200 && slot && count_slot && b_code == 200 && attr(&b_body, "data-state") == "ok" && g_ok,
        format!(
            "page={page_code} slot={slot} count_slot={count_slot} status={b_code} state={:?} rows={:?} want={g_name} {g} promote={promote_url} body={}",
            attr(&b_body, "data-state"),
            shown(&b_rows),
            cut(&b_body)
        ),
    );

    // [GA2] a real operator promote of B in G on the two-step form path: the prepared modal
    // names B's handle and G's name, the post makes the row admin with exactly one
    // group.role_changed, and a repeat through a fresh form answers NoOp with no second event.
    // A failed step records GA2 red and the pass goes on, so GA3 and GA4 still report.
    let modal_body = format!("Promote {} to admin of {g_name}?", b.handle);
    let ok_toast = format!("{} is now an admin of {g_name}", b.handle);
    let noop_toast = format!("{} was already an admin of {g_name}", b.handle);
    let ga2 = async {
        let (first_modal, first) = promote(admin, front, &promote_url).await?;
        let promoted = membership_row(pool, &g, &b.player_id).await;
        // (role_changed rows naming the operator promote of B to admin in G, all role_changed
        // rows in G): the first must be exactly 1; the second says why a mismatch occurred.
        let events_sql = "SELECT count(*) FILTER (WHERE payload->>'player_id' = $2 AND payload->>'role' = 'admin' \
                          AND payload->>'actor_kind' = 'operator'), count(*) \
                          FROM asyncevents.events WHERE topic = 'group.role_changed' AND payload->>'group_id' = $1";
        let events_after_first: (i64, i64) =
            sqlx::query_as(events_sql).bind(&g).bind(&b.player_id).fetch_one(pool).await.context("count role_changed")?;
        let (second_modal, second) = promote(admin, front, &promote_url).await?;
        let events_after_repeat: (i64, i64) =
            sqlx::query_as(events_sql).bind(&g).bind(&b.player_id).fetch_one(pool).await.context("count role_changed")?;
        let ok = first_modal == modal_body
            && second_modal == modal_body
            && first.0 == 303
            && first.1 == ok_toast
            && promoted.as_ref().is_some_and(|(s, r)| s == "member" && r == "admin")
            && events_after_first == (1, 1)
            && second.0 == 303
            && second.1 == noop_toast
            && events_after_repeat == (1, 1);
        let detail = format!(
            "modal={first_modal:?} repeat_modal={second_modal:?} want_modal={modal_body:?} post={} toast={:?} want={ok_toast:?} \
             row={promoted:?} events(promote,all)={events_after_first:?} repeat_post={} repeat_toast={:?} want={noop_toast:?} \
             events_after_repeat(promote,all)={events_after_repeat:?}",
            first.0, first.1, second.0, second.1
        );
        anyhow::Ok((ok, detail))
    }
    .await;
    let (ga2_ok, ga2_detail) = ga2.unwrap_or_else(|e| (false, format!("{e:#}")));
    p.check(
        &format!("[GA2{tag}] portal promote names the player and group, makes the row admin with one role_changed, and a repeat is NoOp"),
        ga2_ok,
        ga2_detail,
    );

    // [GA3] the group entity page answers and its header slot draws the group's name.
    let header_url = format!("/admin/_panel/groups/group.header?entity=group%3A{g}");
    let (gp_code, gp) = get(admin, &format!("{front}/admin/e/group/{g}")).await?;
    let header_slot = attr_values(&gp, "hx-get").contains(&header_url);
    let (h_code, h) = get(admin, &format!("{front}{header_url}")).await?;
    let error_card = |html: &str| html.contains(r#"class="state err"#);
    p.check(
        &format!("[GA3{tag}] group entity page answers 200 and its header draws the group's name"),
        gp_code == 200
            && !error_card(&gp)
            && header_slot
            && h_code == 200
            && attr(&h, "data-state") == "ok"
            && !error_card(&h)
            && text(&h).contains(&g_name),
        format!(
            "page={gp_code} page_error={} header_slot={header_slot} header={h_code} header_state={:?} header_error={} want={g_name} body={}",
            error_card(&gp),
            attr(&h, "data-state"),
            error_card(&h),
            cut(&h)
        ),
    );

    // [GA4] every state counts: the tab count and the meta are 2 (the member row and the
    // requested row), the panel draws one page with no next cursor, and the requested row
    // shows its state as a label and offers no Promote.
    let (count_code, count) = get(admin, &format!("{front}{}", want("tab.count"))).await?;
    let b_meta = meta(&b_body);
    let r_row = b_rows.iter().find(|row| text(&row.0) == r_name);
    let r_ok = r_row.is_some_and(|(_, role, _, acts)| {
        role.contains(r#"class="badge plain"#) && text(role) == "requested" && !acts.iter().any(|u| u.contains("membership.promote"))
    });
    let next_page = attr_values(&b_body, "hx-get").iter().any(|u| u.contains("cursor="));
    p.check(
        &format!("[GA4{tag}] Groups tab count and meta are 2 over a member and a requested row, the requested row a label with no Promote"),
        count_code == 200
            && attr(&count, "data-state") == "ok"
            && count.contains(r#"<span class="count">2</span>"#)
            && b_meta.as_deref() == Some("2")
            && b_rows.len() == 2
            && !next_page
            && r_ok,
        format!(
            "count_status={count_code} count_body={:?} meta={b_meta:?} rows={:?} next_page={next_page} want_pending={r_name}",
            cut(&count),
            shown(&b_rows)
        ),
    );
    Ok(())
}

/// One operator promote as the browser posts it: GET the modal at `url` (the portal prepares
/// its body through the provider), read its `_csrf` and `_form`, POST them, then follow the
/// PRG's `Location` and read the flash toast. Answers (modal body text, (post status, toast
/// text)); the toast is empty when the post did not redirect.
async fn promote(admin: &reqwest::Client, front: &str, url: &str) -> Result<(String, (u16, String))> {
    let (code, modal) = get(admin, &format!("{front}{url}")).await?;
    let fields = extract_form_fields(&modal);
    let pick = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).unwrap_or_default();
    let (csrf, form_id) = (pick("_csrf"), pick("_form"));
    if code != 200 || csrf.is_empty() || form_id.is_empty() {
        bail!("open the promote modal: status {code} csrf={} form={}: {}", !csrf.is_empty(), !form_id.is_empty(), cut(&modal));
    }
    let body = modal_body(&modal);
    let r = admin
        .post(format!("{front}{}", url.split('?').next().unwrap_or(url)))
        .form(&[("_csrf", csrf.as_str()), ("_form", form_id.as_str())])
        .send()
        .await?;
    let status = r.status().as_u16();
    let location = r.headers().get("location").and_then(|v| v.to_str().ok()).map(str::to_string);
    let posted = r.text().await.unwrap_or_default();
    let Some(location) = location else {
        return Ok((body, (status, format!("no redirect: {}", cut(&text(&posted))))));
    };
    let (_, landed) = get(admin, &format!("{front}{location}")).await?;
    Ok((body, (status, toast(&landed))))
}

/// The confirm modal's body text: the `modal-body` div up to the reason input's block.
fn modal_body(html: &str) -> String {
    let open = r#"<div class="modal-body">"#;
    let Some(at) = html.find(open) else { return String::new() };
    let rest = &html[at + open.len()..];
    let end = rest.find("<div").unwrap_or(rest.len());
    text(&rest[..end])
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

/// The drawn rows without their raw html, for a failure detail.
fn shown(rows: &[Drawn]) -> Vec<Shown> {
    rows.iter()
        .map(|(group, role, members, acts)| (text(group), attr_values(group, "href"), text(role), members.clone(), acts.clone()))
        .collect()
}

/// GET one Groups tab panel: (status, body, rows). The Group, Role and Members cells are found
/// by their header labels; a row's `hx-get` values are read over all its cells, so the
/// Promote button is found wherever the actions column sits.
async fn panel(admin: &reqwest::Client, url: &str) -> Result<(u16, String, Vec<Drawn>)> {
    let (code, body) = get(admin, url).await?;
    let (headers, rows) = table(&body);
    let at = |label: &str| headers.iter().position(|h| h == label);
    let rows = match (at("Group"), at("Role"), at("Members")) {
        (Some(gi), Some(ri), Some(mi)) => rows
            .iter()
            .map(|(_, c)| {
                let cell = |i: usize| c.get(i).cloned().unwrap_or_default();
                let acts = c.iter().flat_map(|x| attr_values(x, "hx-get")).collect();
                (cell(gi), cell(ri), text(&cell(mi)), acts)
            })
            .collect(),
        _ => Vec::new(),
    };
    Ok((code, body, rows))
}
