use anyhow::{bail, Context, Result};
use sqlx::PgPool;

use crate::apikeys_admin_checks::{fetch, patient};
use crate::characters_admin_checks::{attr, attr_values, cut, table, text};
use crate::{extract_form_fields, pass_nonce, Ctx, Proof};

/// The page the scheduler provider declares (`modules/scheduler/src/admin.rs` `PAGE`).
const PAGE_URL: &str = "/admin/p/scheduler/schedules";

/// The seeded schedule's interval: 2 h, so the running scheduler never finds it due during the
/// pass and `last_fired` stays where the fixture put it; it draws as "every 2 h".
///
/// Every request goes through `fetch`/`patient`: this pass follows the apikeys pass on the same
/// 127.0.0.1 bucket of the gateway's always-on rate limit, whose 429 answers before the portal.
const INTERVAL_SECONDS: i64 = 7200;

/// The scheduler provider's Schedules page and its disabled Fire now action, drawn and posted
/// through the portal that `admin` holds a session on, over one pass-named schedule written
/// straight into `scheduler.schedules` (the scheduler has no write path; [SC0] seeds the same
/// way). In the split the portal is admin-svc: the scheduler manifest (nav, page, the
/// `implemented: false` action, no dashboard contribution), both page panels and the row menu
/// are built in scheduler-svc and reach admin-svc only through its `scheduler` peer.
///
/// Break run (a full splitproof run, then reverted):
/// - admin-svc without its `scheduler` peer (`cmd/admin-svc/src/lib.rs`): SH1-SH3 red as new in
///   the split, SH1m-SH3m green, 0 changed and 0 stale (2026-11-23, step 22t). Only SH1 was red
///   for the missing peer (page and Schedules panel 404); that run predated the 429 retry, so
///   SH2 and SH3 were red on the gateway's 429, not observed against the missing peer.
pub(super) async fn assertions(_ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    let name = format!("proof-sh-{}", pass_nonce(tag));
    if let Err(e) = checks(pool, p, front, admin, tag, &name).await {
        p.check(&format!("[SH0{tag}] scheduler admin pass completes"), false, format!("{e:#}"));
    }
    // On every exit of `checks`, Ok or Err, so a pass that stopped short leaves no schedule
    // in the reused DB for the live scheduler to fire.
    cleanup(pool, &name, tag).await;
    Ok(())
}

/// Deletes the pass's schedule and says how many rows it removed or why it could not.
async fn cleanup(pool: &PgPool, name: &str, tag: &str) {
    let done = sqlx::query("DELETE FROM scheduler.schedules WHERE name = $1").bind(name).execute(pool).await;
    let said = match done {
        Ok(done) => format!("{} removed", done.rows_affected()),
        Err(e) => format!("not removed: {e}"),
    };
    println!("[splitproof] scheduler pass{tag} cleanup: schedules {said}");
}

async fn checks(
    pool: &PgPool,
    p: &mut Proof,
    front: &str,
    admin: &reqwest::Client,
    tag: &str,
    name: &str,
) -> Result<()> {
    sqlx::query("INSERT INTO scheduler.schedules (name, interval_seconds, last_fired) VALUES ($1, $2, now())")
        .bind(name)
        .bind(INTERVAL_SECONDS)
        .execute(pool)
        .await
        .with_context(|| format!("fixture: insert schedule {name}"))?;
    let stored = || async {
        sqlx::query_as::<_, (i64, String)>("SELECT interval_seconds, last_fired::text FROM scheduler.schedules WHERE name = $1")
            .bind(name)
            .fetch_optional(pool)
            .await
            .with_context(|| format!("read schedule {name}"))
    };
    let before = stored().await?;
    if before.as_ref().map(|(i, _)| *i) != Some(INTERVAL_SECONDS) {
        bail!("fixture: schedule {name} is not stored at {INTERVAL_SECONDS} s: {before:?}");
    }

    // [SH1] the Scheduler page slots its Schedules and Recent fires panels; the Schedules panel
    // draws the seeded schedule's row with its name in Id and "every 2 h" in When, and Recent
    // fires lists it (its last_fired is within 24 h).
    let schedules_url = "/admin/_panel/scheduler/schedules";
    let fires_url = "/admin/_panel/scheduler/fires";
    let (page_code, page) = fetch(admin, &format!("{front}{PAGE_URL}")).await?;
    let slots = attr_values(&page, "hx-get");
    let slotted = slots.iter().any(|u| u == schedules_url) && slots.iter().any(|u| u == fires_url);
    let (s_code, s_body) = fetch(admin, &format!("{front}{schedules_url}")).await?;
    let (headers, rows) = table(&s_body);
    let at = |label: &str| headers.iter().position(|h| h == label);
    let drawn: Vec<(String, String)> = match (at("Id"), at("When")) {
        (Some(ii), Some(wi)) => rows
            .iter()
            .map(|(_, c)| {
                let cell = |i: usize| text(&c.get(i).cloned().unwrap_or_default());
                (cell(ii), cell(wi))
            })
            .filter(|(id, _)| id == name)
            .collect(),
        _ => Vec::new(),
    };
    let row_ok = matches!(drawn.as_slice(), [(_, when)] if when == "every 2 h");
    let (f_code, f_body) = fetch(admin, &format!("{front}{fires_url}")).await?;
    let fired = text(&f_body).contains(name);
    p.check(
        &format!("[SH1{tag}] Scheduler page slots both panels and Schedules draws the seeded schedule every 2 h, listed in Recent fires"),
        page_code == 200
            && slotted
            && s_code == 200
            && attr(&s_body, "data-state") == "ok"
            && row_ok
            && f_code == 200
            && fired,
        format!(
            "page={page_code} slotted={slotted} status={s_code} state={:?} headers={headers:?} rows={drawn:?} fires={f_code} fired={fired} body={}",
            attr(&s_body, "data-state"),
            cut(&s_body)
        ),
    );

    // [SH2] the only schedule control is the row menu's Fire now, drawn disabled as not
    // implemented; the panel draws no + Schedule tool and the menu no Edit or Delete. A direct
    // POST of schedule.fire is refused by the portal's admission (403 "Not implemented yet", not
    // the provider's 404), and the seeded row's interval and last_fired are untouched.
    let refused = |html: &str| -> Vec<(String, String)> {
        attr_values(html, "data-refused-action").into_iter().zip(attr_values(html, "data-refusal")).collect()
    };
    let want = |id: &str| (format!("scheduler/{id}"), "not-implemented".to_string());
    let removed = ["schedule.create", "schedule.edit", "schedule.delete"];
    let panel_refused = refused(&s_body);
    let panel_ok = panel_refused.is_empty();
    let (menu_code, menu) = fetch(admin, &format!("{front}/admin/_menu/schedule/{name}")).await?;
    let menu_refused = refused(&menu);
    let menu_ok = menu_refused == [want("schedule.fire")]
        && text(&menu).contains("Fire now")
        && removed.iter().all(|id| !menu.contains(id) && !s_body.contains(id));
    let csrf = extract_form_fields(&page).into_iter().find(|(k, _)| k == "_csrf").map(|(_, v)| v).unwrap_or_default();
    if csrf.is_empty() {
        bail!("no _csrf on the Scheduler page: {}", cut(&page));
    }
    let entity = format!("schedule:{name}");
    let fire_url = format!("{front}/admin/_action/scheduler/schedule.fire");
    let r = patient(|| admin.post(&fire_url).form(&[("_csrf", csrf.as_str()), ("_entity", entity.as_str())])).await?;
    let post_code = r.status().as_u16();
    let post_text = text(&r.text().await.unwrap_or_default());
    let after = stored().await?;
    p.check(
        &format!("[SH2{tag}] the row menu draws only a not-implemented Fire now, no Schedule tool, Edit or Delete, and a direct Fire now POST is refused by admission"),
        s_code == 200
            && panel_ok
            && menu_code == 200
            && menu_ok
            && post_code == 403
            && post_text.contains("Not implemented yet")
            && after == before,
        format!(
            "panel_refused={panel_refused:?} menu={menu_code} menu_refused={menu_refused:?} post={post_code} post_text={post_text:?} before={before:?} after={after:?}"
        ),
    );

    // [SH3] the dashboard slots no scheduler contribution: the scheduler declares no attention
    // item, so no panel under /admin/_panel/scheduler/attention is fetched. The portal's own
    // Service health slot must be among the parsed slots, so an unparsed dashboard is red.
    let (d_code, dash) = fetch(admin, &format!("{front}/admin")).await?;
    let dash_slots = attr_values(&dash, "hx-get");
    let scheduler_slot = dash_slots.iter().any(|u| u.starts_with("/admin/_panel/scheduler/"));
    let portal_slot = dash_slots.iter().find(|u| u.starts_with("/admin/_panel/portal/health"));
    p.check(
        &format!("[SH3{tag}] dashboard slots nothing from the scheduler provider"),
        d_code == 200 && portal_slot.is_some() && !scheduler_slot,
        format!("dashboard={d_code} portal_slot={portal_slot:?} scheduler_slot={scheduler_slot} slots={dash_slots:?}"),
    );
    Ok(())
}
