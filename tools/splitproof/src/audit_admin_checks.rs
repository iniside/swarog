use anyhow::Result;
use sqlx::PgPool;

use crate::apikeys_admin_checks::fetch;
use crate::characters_admin_checks::{attr, attr_values, cut, table, text};
use crate::{Ctx, Proof};

/// The page the audit provider declares (`modules/audit/src/admin.rs` `PAGE`).
const PAGE_URL: &str = "/admin/p/audit/log";

/// The page's one table and the dashboard's activity panel (`modules/audit/src/admin.rs`
/// `LOG`, `ACTIVITY`).
const LOG_PANEL: &str = "/admin/_panel/audit/log";
const ACTIVITY_PANEL: &str = "/admin/_panel/audit/activity";

/// The dashboard panel's title, verbatim from `modules/audit/src/admin.rs` `ACTIVITY_TITLE`.
const ACTIVITY_TITLE: &str = "Recent operator activity";

/// The operator both sessions log in as: [AD6a] in the split, the monolith's `jar` login.
const LOGIN: &str = "proofadmin";

/// A system topic every run writes ([AU1]); its rows' actor is `system`, never an operator.
const SYSTEM_TOPIC: &str = "character.created";

/// The audit provider's Audit log page and the dashboard's "Recent operator activity" panel,
/// drawn through the portal that `admin` holds a `proofadmin` session on. In the split the
/// portal is admin-svc: the audit manifest (nav, page, the two contributions) and both panels
/// are built in audit-svc and reach admin-svc only through its `audit` peer.
///
/// Every request goes through `fetch`: this pass follows the scheduler pass on the same
/// 127.0.0.1 bucket of the gateway's always-on rate limit, whose 429 answers before the portal.
///
/// Break run (a full splitproof run, then reverted):
/// - admin-svc without its `audit` peer (`cmd/admin-svc/src/lib.rs`): AU3-AU5 red as new in
///   the split, AU3m-AU5m green, 0 changed and 0 stale (2026-11-24, step 24t). With no audit
///   manifest the page answers 404 (AU3, AU4: no slot) and the dashboard stays 200 but slots
///   no audit panel at all (AU5: no slot), so neither draws an error block.
pub(super) async fn assertions(_ctx: &Ctx, _pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    if let Err(e) = checks(p, front, admin, tag).await {
        p.check(&format!("[AU0{tag}] audit admin pass completes"), false, format!("{e:#}"));
    }
    Ok(())
}

/// One drawn Audit log panel: the status of the page that slots it, the slot URL the page
/// names, the panel's status, `data-state`, header labels, rows (actor cell's links, topic
/// cell's text) and the foot's total.
struct LogPanel {
    page_code: u16,
    slot: Option<String>,
    code: u16,
    state: String,
    headers: Vec<String>,
    rows: Vec<(Vec<String>, String)>,
    total: Option<u64>,
    body: String,
}

impl LogPanel {
    fn detail(&self) -> String {
        format!(
            "page={} slot={:?} panel={} state={:?} headers={:?} total={:?} rows={:?} body={}",
            self.page_code,
            self.slot,
            self.code,
            self.state,
            self.headers,
            self.total,
            self.rows.iter().take(5).collect::<Vec<_>>(),
            cut(&self.body)
        )
    }
}

/// GETs the page with `query`, then the log panel it slots, as the browser does.
async fn log_panel(admin: &reqwest::Client, front: &str, query: &str) -> Result<LogPanel> {
    let (page_code, page) = fetch(admin, &format!("{front}{PAGE_URL}?{query}")).await?;
    let slot = attr_values(&page, "hx-get").into_iter().find(|u| u.starts_with(LOG_PANEL));
    let (code, body) = match &slot {
        Some(u) => fetch(admin, &format!("{front}{u}")).await?,
        None => (0, String::new()),
    };
    let (headers, raw) = table(&body);
    let at = |label: &str| headers.iter().position(|h| h == label);
    let rows = match (at("Actor"), at("Topic")) {
        (Some(ai), Some(ti)) => raw
            .iter()
            .map(|(_, c)| {
                let cell = |i: usize| c.get(i).cloned().unwrap_or_default();
                (attr_values(&cell(ai), "href"), text(&cell(ti)))
            })
            .collect(),
        _ => Vec::new(),
    };
    Ok(LogPanel { page_code, slot, code, state: attr(&body, "data-state"), headers, rows, total: foot_total(&body), body })
}

/// The leading number of the table foot's "N entries · showing a–b", if it draws one.
fn foot_total(html: &str) -> Option<u64> {
    let foot = &html[html.find("class=\"tbl-foot\"")?..];
    let start = foot.find("<span>")? + "<span>".len();
    let end = foot[start..].find("</span>")?;
    text(&foot[start..start + end]).split_whitespace().next()?.replace(',', "").parse().ok()
}

async fn checks(p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    // Where an operator's actor cell links: the page narrowed to that login
    // (`modules/audit/src/admin.rs` `actor_link`).
    let actor_href = format!("{PAGE_URL}?actor={LOGIN}");

    // [AU3] the Audit log page slots its table and the table draws ledger rows. Narrowed to
    // the system topic so a written row is on the first page whatever the run logged after
    // it (page 1 is the newest 25 rows): every drawn row is that topic and none is an
    // operator's.
    let au3 = log_panel(admin, front, &format!("topic={SYSTEM_TOPIC}")).await?;
    let au3_rows_ok = !au3.rows.is_empty() && au3.rows.iter().all(|(links, topic)| topic == SYSTEM_TOPIC && links.is_empty());
    p.check(
        &format!("[AU3{tag}] Audit log page draws {SYSTEM_TOPIC} ledger rows from the audit provider"),
        au3.page_code == 200
            && au3.slot.as_deref().is_some_and(|u| u.contains(&format!("topic={SYSTEM_TOPIC}")))
            && au3.code == 200
            && au3.state == "ok"
            && au3_rows_ok,
        au3.detail(),
    );

    // [AU4] X5 actor filter: `?actor=proofadmin` draws only proofadmin's rows (each actor
    // cell links to the page narrowed to proofadmin, each topic is admin.action) and a total
    // of at least one. The same filter over the system topic draws the empty state: AU3 has
    // just drawn rows of that topic, so a page that ignored the actor filter draws them here.
    let mine = log_panel(admin, front, &format!("actor={LOGIN}")).await?;
    let mine_ok = !mine.rows.is_empty()
        && mine.rows.iter().all(|(links, topic)| topic == "admin.action" && links.iter().any(|h| *h == actor_href));
    let none = log_panel(admin, front, &format!("actor={LOGIN}&topic={SYSTEM_TOPIC}")).await?;
    p.check(
        &format!("[AU4{tag}] Audit log actor filter draws only {LOGIN}'s rows, and none of the {SYSTEM_TOPIC} rows AU3 drew"),
        mine.page_code == 200
            && mine.slot.as_deref().is_some_and(|u| u.contains(&format!("actor={LOGIN}")))
            && mine.code == 200
            && mine.state == "ok"
            && mine_ok
            && mine.total.is_some_and(|t| t >= 1)
            && au3_rows_ok
            && none.code == 200
            && none.state == "empty"
            && none.rows.is_empty(),
        format!("mine: {} | none: {}", mine.detail(), none.detail()),
    );

    // [AU5] the dashboard slots the audit activity panel; fetched as the browser fetches it,
    // it is ok, titled "Recent operator activity", draws an entry whose actor links to the
    // page narrowed to proofadmin and carries the "Audit log" tool linking to the page.
    let (d_code, dash) = fetch(admin, &format!("{front}/admin")).await?;
    let slot = attr_values(&dash, "hx-get").into_iter().find(|u| u.starts_with(ACTIVITY_PANEL));
    let (a_code, panel) = match &slot {
        Some(u) => fetch(admin, &format!("{front}{u}")).await?,
        None => (0, String::new()),
    };
    let hrefs = attr_values(&panel, "href");
    let entry = hrefs.iter().any(|h| *h == actor_href);
    let tool = hrefs.iter().any(|h| h == PAGE_URL);
    p.check(
        &format!("[AU5{tag}] dashboard draws Recent operator activity from the audit provider with a {LOGIN} entry"),
        d_code == 200
            && a_code == 200
            && attr(&panel, "data-state") == "ok"
            && text(&panel).contains(ACTIVITY_TITLE)
            && entry
            && tool,
        format!(
            "dashboard={d_code} slot={slot:?} panel={a_code} state={:?} entry={entry} tool={tool} hrefs={hrefs:?} body={}",
            attr(&panel, "data-state"),
            cut(&panel)
        ),
    );
    Ok(())
}
