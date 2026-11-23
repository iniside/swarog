use std::future::Future;
use std::pin::Pin;

use anyhow::{bail, Context, Result};
use sqlx::PgPool;

use crate::characters_admin_checks::{attr, attr_values, cut, get, text};
use crate::{extract_form_fields, grant_target, pass_nonce, Ctx, Proof};

/// The NA1-NA3 check names after their `[NA<n><tag>]` ids.
const NAMES: [&str; 3] = [
    "Notify form sends one operator mail to B and the Inbox tab draws it, count 1",
    "a re-POST of the Notify form instance, also past a released claim, leaves B one row",
    "the row menu's send and the Inbox panel tool open the same Send notification form as Notify",
];

/// The provider's Ok text, verbatim from `modules/notifications/src/admin.rs` `SENT_MESSAGE`.
const SENT: &str = "Notification sent";

/// The notifications provider's operator send and Inbox tab, drawn and posted through the
/// portal that `admin` holds a session on, over one fresh guest B (a real player with an empty
/// inbox: a guest gets no starter grant and no welcome row, so the sends here are its only
/// rows). In the split the portal is admin-svc: the send form, the invoke that writes the row,
/// the Inbox timeline and the tab count are all served by notifications-svc and reach
/// admin-svc only through its `notifications` peer.
///
/// NA2 proves the provider's idempotency key, not only the portal's: a re-POST of a consumed
/// form instance is answered by the portal with zero calls, so the check then releases the
/// instance's claim in `admin.forms` (the state a timed-out invoke leaves) and posts it again,
/// which reaches the provider with the same request id.
///
/// The /admin/inbox operator mail of [NT2] is a separate path and is not checked here.
///
/// Break runs (a full splitproof run each, then reverted):
/// - admin-svc without its `notifications` peer (`cmd/admin-svc/src/lib.rs`): NA1-NA3 red as
///   new in the split (no Notify action on B's page), NA1m-NA3m green, 0 changed and 0 stale
///   (2026-11-23, step 20t).
/// - `operator_dedup_key` salted with the clock (`modules/notifications/src/admin.rs`): NA2
///   and NA2m red as new, two rows after the released re-POST and the edited post a 303, NA1
///   and NA3 green (2026-11-23, step 20t).
///
/// The pass is built and boxed here, out of line, so the caller's frame holds only the box: a
/// `Box::pin` at the call site still sizes the caller's frame for the whole future, which
/// overflowed the main thread's stack at [PH7] in the break run above.
#[inline(never)]
pub(super) fn assertions<'a>(
    ctx: &'a Ctx,
    pool: &'a PgPool,
    p: &'a mut Proof,
    front: &'a str,
    admin: &'a reqwest::Client,
    tag: &'a str,
) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
    Box::pin(async move {
        if let Err(e) = checks(ctx, pool, p, front, admin, tag).await {
            p.check(&format!("[NA0{tag}] notifications admin pass completes"), false, format!("{e:#}"));
        }
        Ok(())
    })
}

async fn checks(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    let nonce = pass_nonce(tag);
    let b = grant_target(ctx, front).await?;
    let title = format!("Splitproof notice {nonce}");
    let body = format!("Operator mail to B, pass {nonce}");
    // (B's rows, B's unread operator.mail rows titled `title` with `body`).
    let rows = |body: String| {
        let (b, title) = (b.clone(), title.clone());
        async move {
            sqlx::query_as::<_, (i64, i64)>(
                "SELECT count(*), count(*) FILTER (WHERE kind = 'operator.mail' AND title = $2 AND body = $3 \
                 AND read_at IS NULL) FROM notifications.messages WHERE player_id = $1::uuid",
            )
            .bind(&b)
            .bind(&title)
            .bind(&body)
            .fetch_one(pool)
            .await
            .context("count B's inbox rows")
        }
    };
    if rows(body.clone()).await? != (0, 0) {
        bail!("fixture: guest {b} does not start with an empty inbox");
    }
    let entity = format!("player%3A{b}");
    let panel_url = format!("/admin/_panel/notifications/inbox?entity={entity}");
    let count_url = format!("/admin/_panel/notifications/inbox.count?entity={entity}");

    // [NA1] B's entity page draws the Notify head action; its form posted with a title and a
    // body answers the 303 PRG with "Notification sent", writes exactly one unread
    // operator.mail row, and the Inbox tab (slotted on the page) draws exactly that entry: the
    // bold title, "operator.mail · unread", meta "1 · 1 unread", tab count 1.
    let (page_code, page) = get(admin, &format!("{front}/admin/e/player/{b}?tab=inbox")).await?;
    let page_hx = attr_values(&page, "hx-get");
    let notify_url = action_url(&page_hx, "notify", &b);
    let slots = page_hx.contains(&panel_url) && page_hx.contains(&count_url);
    let Some(notify_url) = notify_url else {
        // NA2 and NA3 post or compare the Notify form: without it all three are red, never absent.
        let why = format!("page={page_code}: no notifications/notify action on B's page; hx-get={page_hx:?} body={}", cut(&page));
        for (i, name) in NAMES.iter().enumerate() {
            p.check(&format!("[NA{}{tag}] {name}", i + 1), false, why.clone());
        }
        return Ok(());
    };
    let notify = open(admin, front, &notify_url).await?;
    let first = post(admin, front, "notify", &notify, &title, &body).await?;
    let after_first = rows(body.clone()).await?;
    let (panel_code, panel) = get(admin, &format!("{front}{panel_url}")).await?;
    let entries = timeline(&panel);
    let (count_code, count) = get(admin, &format!("{front}{count_url}")).await?;
    let meta = meta(&panel);
    let entry_ok = matches!(entries.as_slice(), [(t, d)] if *t == title && d == "operator.mail · unread");
    p.check(
        &format!("[NA1{tag}] {}", NAMES[0]),
        page_code == 200
            && slots
            && first == (303, SENT.to_string())
            && after_first == (1, 1)
            && panel_code == 200
            && attr(&panel, "data-state") == "ok"
            && entry_ok
            && meta.as_deref() == Some("1 · 1 unread")
            && count_code == 200
            && attr(&count, "data-state") == "ok"
            && count.contains(r#"<span class="count">1</span>"#),
        format!(
            "page={page_code} slots={slots} post={first:?} rows(all,sent)={after_first:?} panel={panel_code} state={:?} \
             entries={entries:?} want_title={title:?} meta={meta:?} count={count_code} count_body={:?} panel_body={}",
            attr(&panel, "data-state"),
            cut(&count),
            cut(&panel)
        ),
    );

    // [NA2] the same form instance posted again: the portal answers the consumed instance as
    // already submitted (a bare 303, no flash) with zero calls; with its claim released, as a
    // timed-out invoke leaves it, the re-POST reaches the provider with the same request id
    // and is a Duplicate (303 "Notification sent"); a third post of the instance with an edited
    // body is refused 409. B still holds exactly the one row with the first body.
    let na2 = async {
        let repeated = post(admin, front, "notify", &notify, &title, &body).await?;
        let after_repeat = rows(body.clone()).await?;
        let released = release(pool, &notify.form_id).await?;
        let replayed = post(admin, front, "notify", &notify, &title, &body).await?;
        let after_replay = rows(body.clone()).await?;
        let released_again = release(pool, &notify.form_id).await?;
        let edited = post(admin, front, "notify", &notify, &title, &format!("{body} edited")).await?;
        let after_edit = rows(body.clone()).await?;
        let ok = repeated == (303, String::new())
            && after_repeat == (1, 1)
            && released == 1
            && replayed == (303, SENT.to_string())
            && after_replay == (1, 1)
            && released_again == 1
            && edited.0 == 409
            && after_edit == (1, 1);
        let detail = format!(
            "repeat={repeated:?} rows(all,sent)={after_repeat:?} released={released} replay={replayed:?} rows={after_replay:?} \
             released_again={released_again} edited={edited:?} rows={after_edit:?}"
        );
        anyhow::Ok((ok, detail))
    }
    .await;
    let (na2_ok, na2_detail) = na2.unwrap_or_else(|e| (false, format!("{e:#}")));
    p.check(
        &format!("[NA2{tag}] {}", NAMES[1]),
        na2_ok,
        na2_detail,
    );

    // [NA3] the user menu's "Send notification" (notifications/send, the players row action) and
    // the Inbox panel tool (notifications/inbox.send) open the same form as Notify, each bound
    // to B over its own instance: title, labels, field names and submit label equal, the
    // instance id shown as the request id, each posting to its own action.
    let (menu_code, menu) = get(admin, &format!("{front}/admin/_menu/player/{b}")).await?;
    let send_url = action_url(&attr_values(&menu, "hx-get"), "send", &b);
    let tool_url = action_url(&attr_values(&panel, "hx-get"), "inbox.send", &b);
    let mut opened = vec![("notify", Some(shape(&notify)))];
    for (id, url) in [("send", &send_url), ("inbox.send", &tool_url)] {
        let form = match url {
            Some(u) => Some(shape(&open(admin, front, u).await?)),
            None => None,
        };
        opened.push((id, form));
    }
    let want = opened[0].1.clone();
    let same = opened.iter().all(|(id, s)| {
        s.as_ref().is_some_and(|s| {
            Some(&s.form) == want.as_ref().map(|w| &w.form) && s.post_url == format!("/admin/_action/notifications/{id}") && s.shows_id
        })
    });
    let form_ok = want.as_ref().is_some_and(|w| {
        w.form.0 == "Send notification"
            && w.form.1.first().is_some_and(|l| l.starts_with("Title"))
            && w.form.1.get(1).is_some_and(|l| l.starts_with("Message"))
            && w.form.1.get(2).is_some_and(|l| l.starts_with("Request id"))
            && w.form.2 == ["title", "body"]
            && w.form.3 == "Send"
    });
    p.check(
        &format!("[NA3{tag}] {}", NAMES[2]),
        menu_code == 200 && text(&menu).contains("Send notification") && same && form_ok,
        format!("menu={menu_code} send_url={send_url:?} tool_url={tool_url:?} opened={opened:?} menu_body={}", cut(&menu)),
    );
    Ok(())
}

/// One opened form modal: its status and html, and the csrf and instance id it posts.
struct Opened {
    html: String,
    csrf: String,
    form_id: String,
}

/// What a modal shows of its form: (title, field labels, posted field names but the portal's,
/// submit label), the URL it posts to, and whether it shows its instance id as the request id
/// (the hidden `_form` plus the readonly field).
#[derive(Clone, Debug)]
struct Shape {
    form: (String, Vec<String>, Vec<String>, String),
    post_url: String,
    shows_id: bool,
}

fn shape(o: &Opened) -> Shape {
    let h2 = o.html.find("<h2>").and_then(|at| o.html[at..].find("</h2>").map(|len| text(&o.html[at..at + len])));
    let labels = o
        .html
        .match_indices(r#"<label class="fl">"#)
        .filter_map(|(at, _)| o.html[at..].find("</label>").map(|len| text(&o.html[at..at + len])))
        .collect();
    let names = attr_values(&o.html, "name").into_iter().filter(|n| n != "_csrf" && n != "_form").collect();
    let submit = o
        .html
        .find(r#"<button type="submit""#)
        .and_then(|at| o.html[at..].find("</button>").map(|len| text(&o.html[at..at + len])));
    Shape {
        form: (h2.unwrap_or_default(), labels, names, submit.unwrap_or_default()),
        post_url: attr(&o.html, "hx-post"),
        shows_id: o.html.matches(&format!("value=\"{}\"", o.form_id)).count() >= 2,
    }
}

/// The `hx-get` among `hx` that opens `notifications/{id}` bound to player `b`.
fn action_url(hx: &[String], id: &str, b: &str) -> Option<String> {
    let want = format!("/admin/_action/notifications/{id}?");
    hx.iter().find(|u| u.starts_with(&want) && u.contains(b)).cloned()
}

/// GET a form modal as the browser opens it and read its `_csrf` and `_form`.
async fn open(admin: &reqwest::Client, front: &str, url: &str) -> Result<Opened> {
    let (code, html) = get(admin, &format!("{front}{url}")).await?;
    let fields = extract_form_fields(&html);
    let pick = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).unwrap_or_default();
    let (csrf, form_id) = (pick("_csrf"), pick("_form"));
    if code != 200 || csrf.is_empty() || form_id.is_empty() {
        bail!("open {url}: status {code} csrf={} form={}: {}", !csrf.is_empty(), !form_id.is_empty(), cut(&html));
    }
    Ok(Opened { html, csrf, form_id })
}

/// POST the form instance `o` to `notifications/{action}` as the browser posts it, then
/// follow the PRG's `Location` and read the flash toast. Answers (status, toast text); the
/// toast is empty on a redirect that carries none, and is the answer's text without one.
async fn post(admin: &reqwest::Client, front: &str, action: &str, o: &Opened, title: &str, body: &str) -> Result<(u16, String)> {
    let r = admin
        .post(format!("{front}/admin/_action/notifications/{action}"))
        .form(&[("_csrf", o.csrf.as_str()), ("_form", o.form_id.as_str()), ("title", title), ("body", body)])
        .send()
        .await?;
    let status = r.status().as_u16();
    let location = r.headers().get("location").and_then(|v| v.to_str().ok()).map(str::to_string);
    let posted = r.text().await.unwrap_or_default();
    let Some(location) = location else {
        return Ok((status, text(&posted)));
    };
    let (_, landed) = get(admin, &format!("{front}{location}")).await?;
    Ok((status, toast(&landed)))
}

/// Releases the form instance's claim and consumption, as a timed-out invoke leaves an
/// instance (`forms::settle` with `consumed = false`), so its next POST invokes again with
/// the same request id. Answers the rows updated.
async fn release(pool: &PgPool, form_id: &str) -> Result<u64> {
    let r = sqlx::query("UPDATE admin.forms SET claimed_at = NULL, consumed_at = NULL WHERE id = $1")
        .bind(form_id)
        .execute(pool)
        .await
        .context("release the form instance")?;
    Ok(r.rows_affected())
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

/// Every timeline entry in `html` (each `<ul class="tl">` list's items) as (bold title, meta
/// line).
fn timeline(html: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (at, _) in html.match_indices(r#"<ul class="tl">"#) {
        let list = &html[at..];
        let list = &list[..list.find("</ul>").unwrap_or(list.len())];
        for item in list.split("<li>").skip(1) {
            let title = item.find("<b>").and_then(|s| item[s..].find("</b>").map(|len| text(&item[s..s + len])));
            let open = r#"<div class="meta">"#;
            let detail = item.find(open).and_then(|s| item[s..].find("</div>").map(|len| text(&item[s..s + len])));
            out.push((title.unwrap_or_default(), detail.unwrap_or_default()));
        }
    }
    out
}
