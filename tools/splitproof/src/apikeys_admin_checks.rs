use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use sha2::{Digest as _, Sha256};
use sqlx::PgPool;

use crate::characters_admin_checks::{attr, attr_values, cut, text};
use crate::{extract_form_fields, harness_player, pass_nonce, Ctx, Proof};

/// The apikeys page (`modules/apikeys/src/admin.rs` `MODULE` and `PAGE`).
const PAGE: &str = "/admin/p/apikeys/keys";

/// Where the browser's `admin.js` points a post's `_return`: the page the modal opened on.
const RETURN: &str = PAGE;

/// The one policy the pass grants: a method the catalog serves, so the role form offers it.
const POLICY: &str = "leaderboard.topScores";

/// The ids after AD4 with their names. All are reported red together when the page draws no
/// Create key opener, since each one opens a modal from it or from a row it leads to.
const LATER: [(&str, &str); 8] = [
    ("AD6b", "role.create through the portal -> 303, apikeys.roles row with the exact policy at rev 1"),
    ("AD6c", "key.create -> 303 to ?reveal=<token>, the secret shown once and gone on the second GET"),
    ("AD6d", "key row: secret_hash==base64url(sha256), role FK, no cleartext (reflected cols)"),
    ("AD6e", "the minted key authenticates at the gateway: leaderboard 200, match.report 403"),
    ("AD6f", "key.create on a role deleted after the form opened -> 409, no row"),
    ("AD6g", "admin.action records each attempt's outcome: role.create ok twice, key.create ok and conflict"),
    ("AD6h", "key.revoke twice -> Ok 'Key revoked', then NoOp 'already revoked', row unchanged"),
    ("AD6i", "role.delete of a role a revoked key still uses -> 409, row kept; unused -> 303 deleted"),
];

/// The apikeys provider's page, its actions and the key they mint, drawn and posted through
/// the portal that `admin` holds a session on. In the split the portal is admin-svc: the page,
/// both panels, the row menus, every prepared modal and every invoke are served by apikeys-svc
/// and reach admin-svc only through its `apikeys` peer; the minted key then authenticates at
/// the gateway. AD6b-AD6i prove that hop through `apikeys` rows only apikeys-svc writes (the
/// role, the key, its hash and its revocation), read back from the DB after each portal post.
/// Re-points the checks of the razed `/admin/api-keys` configurator (SPLITPROOF-06, plan step 21, D5): AD3b (the monolith's M3a), AD4, AD6b (the monolith's
/// M3b) and AD6c-AD6g, and adds AD6h (revoke twice, D1) and AD6i (delete an in-use role, K9).
/// The monolith runs the same ids tagged `m`.
///
/// AD4 is refused by the portal's CSRF check before any action lookup, so it stays green with
/// no apikeys provider at all; every other check needs one.
///
/// Break run (a full splitproof run, then reverted): admin-svc without its `apikeys` peer
/// (`cmd/admin-svc/src/lib.rs`): AD3b red as new in the split, AD4 green, the monolith's AD*m
/// green, 0 changed and 0 stale (2026-11-23, step 21t). That run proves AD3b and AD4 only:
/// AD6b-AD6i went red there from the missing Create key opener, before their own assertions
/// ran.
///
/// The pass is built and boxed here, out of line, as `notifications_admin_checks::assertions`
/// is, to keep the caller's stack frame small.
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
        let nonce = pass_nonce(tag);
        let names = Names {
            nonce: nonce.clone(),
            role: format!("proofrole-{nonce}"),
            ghost_role: format!("ghostrole-{nonce}"),
            key: format!("proofkey-{nonce}"),
            orphan_key: format!("orphankey-{nonce}"),
        };
        if let Err(e) = checks(ctx, pool, p, front, admin, tag, &names).await {
            p.check(&format!("[AD60{tag}] apikeys admin pass completes"), false, format!("{e:#}"));
        }
        // On every exit of `checks`, Ok or Err, so a pass that stopped short leaves no
        // working key or role in the reused DB.
        cleanup(pool, &names, tag).await;
        Ok(())
    })
}

/// The rows one pass may write, nonce-scoped so no pass reads or deletes another's.
struct Names {
    nonce: String,
    role: String,
    ghost_role: String,
    key: String,
    orphan_key: String,
}

/// Deletes the pass's keys, then its roles (the FK), and says how many rows each removed or
/// why it could not; a pass that ran to the end has already deleted both and removes none.
async fn cleanup(pool: &PgPool, n: &Names, tag: &str) {
    let keys = sqlx::query("DELETE FROM apikeys.keys WHERE name = ANY($1)")
        .bind(vec![n.key.clone(), n.orphan_key.clone()])
        .execute(pool)
        .await;
    let roles = sqlx::query("DELETE FROM apikeys.roles WHERE name = ANY($1)")
        .bind(vec![n.role.clone(), n.ghost_role.clone()])
        .execute(pool)
        .await;
    let said = |r: &std::result::Result<sqlx::postgres::PgQueryResult, sqlx::Error>| match r {
        Ok(done) => format!("{} removed", done.rows_affected()),
        Err(e) => format!("not removed: {e}"),
    };
    println!("[splitproof] apikeys pass{tag} cleanup: keys {}, roles {}", said(&keys), said(&roles));
}

async fn checks(
    ctx: &Ctx,
    pool: &PgPool,
    p: &mut Proof,
    front: &str,
    admin: &reqwest::Client,
    tag: &str,
    names: &Names,
) -> Result<()> {
    let (role_name, ghost_role): (&str, &str) = (&names.role, &names.ghost_role);
    let (key_name, orphan_key): (&str, &str) = (&names.key, &names.orphan_key);

    // [AD3b] the page draws the Create key head action and slots both panels; each panel,
    // fetched as the browser fetches it, is ok. The Keys panel searched for the dev seed key
    // (so the check holds past page 1) draws that key's own row, read off its row menu bound
    // to `apikey:dev-client` (a role cell naming dev-client cannot meet it), and no panel
    // shows a dev secret (the table shows a prefix at most).
    let (page_code, page) = fetch(admin, &format!("{front}{PAGE}")).await?;
    let page_hx = attr_values(&page, "hx-get");
    let create_key_url = page_hx.iter().find(|u| u.starts_with("/admin/_action/apikeys/key.create")).cloned();
    let keys_url = page_hx.iter().find(|u| u.starts_with("/admin/_panel/apikeys/keys")).cloned();
    let roles_url = page_hx.iter().find(|u| u.starts_with("/admin/_panel/apikeys/roles")).cloned();
    let (keys_code, keys) = match &keys_url {
        Some(u) => fetch(admin, &format!("{front}{u}")).await?,
        None => (0, String::new()),
    };
    let (roles_code, roles) = match &roles_url {
        Some(u) => fetch(admin, &format!("{front}{u}")).await?,
        None => (0, String::new()),
    };
    let (dev_code, dev) = match &keys_url {
        Some(u) => fetch(admin, &format!("{front}{u}{}q=dev-client", if u.contains('?') { '&' } else { '?' })).await?,
        None => (0, String::new()),
    };
    let dev_row = attr_values(&dev, "data-menu-url").iter().any(|u| u == "/admin/_menu/apikey/dev-client");
    let leak =
        ["dev-key-server", "dev-key-client"].iter().any(|s| keys.contains(s) || dev.contains(s) || page.contains(s));
    p.check(
        &format!("[AD3b{tag}] {PAGE} draws the Keys and Roles panels and the Create key opener, no dev secret"),
        page_code == 200
            && create_key_url.is_some()
            && keys_code == 200
            && attr(&keys, "data-state") == "ok"
            && dev_code == 200
            && dev_row
            && roles_code == 200
            && attr(&roles, "data-state") == "ok"
            && !leak,
        format!(
            "page={page_code} create={create_key_url:?} keys={keys_code} keys_state={:?} dev={dev_code} \
             dev_row={dev_row} roles={roles_code} roles_state={:?} leak={leak} page_body={}",
            attr(&keys, "data-state"),
            attr(&roles, "data-state"),
            cut(&page)
        ),
    );

    // [AD4] the portal's CSRF check runs before the action lookup (`core/admin/src/routes.rs`
    // `action_submit`): an apikeys action posted with a session and no _csrf is refused there,
    // 403 "invalid csrf token", whether or not any apikeys provider is reachable.
    let ad4_form = [("name", role_name), ("methods", POLICY)];
    let ad4 = patient(|| admin.post(format!("{front}/admin/_action/apikeys/role.create")).form(&ad4_form)).await?;
    let ad4_code = ad4.status().as_u16();
    let ad4_body = text(&ad4.text().await.unwrap_or_default());
    p.check(
        &format!("[AD4{tag}] portal CSRF before action lookup: apikeys POST without _csrf -> 403 'invalid csrf token'"),
        ad4_code == 403 && ad4_body.contains("invalid csrf token"),
        format!("status={ad4_code} body={}", cut(&ad4_body)),
    );

    let Some(create_key_url) = create_key_url else {
        let why = format!("page={page_code}: no apikeys/key.create opener on {PAGE}; hx-get={page_hx:?}");
        for (id, name) in LATER {
            p.check(&format!("[{id}{tag}] {name}"), false, why.clone());
        }
        return Ok(());
    };
    let audit_mark = audit_mark(pool).await?;

    // [AD6b] the Roles panel tool opens the role form; posted with one catalog method it
    // answers the 303 PRG with "Role created" and writes the role with exactly that policy at
    // revision 1.
    let role_create_url = attr_values(&roles, "hx-get").into_iter().find(|u| u.starts_with("/admin/_action/apikeys/role.create"));
    let Some(role_create_url) = role_create_url else {
        bail!("no apikeys/role.create tool on the Roles panel: {}", cut(&roles));
    };
    let role_form = open(admin, front, &role_create_url).await?;
    let offered = attr_values(&role_form.html, "value").iter().any(|v| v == POLICY);
    let role_fields = [("name", role_name), ("methods", POLICY), ("extra_methods", "")];
    let created = post(admin, front, "role.create", &role_form, &role_fields).await?;
    let row = role_row(pool, role_name).await?;
    p.check(
        &format!("[AD6b{tag}] {}", LATER[0].1),
        offered && created == (303, "Role created".to_string()) && row == Some((POLICY.to_string(), 1)),
        format!("offered={offered} post={created:?} row={row:?}"),
    );

    // [AD6c] the Create key form, posted for that role, answers 303 to ?reveal=<token>; the
    // GET of that URL shows the secret once in the reveal modal, and a second GET shows none.
    let key_form = open(admin, front, &create_key_url).await?;
    let (ck_code, location, ck_body) = create_key(admin, front, &key_form, key_name, role_name).await?;
    let (first_code, first) = fetch(admin, &format!("{front}{location}")).await?;
    let secret = secret_of(&first).unwrap_or_default();
    let (second_code, second) = fetch(admin, &format!("{front}{location}")).await?;
    p.check(
        &format!("[AD6c{tag}] {}", LATER[1].1),
        ck_code == 303
            && location.starts_with(&format!("{RETURN}?reveal="))
            && first_code == 200
            && secret.starts_with("ak_")
            && text(&first).contains(&format!("Key created — {key_name}"))
            && second_code == 200
            && !second.contains("data-secret")
            && !second.contains(&secret),
        format!(
            "status={ck_code} loc={location:?} first={first_code} secret_len={} second={second_code} second_secret={} body={}",
            secret.len(),
            second.contains("data-secret"),
            cut(&ck_body)
        ),
    );

    // [AD6d] the persisted row: secret_hash == base64url(sha256(secret)), the role FK, and no
    // column of the row holds the secret. The column set is reflected from
    // information_schema, so a future cleartext-bearing column cannot escape the scan; each
    // column is cast to text and joined with chr(31) (which a base64url secret cannot contain,
    // so no match can straddle a boundary); concat_ws skips NULLs.
    let key_meta: Option<(String, String)> =
        sqlx::query_as("SELECT secret_hash::text, role::text FROM apikeys.keys WHERE name = $1")
            .bind(key_name)
            .fetch_optional(pool)
            .await
            .context("read the minted key row")?;
    let key_columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns \
          WHERE table_schema = 'apikeys' AND table_name = 'keys' ORDER BY ordinal_position",
    )
    .fetch_all(pool)
    .await
    .context("reflect the apikeys.keys columns")?;
    let combined: Option<String> = if key_columns.is_empty() {
        None
    } else {
        let casts = key_columns.iter().map(|c| format!("\"{c}\"::text")).collect::<Vec<_>>().join(", ");
        sqlx::query_scalar(&format!("SELECT concat_ws(chr(31), {casts}) FROM apikeys.keys WHERE name = $1"))
            .bind(key_name)
            .fetch_optional(pool)
            .await
            .context("read the minted key row's columns")?
    };
    let recomputed = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_bytes()));
    let hash_ok = key_meta.as_ref().is_some_and(|(h, _)| *h == recomputed);
    let role_fk_ok = key_meta.as_ref().is_some_and(|(_, r)| *r == role_name);
    // A scan with no secret, no reflected column or no row is a failure, never a vacuous pass.
    let no_cleartext =
        !secret.is_empty() && !key_columns.is_empty() && combined.as_ref().is_some_and(|c| !c.contains(&secret));
    p.check(
        &format!("[AD6d{tag}] {}", LATER[2].1),
        hash_ok && role_fk_ok && no_cleartext,
        format!(
            "present={} cols={} hash_ok={hash_ok} role_fk={role_fk_ok} no_cleartext={no_cleartext}",
            key_meta.is_some(),
            key_columns.len()
        ),
    );

    // [AD6e] the minted key authenticates at the gateway: 200 on the one op its role allows,
    // 403 on another (transient gateway 429s retried past, never counted).
    let auth_code = keyed(|| ctx.http.get(format!("{front}/leaderboard")).header("X-Api-Key", secret.as_str())).await?;
    let report = serde_json::json!({
        "ReportId": format!("ad6-{}", names.nonce),
        "Winner": harness_player("ad6-w"),
        "Loser": harness_player("ad6-l"),
    });
    let deny_code =
        keyed(|| ctx.http.post(format!("{front}/match/report")).header("X-Api-Key", secret.as_str()).json(&report)).await?;
    p.check(
        &format!("[AD6e{tag}] {}", LATER[3].1),
        auth_code == 200 && deny_code == 403,
        format!("leaderboard={auth_code} report={deny_code}"),
    );

    // [AD6f] a Create key form opened while its role exists, posted after that role was
    // deleted underneath it, is a 409 (the FK refusal, never a 405 or a 422) and writes no key.
    let ghost_form = open(admin, front, &role_create_url).await?;
    let ghost_fields = [("name", ghost_role), ("methods", POLICY), ("extra_methods", "")];
    let ghost = post(admin, front, "role.create", &ghost_form, &ghost_fields).await?;
    let orphan_form = open(admin, front, &create_key_url).await?;
    let offers_ghost = orphan_form.html.contains(&format!(">{ghost_role}<"));
    let dropped = sqlx::query("DELETE FROM apikeys.roles WHERE name = $1")
        .bind(ghost_role)
        .execute(pool)
        .await
        .context("delete the ghost role under the open form")?
        .rows_affected();
    let (orphan_code, _, orphan_body) = create_key(admin, front, &orphan_form, orphan_key, ghost_role).await?;
    let orphan_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM apikeys.keys WHERE name = $1")
        .bind(orphan_key)
        .fetch_one(pool)
        .await
        .context("count the orphan key rows")?;
    p.check(
        &format!("[AD6f{tag}] {}", LATER[4].1),
        ghost == (303, "Role created".to_string()) && offers_ghost && dropped == 1 && orphan_code == 409 && orphan_rows == 0,
        format!(
            "ghost={ghost:?} offered={offers_ghost} dropped={dropped} status={orphan_code} rows={orphan_rows} body={}",
            cut(&text(&orphan_body))
        ),
    );

    // [AD6g] the portal's plane recorded one admin.action per invoke attempt since the mark,
    // each with its outcome (the detail up to its first ':'): two apikeys.role.create ok (the
    // role and the ghost), one apikeys.key.create ok (the minted key) and one
    // apikeys.key.create conflict (the 409, the refused attempt included).
    let recorded = audit_since(pool, audit_mark).await?;
    let mut outcomes: Vec<(String, String)> = recorded
        .iter()
        .map(|(action, detail)| (action.clone(), detail.split(':').next().unwrap_or_default().to_string()))
        .collect();
    outcomes.sort();
    let want = [
        ("apikeys.key.create", "conflict"),
        ("apikeys.key.create", "ok"),
        ("apikeys.role.create", "ok"),
        ("apikeys.role.create", "ok"),
    ];
    p.check(
        &format!("[AD6g{tag}] {}", LATER[5].1),
        outcomes.len() == want.len() && outcomes.iter().zip(want).all(|((a, o), (wa, wo))| a == wa && o == wo),
        format!("mark={audit_mark} recorded={recorded:?}"),
    );

    // [AD6h] the key's row menu opens Revoke bound to the key; confirmed with the typed name it
    // answers "Key revoked" and sets revoked_at; a second Revoke is a NoOp that leaves the row
    // (revoked_at, revision) as the first left it.
    let ad6h = async {
        let Some(url) = menu_action(admin, front, "apikey", key_name, "key.revoke").await? else {
            bail!("no apikeys/key.revoke in {key_name}'s row menu");
        };
        let confirm = [("_reason", ""), ("_confirm", key_name)];
        let form = open(admin, front, &url).await?;
        let first = post(admin, front, "key.revoke", &form, &confirm).await?;
        let after_first = revoked(pool, key_name).await?;
        let form = open(admin, front, &url).await?;
        let second = post(admin, front, "key.revoke", &form, &confirm).await?;
        let after_second = revoked(pool, key_name).await?;
        let ok = first == (303, "Key revoked".to_string())
            && after_first.as_ref().is_some_and(|(at, _)| at.is_some())
            && second == (303, format!("Key {key_name} is already revoked"))
            && after_second == after_first;
        anyhow::Ok((ok, format!("first={first:?} row={after_first:?} second={second:?} row={after_second:?}")))
    }
    .await;
    let (ad6h_ok, ad6h_detail) = ad6h.unwrap_or_else(|e| (false, format!("{e:#}")));
    p.check(&format!("[AD6h{tag}] {}", LATER[6].1), ad6h_ok, ad6h_detail);

    // [AD6i] Delete role on the role the (now revoked) key still uses is a 409 and the role
    // stays; with the key gone the same action deletes it, so the 409 is the key, not the route.
    let ad6i = async {
        let Some(url) = menu_action(admin, front, "apirole", role_name, "role.delete").await? else {
            bail!("no apikeys/role.delete in {role_name}'s row menu");
        };
        let form = open(admin, front, &url).await?;
        let in_use = post(admin, front, "role.delete", &form, &[]).await?;
        let kept = role_row(pool, role_name).await?;
        let gone_key = sqlx::query("DELETE FROM apikeys.keys WHERE name = $1")
            .bind(key_name)
            .execute(pool)
            .await
            .context("delete the revoked key")?
            .rows_affected();
        let form = open(admin, front, &url).await?;
        let unused = post(admin, front, "role.delete", &form, &[]).await?;
        let after = role_row(pool, role_name).await?;
        let ok = in_use.0 == 409
            && kept.is_some()
            && gone_key == 1
            && unused == (303, "Role deleted".to_string())
            && after.is_none();
        anyhow::Ok((ok, format!("in_use={in_use:?} kept={kept:?} key_deleted={gone_key} unused={unused:?} after={after:?}")))
    }
    .await;
    let (ad6i_ok, ad6i_detail) = ad6i.unwrap_or_else(|e| (false, format!("{e:#}")));
    p.check(&format!("[AD6i{tag}] {}", LATER[7].1), ad6i_ok, ad6i_detail);
    Ok(())
}

/// One opened modal: its html, and the csrf and instance id it posts.
struct Opened {
    html: String,
    csrf: String,
    form_id: String,
}

/// GET a modal as the browser opens it and read its `_csrf` and `_form`.
async fn open(admin: &reqwest::Client, front: &str, url: &str) -> Result<Opened> {
    let (code, html) = fetch(admin, &format!("{front}{url}")).await?;
    let fields = extract_form_fields(&html);
    let pick = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).unwrap_or_default();
    let (csrf, form_id) = (pick("_csrf"), pick("_form"));
    if code != 200 || csrf.is_empty() || form_id.is_empty() {
        bail!("open {url}: status {code} csrf={} form={}: {}", !csrf.is_empty(), !form_id.is_empty(), cut(&html));
    }
    Ok(Opened { html, csrf, form_id })
}

/// POST the instance `o` to `apikeys/{action}` with `fields`, as the browser posts it (with
/// `_return` set to the page), then follow the PRG's `Location` and read the flash toast.
/// Answers (status, toast text); without a `Location` the text is the answer's own.
async fn post(admin: &reqwest::Client, front: &str, action: &str, o: &Opened, fields: &[(&str, &str)]) -> Result<(u16, String)> {
    let mut form = vec![("_csrf", o.csrf.as_str()), ("_form", o.form_id.as_str()), ("_return", RETURN)];
    form.extend_from_slice(fields);
    let r = patient(|| admin.post(format!("{front}/admin/_action/apikeys/{action}")).form(&form)).await?;
    let status = r.status().as_u16();
    let location = r.headers().get("location").and_then(|v| v.to_str().ok()).map(str::to_string);
    let posted = r.text().await.unwrap_or_default();
    let Some(location) = location else {
        return Ok((status, text(&posted)));
    };
    let (_, landed) = fetch(admin, &format!("{front}{location}")).await?;
    Ok((status, toast(&landed)))
}

/// POST the Create key instance `o` for `name` on `role`, never following the redirect: the
/// reveal it carries is one-shot. Answers (status, `Location`, body).
async fn create_key(admin: &reqwest::Client, front: &str, o: &Opened, name: &str, role: &str) -> Result<(u16, String, String)> {
    let form = [
        ("_csrf", o.csrf.as_str()),
        ("_form", o.form_id.as_str()),
        ("_return", RETURN),
        ("name", name),
        ("role", role),
        ("expires", "Never"),
    ];
    let r = patient(|| admin.post(format!("{front}/admin/_action/apikeys/key.create")).form(&form)).await?;
    let status = r.status().as_u16();
    let location = r.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    Ok((status, location, r.text().await.unwrap_or_default()))
}

/// The `hx-get` of `apikeys/{action}` bound to `{kind}:{id}` in that row's menu.
async fn menu_action(admin: &reqwest::Client, front: &str, kind: &str, id: &str, action: &str) -> Result<Option<String>> {
    let (code, menu) = fetch(admin, &format!("{front}/admin/_menu/{kind}/{id}")).await?;
    if code != 200 {
        bail!("menu {kind}/{id}: status {code}: {}", cut(&menu));
    }
    let want = format!("/admin/_action/apikeys/{action}?");
    Ok(attr_values(&menu, "hx-get").into_iter().find(|u| u.starts_with(&want) && u.contains(id)))
}

/// The flash toast's text (`<div data-toast …>`), empty when the page draws none.
fn toast(html: &str) -> String {
    let Some(at) = html.find("<div data-toast") else { return String::new() };
    let rest = &html[at..];
    let end = rest.find("</div>").unwrap_or(rest.len());
    text(&rest[..end])
}

/// The show-once secret in the reveal modal (`<span data-secret>`), none when not drawn.
fn secret_of(html: &str) -> Option<String> {
    let open = "<span data-secret>";
    let start = html.find(open)? + open.len();
    let len = html[start..].find("</span>")?;
    Some(text(&html[start..start + len]))
}

/// The role's (policy, revision), none when absent.
async fn role_row(pool: &PgPool, name: &str) -> Result<Option<(String, i64)>> {
    sqlx::query_as("SELECT policy, revision FROM apikeys.roles WHERE name = $1")
        .bind(name)
        .fetch_optional(pool)
        .await
        .context("read the role row")
}

/// The key's (revoked_at, revision), none when absent.
async fn revoked(pool: &PgPool, name: &str) -> Result<Option<(Option<String>, i64)>> {
    sqlx::query_as("SELECT revoked_at::text, revision FROM apikeys.keys WHERE name = $1")
        .bind(name)
        .fetch_optional(pool)
        .await
        .context("read the key's revocation")
}

/// The newest event's `tie_breaker` (an identity, so every event appended later is above it).
async fn audit_mark(pool: &PgPool) -> Result<i64> {
    sqlx::query_scalar("SELECT coalesce(max(tie_breaker), 0) FROM asyncevents.events")
        .fetch_one(pool)
        .await
        .context("mark the event plane before the apikeys posts")
}

/// proofadmin's apikeys create `admin.action` rows appended after `mark`: (action, detail).
async fn audit_since(pool: &PgPool, mark: i64) -> Result<Vec<(String, String)>> {
    sqlx::query_as(
        "SELECT payload->>'action', payload->>'detail' FROM asyncevents.events \
          WHERE topic = 'admin.action' AND payload->>'actor' = 'proofadmin' AND tie_breaker > $1 \
            AND payload->>'action' IN ('apikeys.role.create', 'apikeys.key.create') \
          ORDER BY tie_breaker",
    )
    .bind(mark)
    .fetch_all(pool)
    .await
    .context("read the apikeys admin.action rows")
}

/// Sends the request `build` makes until the answer is not a 429 (ten tries, a growing pause
/// between them); answers the last response. A 429 is the gateway's rate limit refusing the
/// request before the portal sees it, so the retry never repeats an action.
async fn patient(build: impl Fn() -> reqwest::RequestBuilder) -> Result<reqwest::Response> {
    let mut tries = 0;
    loop {
        let r = build().send().await?;
        tries += 1;
        if r.status().as_u16() != 429 || tries == 10 {
            return Ok(r);
        }
        tokio::time::sleep(Duration::from_millis(300 * tries)).await;
    }
}

/// The status of the request `build` makes, past the gateway's 429s.
async fn keyed(build: impl Fn() -> reqwest::RequestBuilder) -> Result<u16> {
    Ok(patient(build).await?.status().as_u16())
}

/// GET `url` past the gateway's 429s; answers (status, body).
async fn fetch(admin: &reqwest::Client, url: &str) -> Result<(u16, String)> {
    let r = patient(|| admin.get(url)).await.with_context(|| format!("GET {url}"))?;
    Ok((r.status().as_u16(), r.text().await.unwrap_or_default()))
}
