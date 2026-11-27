//! [TK1]-[TK4]: player access tokens are ES256 JWTs a gateway checks on its own, against the
//! `accounts.token_state` snapshot it pulls every `accountsapi::TOKEN_STATE_PULL_INTERVAL`, run
//! through gateway-svc in the split and through the monolith's own front. A token of the dev
//! client verifies ([TK1]); a login naming a client accounts does not hold is 401 ([TK2]); a client
//! the operator creates through the portal mints tokens that verify ([TK3]); deleting it through
//! the portal refuses its tokens within [`REFUSAL_BOUND`] while the same player's dev-client
//! token stays live ([TK4]). The portal Revoke of one family is refused within the same bound in
//! [AR2] (`accounts_admin_checks`), and the other revocation writers in [A9], [DL2] and [PH13].
//!
//! [`until_refused`] is the one bound every such check waits under: a 401 still missing at
//! [`REFUSAL_BOUND`] after the revoking request returned is red, so a revocation that never
//! reaches the gateway's snapshot fails the check instead of waiting it out.

use std::fmt;
use std::future::Future;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use sqlx::PgPool;

use crate::accounts_admin_checks::toast;
use crate::apikeys_admin_checks::{fetch, patient};
use crate::characters_admin_checks::{attr_values, cut, text};
use crate::{accounts_me, extract_form_fields, pass_nonce, post_json, Ctx, Proof};

/// The longest a revocation committed at accounts may take to refuse its tokens at a gateway:
/// two pull periods, one for the next pull to start after the commit and one for that pull to
/// answer.
pub(super) const REFUSAL_BOUND: Duration = accountsapi::TOKEN_STATE_PULL_INTERVAL.saturating_mul(2);

/// The pause between two probes of a bearer that is not refused yet.
pub(super) const PROBE_GAP: Duration = Duration::from_millis(250);

/// The OIDC clients page, where Create client is a head action.
const CLIENTS_PAGE: &str = "/admin/p/accounts/clients";

/// How a bearer met its revocation: whether a probe started inside [`REFUSAL_BOUND`] answered
/// 401, the last status a probe answered, and when that probe started.
pub(super) struct Refusal {
    pub(super) refused: bool,
    last: Option<u16>,
    after: Duration,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "refused={} last={:?} probe_at_ms={} bound_ms={}",
            self.refused,
            self.last,
            self.after.as_millis(),
            REFUSAL_BOUND.as_millis()
        )
    }
}

/// Probes a revoked bearer until it answers exactly 401, starting probes only while
/// [`REFUSAL_BOUND`] has not passed since `since` (the instant the revoking request returned).
/// Any other status (200 before the pull, a 404 from accounts, a 503) is retried, never taken
/// as a refusal.
pub(super) async fn until_refused<F, Fut>(since: Instant, probe: F) -> Result<Refusal>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<u16>>,
{
    let mut last = None;
    let mut after = Duration::ZERO;
    while since.elapsed() <= REFUSAL_BOUND {
        after = since.elapsed();
        let code = probe().await?;
        last = Some(code);
        if code == 401 {
            return Ok(Refusal { refused: true, last, after });
        }
        tokio::time::sleep(PROBE_GAP).await;
    }
    Ok(Refusal { refused: false, last, after })
}

pub(super) async fn assertions(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    if let Err(e) = checks(ctx, pool, p, front, admin, tag).await {
        p.check(&format!("[TK0{tag}] token mechanism pass completes"), false, format!("{e:#}"));
    }
    Ok(())
}

async fn checks(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    let nonce = pass_nonce(tag);
    let email = format!("tk-{nonce}@test.local");
    let login_url = format!("{front}/accounts/login");
    let login = |client: &str| serde_json::json!({ "email": email, "password": "pw", "clientId": client });
    let field = |v: &serde_json::Value, k: &str| v.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();

    // [TK1] register and log in through the dev client: the login's bearer is a JWT that
    // /accounts/me accepts as the registered player.
    let register = serde_json::json!({ "email": email, "password": "pw", "displayName": "TkA", "clientId": accountsapi::DEV_GAME_CLIENT_ID });
    let (reg_code, reg) = post_json(ctx, &format!("{front}/accounts/register"), register).await?;
    let pid = field(&reg, "player_id");
    let (dev_code, dev) = post_json(ctx, &login_url, login(accountsapi::DEV_GAME_CLIENT_ID)).await?;
    let dev_token = field(&dev, "token");
    let (dev_me_code, dev_me) = accounts_me(ctx, front, &dev_token).await?;
    p.check(
        &format!("[TK1{tag}] a login through the dev client mints a JWT bearer that /accounts/me accepts as its player"),
        (200..300).contains(&reg_code)
            && !pid.is_empty()
            && dev_code == 200
            && dev_token.split('.').count() == 3
            && dev_me_code == 200
            && field(&dev_me, "player_id") == pid,
        format!("register={reg_code} pid={pid} login={dev_code} me={dev_me_code} me_pid={:?}", field(&dev_me, "player_id")),
    );
    if pid.is_empty() || dev_me_code != 200 {
        bail!("fixture: no live dev-client player for the TK pass (register {reg_code}, me {dev_me_code})");
    }

    // [TK2] the same credentials naming a client accounts does not hold are exactly 401, and
    // mint no family; [TK1]'s login with them is the control.
    let unknown = format!("c_unknown{nonce}");
    let families_before = player_families(pool, &pid).await?;
    let (unknown_code, _) = post_json(ctx, &login_url, login(&unknown)).await?;
    let families_after = player_families(pool, &pid).await?;
    p.check(
        &format!("[TK2{tag}] a login with an unknown clientId -> exactly 401 and no family minted"),
        unknown_code == 401 && families_after == families_before,
        format!("code={unknown_code} families_before={families_before} after={families_after}"),
    );

    // [TK3] Create client through the portal: the generated id is named in the toast and held
    // by accounts, and a login through it mints a bearer /accounts/me accepts. It holds every grant,
    // and `authorization_code` needs a registered redirect URI, which this op-path login never uses.
    let display = format!("Tk {nonce}");
    let (page_code, page) = fetch(admin, &format!("{front}{CLIENTS_PAGE}")).await?;
    let Some(create_url) = attr_values(&page, "hx-get").into_iter().find(|u| u.starts_with("/admin/_action/accounts/create-client")) else {
        bail!("no accounts/create-client on {CLIENTS_PAGE}: status {page_code}: {}", cut(&page));
    };
    let form = open(admin, front, &create_url).await?;
    let mut fields = vec![("display_name".to_string(), display.clone())];
    fields.extend(accountsapi::grant_types::ALL.iter().map(|g| ("grant_types".to_string(), g.to_string())));
    fields.push(("redirect_uris".to_string(), "http://127.0.0.1:9/tk/callback".to_string()));
    let created = post(admin, front, "create-client", &form, fields).await?;
    let client_id = created.1.rsplit(' ').next().unwrap_or_default().to_string();
    let held: Option<String> = sqlx::query_scalar("SELECT client_id FROM accounts.oidc_clients WHERE display_name = $1")
        .bind(&display)
        .fetch_optional(pool)
        .await
        .context("read the created client")?;
    let (own_code, own) = post_json(ctx, &login_url, login(&client_id)).await?;
    let own_token = field(&own, "token");
    let own_family: Option<String> = sqlx::query_scalar(
        "SELECT id::text FROM accounts.token_families WHERE player_id::text = $1 AND client_id = $2",
    )
    .bind(&pid)
    .bind(&client_id)
    .fetch_optional(pool)
    .await
    .context("read the created client's family")?;
    let (own_me_code, own_me) = accounts_me(ctx, front, &own_token).await?;
    p.check(
        &format!("[TK3{tag}] a client created through the portal logs in: its bearer is accepted by /accounts/me"),
        created.0 == 303
            && created.1.starts_with("Client created")
            && client_id.starts_with("c_")
            && held.as_deref() == Some(client_id.as_str())
            && own_code == 200
            && own_family.is_some()
            && own_me_code == 200
            && field(&own_me, "player_id") == pid,
        format!(
            "post={created:?} client={client_id} held={held:?} login={own_code} family={own_family:?} me={own_me_code} me_pid={:?}",
            field(&own_me, "player_id")
        ),
    );
    if held.as_deref() != Some(client_id.as_str()) || own_me_code != 200 {
        bail!("fixture: no live bearer of the created client {client_id:?} for [TK4]");
    }

    // [TK4] Delete client through the portal: its row and families go, one client revocation is
    // written, and its bearer is refused within REFUSAL_BOUND; the same player's dev-client
    // bearer still answers 200, so the refusal is the client's revocation, not the player's.
    let (menu_code, menu) = fetch(admin, &format!("{front}/admin/_menu/oidc_client/{client_id}")).await?;
    let Some(delete_url) = attr_values(&menu, "hx-get")
        .into_iter()
        .find(|u| u.starts_with("/admin/_action/accounts/delete-client?") && u.contains(&client_id))
    else {
        bail!("no accounts/delete-client in {client_id}'s row menu: status {menu_code}: {}", cut(&menu));
    };
    let form = open(admin, front, &delete_url).await?;
    let deleted = post(admin, front, "delete-client", &form, vec![("_confirm".to_string(), client_id.clone())]).await?;
    let since = Instant::now();
    let client_rows = count(pool, "SELECT count(*) FROM accounts.oidc_clients WHERE client_id = $1", &client_id).await?;
    let client_families = count(pool, "SELECT count(*) FROM accounts.token_families WHERE client_id = $1", &client_id).await?;
    let client_revocations = count(pool, "SELECT count(*) FROM accounts.token_revocations WHERE client_id = $1", &client_id).await?;
    let own_me = until_refused(since, || async { Ok(accounts_me(ctx, front, &own_token).await?.0) }).await?;
    let (dev_after, _) = accounts_me(ctx, front, &dev_token).await?;
    p.check(
        &format!("[TK4{tag}] deleting the client through the portal refuses its bearer within the pull bound; the dev client's bearer stays live"),
        deleted == (303, "Client deleted".to_string())
            && client_rows == 0
            && client_families == 0
            && client_revocations == 1
            && own_me.refused
            && dev_after == 200,
        format!(
            "post={deleted:?} client_rows={client_rows} families={client_families} revocations={client_revocations} \
             own_me=({own_me}) dev_me={dev_after}"
        ),
    );
    Ok(())
}

/// The number of token families `pid` holds.
async fn player_families(pool: &PgPool, pid: &str) -> Result<i64> {
    count(pool, "SELECT count(*) FROM accounts.token_families WHERE player_id::text = $1", pid).await
}

async fn count(pool: &PgPool, sql: &str, a: &str) -> Result<i64> {
    sqlx::query_scalar(sql).bind(a).fetch_one(pool).await.with_context(|| format!("count: {sql}"))
}

/// One opened modal's `_csrf` and `_form` instance id.
struct Opened {
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
    Ok(Opened { csrf, form_id })
}

/// POST the instance `o` to `accounts/{action}` with `fields` (a Multi field's key repeated),
/// returning to the clients page, then follow the PRG's `Location` and read the flash toast.
/// Answers (status, toast text); without a `Location` the text is the answer's own.
async fn post(admin: &reqwest::Client, front: &str, action: &str, o: &Opened, fields: Vec<(String, String)>) -> Result<(u16, String)> {
    let mut form = vec![
        ("_csrf".to_string(), o.csrf.clone()),
        ("_form".to_string(), o.form_id.clone()),
        ("_return".to_string(), CLIENTS_PAGE.to_string()),
    ];
    form.extend(fields);
    let r = patient(|| admin.post(format!("{front}/admin/_action/accounts/{action}")).form(&form)).await?;
    let status = r.status().as_u16();
    let location = r.headers().get("location").and_then(|v| v.to_str().ok()).map(str::to_string);
    let posted = r.text().await.unwrap_or_default();
    let Some(location) = location else {
        return Ok((status, text(&posted)));
    };
    let (_, landed) = fetch(admin, &format!("{front}{location}")).await?;
    Ok((status, toast(&landed)))
}
