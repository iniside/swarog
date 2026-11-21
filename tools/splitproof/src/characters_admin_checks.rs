use anyhow::{bail, Context, Result};
use sqlx::PgPool;

use crate::{count_of, create_character, decode_html_attr, pass_nonce, register_capture, Ctx, Proof};

/// Every value of ` {key}="…"` in `html`, decoded as a browser reads it.
pub(super) fn attr_values(html: &str, key: &str) -> Vec<String> {
    let pat = format!(" {key}=\"");
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find(&pat) {
        let start = at + pat.len();
        let Some(len) = rest[start..].find('"') else { break };
        out.push(decode_html_attr(&rest[start..start + len]));
        rest = &rest[start + len..];
    }
    out
}

/// The first value of ` {key}="…"` in `html`, empty when it has none.
pub(super) fn attr(html: &str, key: &str) -> String {
    attr_values(html, key).into_iter().next().unwrap_or_default()
}

/// Each `<tag …>…</tag>` in `html` as (opening-tag attributes, raw inner html). The name must
/// be followed by a space or `>`, so `<th` never matches `<thead>`; the tag must not nest.
fn elements<'a>(html: &'a str, tag: &str) -> Vec<(&'a str, &'a str)> {
    let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find(&open) {
        let after = &rest[at + open.len()..];
        if !(after.starts_with(' ') || after.starts_with('>')) {
            rest = after;
            continue;
        }
        let Some(gt) = after.find('>') else { break };
        let inner = &after[gt + 1..];
        let Some(end) = inner.find(&close) else { break };
        out.push((&after[..gt], &inner[..end]));
        rest = &inner[end + close.len()..];
    }
    out
}

/// The first `<tag>` element's inner html, empty when there is none.
fn first_inner<'a>(html: &'a str, tag: &str) -> &'a str {
    elements(html, tag).first().map_or("", |(_, inner)| *inner)
}

/// `html` with its tags stripped, entities decoded and whitespace collapsed.
pub(super) fn text(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    decode_html_attr(&out).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One table's header labels and its body rows' cells (raw inner html), in order.
pub(super) fn table(html: &str) -> (Vec<String>, Vec<(String, Vec<String>)>) {
    let headers = elements(first_inner(html, "thead"), "th").iter().map(|(_, h)| text(h)).collect();
    let rows = elements(first_inner(html, "tbody"), "tr")
        .iter()
        .map(|(open, row)| (attr(open, "data-href"), elements(row, "td").iter().map(|(_, c)| c.to_string()).collect()))
        .collect();
    (headers, rows)
}

pub(super) fn cut(body: &str) -> String {
    body.chars().take(400).collect()
}

pub(super) async fn get(admin: &reqwest::Client, url: &str) -> Result<(u16, String)> {
    let r = admin.get(url).send().await.with_context(|| format!("GET {url}"))?;
    Ok((r.status().as_u16(), r.text().await.unwrap_or_default()))
}

/// The characters provider's four player-entity contributions (Characters tab, its count, the
/// "N characters" summary fact, the players list's Characters column) drawn by the portal that
/// `admin` holds a session on. In the split that portal is admin-svc, which reaches characters
/// only through its `characters` peer, so every panel here crosses the internal edge; a missing
/// or broken peer turns the slots to `error` and fails CH1-CH4.
pub(super) async fn assertions(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    if let Err(e) = checks(ctx, pool, p, front, admin, tag).await {
        p.check(&format!("[CH0{tag}] characters admin pass completes"), false, format!("{e:#}"));
    }
    Ok(())
}

async fn checks(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    let nonce = pass_nonce(tag);
    let (pid, token) = register_capture(ctx, front, &format!("ch-{nonce}@test.local"), "ChA").await?;
    // Created in reverse name order, so the creation-order assertion in CH2 fails on a name sort.
    let (one, two) = (format!("ChZed-{nonce}"), format!("ChAda-{nonce}"));
    let one_id = create_character(ctx, front, &token, &one).await.context("create ChZed")?;
    let two_id = create_character(ctx, front, &token, &two).await.context("create ChAda")?;
    let owned = count_of(pool, "SELECT count(*) FROM characters.characters WHERE player_id = $1::uuid", &pid).await;
    if owned != 2 {
        bail!("fixture: player {pid} owns {owned} characters, expected 2");
    }

    // [CH1] the entity page draws the Characters tab, selected, and points its slots at characters.
    let entity = format!("entity=player%3A{pid}");
    let (page_code, page) = get(admin, &format!("{front}/admin/e/player/{pid}?tab=characters")).await?;
    let tabs: Vec<_> = elements(&page, "button").into_iter().filter(|(open, _)| attr(open, "class") == "tab on").collect();
    let tab_href = tabs.first().map(|(open, _)| attr(open, "data-href")).unwrap_or_default();
    let tab_label = tabs.first().map(|(_, inner)| text(inner)).unwrap_or_default();
    let hx = attr_values(&page, "hx-get");
    let want = |c: &str| format!("/admin/_panel/characters/{c}?{entity}");
    let slot = |c: &str| hx.iter().find(|u| **u == want(c)).cloned();
    let (list_url, fact_url, count_url) = (slot("characters"), slot("fact.count"), slot("tab.count"));
    p.check(&format!("[CH1{tag}] entity page selects the Characters tab and slots the three characters panels"),
        page_code == 200 && tabs.len() == 1 && tab_href == format!("/admin/e/player/{pid}?tab=characters")
            && tab_label == "Characters" && list_url.is_some() && fact_url.is_some() && count_url.is_some(),
        format!("status={page_code} tabs_on={} href={tab_href:?} label={tab_label:?} characters={} fact={} count={}",
            tabs.len(), list_url.is_some(), fact_url.is_some(), count_url.is_some()));

    // [CH2] the tab panel: the six columns, both characters in creation order.
    let list_url = list_url.unwrap_or_else(|| want("characters"));
    let (list_code, list) = get(admin, &format!("{front}{list_url}")).await?;
    let (headers, rows) = table(&list);
    let row_ok = |row: Option<&(String, Vec<String>)>, name: &str, id: &str| {
        row.is_some_and(|(_, cells)| {
            cells.len() == 6 && text(&cells[0]) == name && text(&cells[1]) == "mage" && text(&cells[4]) == "—"
                && cells[5] == format!("<span class=\"mono muted\">{id}</span>")
        })
    };
    p.check(&format!("[CH2{tag}] Characters tab panel lists both characters through the characters provider"),
        list_code == 200 && attr(&list, "data-state") == "ok"
            && headers == ["Name", "Class", "Level", "Region", "Last played", "Id"]
            && rows.len() == 2 && row_ok(rows.first(), &one, &one_id) && row_ok(rows.get(1), &two, &two_id),
        format!("status={list_code} state={:?} headers={headers:?} rows={:?} body={}",
            attr(&list, "data-state"), rows.iter().map(|(_, c)| c.iter().map(|x| text(x)).collect::<Vec<_>>()).collect::<Vec<_>>(),
            cut(&list)));

    // [CH3] the summary fact and the tab count.
    let fact_url = fact_url.unwrap_or_else(|| want("fact.count"));
    let count_url = count_url.unwrap_or_else(|| want("tab.count"));
    let (fact_code, fact) = get(admin, &format!("{front}{fact_url}")).await?;
    let (count_code, count) = get(admin, &format!("{front}{count_url}")).await?;
    p.check(&format!("[CH3{tag}] summary fact reads 2 characters with the swords icon; tab count is 2"),
        fact_code == 200 && attr(&fact, "data-state") == "ok" && text(&fact) == "2 characters"
            && fact.contains(r#"<path d="M14.5 17.5 3 6V3h3l11.5 11.5"/>"#)
            && count_code == 200 && attr(&count, "data-state") == "ok" && count.contains(r#"<span class="count">2</span>"#),
        format!("fact={fact_code} {:?} count={count_code} {:?}", cut(&fact), cut(&count)));

    // [CH4] the players list (accounts) draws the Characters column from the characters provider.
    let (players_code, players) = get(admin, &format!("{front}/admin/_panel/accounts/players?q={pid}")).await?;
    let (headers, rows) = table(&players);
    let col = headers.iter().position(|h| h == "Characters");
    let row = rows.first();
    let cell = match (row, col) {
        (Some((_, cells)), Some(i)) => cells.get(i).map(|c| text(c)),
        _ => None,
    };
    p.check(&format!("[CH4{tag}] players list row for the player shows 2 in the Characters column"),
        players_code == 200 && attr(&players, "data-state") == "ok" && rows.len() == 1
            && row.is_some_and(|(href, cells)| *href == format!("/admin/e/player/{pid}") && cells.len() == headers.len())
            && cell.as_deref() == Some("2"),
        format!("status={players_code} state={:?} headers={headers:?} rows={} col={col:?} cell={cell:?} body={}",
            attr(&players, "data-state"), rows.len(), cut(&players)));
    Ok(())
}
