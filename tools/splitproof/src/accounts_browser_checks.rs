//! The hosted browser sign-in (`/accounts/oidc/*`) and the one per-IP sign-in bucket, proven where they
//! are at risk: the split, through gateway-svc's passthrough into accounts-svc.
//!
//! The account is created over `cp.invoke` (admin-svc's portal, accounts-svc's `cp.*` face), its password
//! read off the one-shot reveal; every browser request then goes to gateway-svc, which proxies
//! `/accounts/oidc` and `/accounts/epic` to accounts-svc's one HTTP origin and appends its own peer to
//! `X-Forwarded-For`. Cookies are carried by hand (no cookie store), so each request states exactly
//! which `swarog_session` it presents and a cookie the browser was told to forget can be replayed.
//!
//! The bucket checks ([BR1], [BR2]) run last: with accounts-svc not trusting gateway-svc's host
//! every request it meets is one client, so a burst before the sign-in checks would refuse them.
//! Each burst names its two clients by `X-Forwarded-For` addresses from TEST-NET-3 that no other check
//! uses, and the gateway's own front-door limiter (keyed on this harness's loopback peer, its
//! answer "rate limit exceeded") is told apart from accounts' by the body, never counted as either
//! client's answer.
//!
//! Break runs (each a full splitproof run, then reverted): filled in by the step that ran them.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::header::{COOKIE, LOCATION, SET_COOKIE};
use sqlx::PgPool;

use crate::apikeys_admin_checks::{fetch, patient, secret_of};
use crate::characters_admin_checks::{attr_values, cut, text};
use crate::{accounts_me, extract_form_fields, pass_nonce, post_json, Ctx, Proof};

/// The accounts players page, where Create account is a page-head action.
const PLAYERS: &str = "/admin/p/accounts/players";
const LOGIN: &str = "/accounts/oidc/login";
const LANDING: &str = "/accounts/oidc/";
const LOGOUT: &str = "/accounts/oidc/logout";
const EPIC_START: &str = "/accounts/epic/start";
const SESSION: &str = "swarog_session";
const LOGIN_CSRF: &str = "swarog_login_csrf";
/// The hosted form's one error for an unknown email and a wrong password.
const GENERIC_ERROR: &str = "Invalid email or password.";
/// What accounts' sign-in bucket answers on the hosted form (its 429 page) and on the JSON Epic
/// start (its 429 body); the gateway's own 429 carries neither.
const HOSTED_REFUSAL: &str = "Sign-in is busy.";
const EPIC_REFUSAL: &str = "too many requests";
/// The two clients of each burst, as gateway-svc's caller names them in `X-Forwarded-For`.
const HOSTED_A: &str = "203.0.113.71";
const HOSTED_B: &str = "203.0.113.72";
const EPIC_A: &str = "203.0.113.73";
const EPIC_B: &str = "203.0.113.74";
/// Client B's requests, each of which must pass the bucket client A drained.
const B_REQUESTS: usize = 5;

pub(super) async fn assertions(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    if let Err(e) = sign_in_checks(ctx, pool, p, front, admin, tag).await {
        p.check(&format!("[BL0{tag}] hosted browser sign-in pass completes"), false, format!("{e:#}"));
    }
    if let Err(e) = bucket_checks(ctx, p, front, tag).await {
        p.check(&format!("[BR0{tag}] per-IP sign-in bucket pass completes"), false, format!("{e:#}"));
    }
    Ok(())
}

async fn sign_in_checks(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    let nonce = pass_nonce(tag);
    let email = format!("bl-{nonce}@test.local");
    let opener = page_head_opener(admin, front).await?;
    let password = reveal(admin, front, &opener, "create-account", &[("email", email.as_str()), ("display_name", "BlA")]).await?;
    let pid: String = sqlx::query_scalar("SELECT player_id::text FROM accounts.identities WHERE provider = 'password' AND subject = $1")
        .bind(&email)
        .fetch_optional(pool)
        .await
        .context("read the created account's player")?
        .with_context(|| format!("fixture: create-account answered a password but no password identity holds {email}"))?;
    // The handle the landing page must show, read the way a game client reads it: the revealed
    // password over the JSON login op, then `/accounts/me`.
    let (login_code, login) = post_json(ctx, &format!("{front}/accounts/login"), serde_json::json!({ "email": email, "password": password })).await?;
    let token = login.get("token").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    if !(200..300).contains(&login_code) || token.is_empty() {
        bail!("fixture: the revealed password on /accounts/login -> {login_code} {login}");
    }
    let (me_code, me) = accounts_me(ctx, front, &token).await?;
    let handle = me.get("handle").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    if me_code != 200 || !handle.starts_with("BlA#") {
        bail!("fixture: accounts/me for {pid} -> {me_code} handle {handle:?}");
    }

    // [BL1] the form through gateway-svc: 200, and the login-CSRF cookie scoped to the hosted
    // pages is the value the form double-submits.
    let page = browse(|| ctx.http_noredirect.get(format!("{front}{LOGIN}"))).await?;
    let csrf_cookie = set_cookie(&page.cookies, LOGIN_CSRF);
    let csrf = cookie_value(&csrf_cookie);
    let form_csrf = field(&page.body, "csrf");
    p.check(
        &format!("[BL1{tag}] GET {LOGIN} through gateway-svc -> 200 with {LOGIN_CSRF} (Path=/accounts/oidc, HttpOnly, SameSite=Strict) the form double-submits"),
        page.status == 200 && !csrf.is_empty() && form_csrf == csrf && has_attrs(&csrf_cookie, &["Path=/accounts/oidc", "HttpOnly", "SameSite=Strict"]),
        format!("code={} cookie={:?} form_matches={}", page.status, redact(&csrf_cookie), form_csrf == csrf),
    );

    // [BL2] the operator-created account signs in on the form: 303 to the landing page with the
    // browser session cookie, and accounts holds exactly that one browser session for it.
    let signed = sign_in(ctx, front, &email, &password).await?;
    let session_cookie = set_cookie(&signed.cookies, SESSION);
    let session = cookie_value(&session_cookie);
    let rows = browser_sessions(pool, &pid).await?;
    p.check(
        &format!("[BL2{tag}] POST {LOGIN} with the cp.invoke-created account -> 303 {LANDING} with {SESSION} (Path=/accounts/oidc, HttpOnly, SameSite=Lax) and one browser session row"),
        signed.status == 303
            && signed.location == LANDING
            && !session.is_empty()
            && has_attrs(&session_cookie, &["Path=/accounts/oidc", "HttpOnly", "SameSite=Lax"])
            && rows == 1,
        format!("code={} loc={:?} cookie={:?} rows={rows} body={}", signed.status, signed.location, redact(&session_cookie), cut(&text(&signed.body))),
    );

    // [BL3] the browser session is not a bearer: exactly 401 on an op, never 503 (accounts
    // unreachable) or 200.
    let bearer = patient(|| {
        ctx.http
            .get(format!("{front}/accounts/me"))
            .header("X-Api-Key", "dev-key-client")
            .header("Authorization", format!("Bearer {session}"))
    })
    .await?
    .status()
    .as_u16();
    p.check(
        &format!("[BL3{tag}] the {SESSION} value as a bearer on GET /accounts/me -> exactly 401"),
        !session.is_empty() && bearer == 401,
        format!("code={bearer} cookie_present={}", !session.is_empty()),
    );

    // [BL4] the landing page with that cookie shows the player's handle.
    let landing = browse(|| ctx.http_noredirect.get(format!("{front}{LANDING}")).header(COOKIE, format!("{SESSION}={session}"))).await?;
    let logout_csrf = field(&landing.body, "csrf");
    p.check(
        &format!("[BL4{tag}] GET {LANDING} with {SESSION} -> 200 showing the player's handle"),
        !session.is_empty() && landing.status == 200 && text(&landing.body).contains(&handle),
        format!("code={} loc={:?} handle={handle} body={}", landing.status, landing.location, cut(&text(&landing.body))),
    );

    // [BL5] logout clears the cookie and ends the session itself: the old cookie, replayed as a
    // browser that ignored the clear would send it, lands on the form and accounts holds no row.
    let out = browse(|| {
        ctx.http_noredirect
            .post(format!("{front}{LOGOUT}"))
            .header(COOKIE, format!("{SESSION}={session}"))
            .form(&[("csrf", logout_csrf.as_str())])
    })
    .await?;
    let cleared = set_cookie(&out.cookies, SESSION);
    let replay = browse(|| ctx.http_noredirect.get(format!("{front}{LANDING}")).header(COOKIE, format!("{SESSION}={session}"))).await?;
    let rows_after = browser_sessions(pool, &pid).await?;
    p.check(
        &format!("[BL5{tag}] POST {LOGOUT} -> 303 {LOGIN} clearing {SESSION}; the old cookie replayed on {LANDING} -> 303 {LOGIN}, no browser session row"),
        !session.is_empty()
            && out.status == 303
            && out.location == LOGIN
            && cleared.starts_with(&format!("{SESSION}=;"))
            && has_attrs(&cleared, &["Max-Age=0"])
            && replay.status == 303
            && replay.location == LOGIN
            && rows_after == 0,
        format!(
            "logout={} loc={:?} cleared={cleared:?} replay={} replay_loc={:?} rows={rows_after}",
            out.status, out.location, replay.status, replay.location
        ),
    );

    // [BL6] a second sign-in, then the operator's Reset password over cp.invoke: the browser
    // session minted before the reset no longer reaches the landing page.
    let again = sign_in(ctx, front, &email, &password).await?;
    let before_reset = cookie_value(&set_cookie(&again.cookies, SESSION));
    let held = browse(|| ctx.http_noredirect.get(format!("{front}{LANDING}")).header(COOKIE, format!("{SESSION}={before_reset}"))).await?;
    let new_password = match reset_opener(admin, front, &pid).await? {
        Some(url) => reveal(admin, front, &url, "reset-password", &[]).await?,
        None => bail!("fixture: the menu of player {pid} offers no Reset password"),
    };
    let after_reset = browse(|| ctx.http_noredirect.get(format!("{front}{LANDING}")).header(COOKIE, format!("{SESSION}={before_reset}"))).await?;
    let rows_reset = browser_sessions(pool, &pid).await?;
    p.check(
        &format!("[BL6{tag}] Reset password over cp.invoke ends the browser session minted before it: {LANDING} with that cookie 200 -> 303 {LOGIN}"),
        !before_reset.is_empty()
            && held.status == 200
            && new_password != password
            && after_reset.status == 303
            && after_reset.location == LOGIN
            && rows_reset == 0,
        format!(
            "signed_in={} before={} after={} after_loc={:?} rows={rows_reset} new_differs={}",
            again.status,
            held.status,
            after_reset.status,
            after_reset.location,
            new_password != password
        ),
    );

    // [BL7] the old password no longer signs in on the form; the revealed new one does.
    let old = sign_in(ctx, front, &email, &password).await?;
    let new = sign_in(ctx, front, &email, &new_password).await?;
    let old_session = cookie_value(&set_cookie(&old.cookies, SESSION));
    let new_session = cookie_value(&set_cookie(&new.cookies, SESSION));
    p.check(
        &format!("[BL7{tag}] after the reset the old password on {LOGIN} -> 401 generic error, no {SESSION}; the revealed new one -> 303 {LANDING} with {SESSION}"),
        old.status == 401
            && text(&old.body).contains(GENERIC_ERROR)
            && old_session.is_empty()
            && new.status == 303
            && new.location == LANDING
            && !new_session.is_empty(),
        format!(
            "old={} old_session={} new={} new_loc={:?} new_session={}",
            old.status,
            !old_session.is_empty(),
            new.status,
            new.location,
            !new_session.is_empty()
        ),
    );
    Ok(())
}

async fn bucket_checks(ctx: &Ctx, p: &mut Proof, front: &str, tag: &str) -> Result<()> {
    // [BR1] the hosted form: client A is refused by accounts' bucket, then client B's posts all
    // pass it (a 403 is the login-CSRF refusal, which only a request the bucket let through meets).
    let hosted = |client: &str| {
        ctx.http_noredirect
            .post(format!("{front}{LOGIN}"))
            .header("X-Forwarded-For", client)
            .form(&[("csrf", "none"), ("return_to", ""), ("email", "nobody@test.local"), ("password", "x")])
    };
    let br1 = two_clients(hosted, HOSTED_A, HOSTED_B, HOSTED_REFUSAL).await?;
    p.check(
        &format!("[BR1{tag}] a burst on POST {LOGIN} from one X-Forwarded-For client -> 429, a second client's posts -> 403 (bucket passed)"),
        br1.limited && br1.b.iter().all(|code| *code == 403),
        br1.detail(),
    );

    // [BR2] the JSON Epic start draws from the same bucket kind, per client: A refused, B's
    // starts all answered (200 with an authorize_url).
    let epic = |client: &str| ctx.http_noredirect.post(format!("{front}{EPIC_START}")).header("X-Forwarded-For", client);
    let br2 = two_clients(epic, EPIC_A, EPIC_B, EPIC_REFUSAL).await?;
    p.check(
        &format!("[BR2{tag}] a burst on POST {EPIC_START} from one X-Forwarded-For client -> 429, a second client's starts -> 200"),
        br2.limited && br2.b.iter().all(|code| *code == 200),
        br2.detail(),
    );
    Ok(())
}

/// One two-client burst: how many requests client A sent until accounts refused it, the
/// gateway's own 429s met on the way, and client B's answers.
struct Burst {
    a_sent: u32,
    limited: bool,
    gateway: u32,
    b: Vec<u16>,
}

impl Burst {
    fn detail(&self) -> String {
        format!("a_sent={} a_limited={} gateway_429={} b={:?}", self.a_sent, self.limited, self.gateway, self.b)
    }
}

/// Client A sends until accounts refuses it (`refusal` in a 429's body; at most 100), then client B
/// sends `B_REQUESTS` back to back. A 429 without `refusal` is the gateway's front-door limiter,
/// not accounts': that request is sent again after a short pause and counts for neither client.
async fn two_clients(build: impl Fn(&str) -> reqwest::RequestBuilder, a: &str, b: &str, refusal: &str) -> Result<Burst> {
    let mut burst = Burst { a_sent: 0, limited: false, gateway: 0, b: Vec::new() };
    while burst.a_sent < 100 && !burst.limited {
        match met(build(a), refusal).await? {
            None => burst.gateway += 1,
            Some(code) => {
                burst.a_sent += 1;
                burst.limited = code == 429;
            }
        }
        if burst.gateway > 50 {
            bail!("the gateway's front door refused {} requests: {}", burst.gateway, burst.detail());
        }
    }
    while burst.b.len() < B_REQUESTS {
        match met(build(b), refusal).await? {
            None => burst.gateway += 1,
            Some(code) => burst.b.push(code),
        }
        if burst.gateway > 50 {
            bail!("the gateway's front door refused {} requests: {}", burst.gateway, burst.detail());
        }
    }
    Ok(burst)
}

/// The status accounts answered, or none when the gateway's own limiter answered 429 (after a
/// pause long enough for its bucket to refill a token).
async fn met(req: reqwest::RequestBuilder, refusal: &str) -> Result<Option<u16>> {
    let r = req.send().await?;
    let code = r.status().as_u16();
    let body = r.text().await.unwrap_or_default();
    if code == 429 && !body.contains(refusal) {
        tokio::time::sleep(Duration::from_millis(100)).await;
        return Ok(None);
    }
    Ok(Some(code))
}

/// One answer as a browser meets it: status, `Location`, every `Set-Cookie` and the body.
struct Answer {
    status: u16,
    location: String,
    cookies: Vec<String>,
    body: String,
}

/// Sends the request `build` makes past the gateway's 429s, never following a redirect.
async fn browse(build: impl Fn() -> reqwest::RequestBuilder) -> Result<Answer> {
    let r = patient(build).await?;
    let status = r.status().as_u16();
    let location = r.headers().get(LOCATION).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    let cookies = r.headers().get_all(SET_COOKIE).iter().filter_map(|v| v.to_str().ok()).map(str::to_string).collect();
    let body = r.text().await.unwrap_or_default();
    Ok(Answer { status, location, cookies, body })
}

/// The form as a browser fills it: GET it for the login-CSRF cookie, then POST the credentials
/// with that cookie and its double-submitted value. Answers the POST.
async fn sign_in(ctx: &Ctx, front: &str, email: &str, password: &str) -> Result<Answer> {
    let page = browse(|| ctx.http_noredirect.get(format!("{front}{LOGIN}"))).await?;
    let csrf = cookie_value(&set_cookie(&page.cookies, LOGIN_CSRF));
    let form = [("csrf", csrf.as_str()), ("return_to", ""), ("email", email), ("password", password)];
    browse(|| ctx.http_noredirect.post(format!("{front}{LOGIN}")).header(COOKIE, format!("{LOGIN_CSRF}={csrf}")).form(&form)).await
}

/// The `Set-Cookie` for `name`, empty when the answer sets none.
fn set_cookie(cookies: &[String], name: &str) -> String {
    let prefix = format!("{name}=");
    cookies.iter().find(|c| c.starts_with(&prefix)).cloned().unwrap_or_default()
}

/// The value a `Set-Cookie` line sets, empty for a cleared or absent cookie.
fn cookie_value(set_cookie: &str) -> String {
    let pair = set_cookie.split(';').next().unwrap_or_default();
    pair.split_once('=').map(|(_, v)| v.trim().to_string()).unwrap_or_default()
}

/// Whether a `Set-Cookie` line carries every attribute in `attrs`.
fn has_attrs(set_cookie: &str, attrs: &[&str]) -> bool {
    let held: Vec<&str> = set_cookie.split(';').skip(1).map(str::trim).collect();
    !set_cookie.is_empty() && attrs.iter().all(|a| held.contains(a))
}

/// A `Set-Cookie` line with its value cut out, for a check's detail.
fn redact(set_cookie: &str) -> String {
    match set_cookie.split_once(';') {
        Some((pair, attrs)) => format!("{}=…;{attrs}", pair.split('=').next().unwrap_or_default()),
        None => set_cookie.to_string(),
    }
}

/// The value of the first input named `name` in `html`, empty when there is none.
fn field(html: &str, name: &str) -> String {
    extract_form_fields(html).into_iter().find(|(k, _)| k == name).map(|(_, v)| v).unwrap_or_default()
}

/// The players page's Create account opener.
async fn page_head_opener(admin: &reqwest::Client, front: &str) -> Result<String> {
    let (code, page) = fetch(admin, &format!("{front}{PLAYERS}")).await?;
    let opener = attr_values(&page, "hx-get").into_iter().find(|u| u.starts_with("/admin/_action/accounts/create-account"));
    opener.with_context(|| format!("fixture: {PLAYERS} -> {code} draws no Create account opener: {}", cut(&text(&page))))
}

/// The Reset password item of the player's row menu, none when the menu does not offer it.
async fn reset_opener(admin: &reqwest::Client, front: &str, pid: &str) -> Result<Option<String>> {
    let (code, menu) = fetch(admin, &format!("{front}/admin/_menu/player/{pid}")).await?;
    if code != 200 {
        bail!("fixture: menu of player {pid}: status {code}: {}", cut(&menu));
    }
    Ok(attr_values(&menu, "hx-get").into_iter().find(|u| u.starts_with("/admin/_action/accounts/reset-password?")))
}

/// Opens the modal at `url` (accounts prepares it over `cp.*`), posts its `_csrf` and `_form` with
/// `fields` to `accounts/{action}`, and reads the password off the one-shot reveal the 303 names.
async fn reveal(admin: &reqwest::Client, front: &str, url: &str, action: &str, fields: &[(&str, &str)]) -> Result<String> {
    let (code, modal) = fetch(admin, &format!("{front}{url}")).await?;
    let (csrf, form_id) = (field(&modal, "_csrf"), field(&modal, "_form"));
    if code != 200 || csrf.is_empty() || form_id.is_empty() {
        bail!("fixture: open {url}: status {code} csrf={} form={}: {}", !csrf.is_empty(), !form_id.is_empty(), cut(&text(&modal)));
    }
    let mut form = vec![("_csrf", csrf.as_str()), ("_form", form_id.as_str()), ("_return", PLAYERS)];
    form.extend_from_slice(fields);
    let r = patient(|| admin.post(format!("{front}/admin/_action/accounts/{action}")).form(&form)).await?;
    let status = r.status().as_u16();
    let location = r.headers().get(LOCATION).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    let posted = r.text().await.unwrap_or_default();
    if status != 303 || !location.contains("reveal=") {
        bail!("fixture: {action} -> {status} loc={location:?}: {}", cut(&text(&posted)));
    }
    let (_, revealed) = fetch(admin, &format!("{front}{location}")).await?;
    secret_of(&revealed)
        .filter(|s| !s.is_empty())
        .with_context(|| format!("fixture: the {action} reveal shows no password: {}", cut(&text(&revealed))))
}

/// The player's browser sessions, expired ones included.
async fn browser_sessions(pool: &PgPool, pid: &str) -> Result<i64> {
    sqlx::query_scalar("SELECT count(*) FROM accounts.browser_sessions WHERE player_id::text = $1")
        .bind(pid)
        .fetch_one(pool)
        .await
        .context("count the player's browser sessions")
}
