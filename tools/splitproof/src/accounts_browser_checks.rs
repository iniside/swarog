//! The hosted browser sign-in (`/accounts/oidc/*`) and the one per-IP sign-in bucket, proven where they
//! are at risk: the split, through gateway-svc's passthrough into accounts-svc.
//!
//! The account is created over `cp.invoke` (admin-svc's portal, accounts-svc's `cp.*` face), its password
//! read off the one-shot reveal; every browser request then goes to gateway-svc, which proxies
//! `/accounts/oidc` and `/accounts/epic` to accounts-svc's one HTTP origin and appends its own peer to
//! `X-Forwarded-For`. Cookies are carried by hand (no cookie store), so each request states exactly
//! which `swarog_session` it presents and a cookie the browser was told to forget can be replayed.
//!
//! The bucket checks ([BR1], [BR2], [BR3]) run last: with accounts-svc not trusting gateway-svc's host
//! every request it meets is one client, so a burst before the sign-in checks would refuse them.
//! Each burst names its clients by `X-Forwarded-For` addresses from TEST-NET-3 that no other check
//! uses, and the gateway's own front-door limiter (keyed on this harness's loopback peer, its
//! answer "rate limit exceeded") is told apart from accounts' by the body, never counted as either
//! client's answer. [BR1] and [BR2] show a second client passing the bucket the first drained; [BR3]
//! drains one client on the hosted form and shows its next Epic starts refused, so the form and
//! the Epic start draw from one bucket as accounts wires it.
//!
//! The Google checks ([BG1], [BG2], [BG3]) run between the two: the split Proof fleet configures
//! google against the loopback OIDC fixture with a dead token endpoint, so a start, a failed
//! callback and a state another provider minted are provable without a live Google. Each names its
//! own `X-Forwarded-For` client, so no bucket another check drew from can refuse it.
//!
//! Timing rule: the run after a refusal (`B_REQUESTS` back to back) is timed from the instant this
//! harness took just before sending the request accounts refused, so the clock starts at or before
//! the server's refusal, never after it. At the refusal the bucket holds some fraction `f` of a
//! token, `f < 1` (it refills continuously and keeps fractions), so a shared bucket needs at least
//! `(B_REQUESTS - f) / REFILL_PER_SEC` seconds after the refusal to answer the run
//! (`REFILL_PER_SEC` being `accountsapi::LOGIN_LIMIT_PER_SEC`, the rate accounts builds the bucket
//! from), which is more than `(B_REQUESTS - 1) / REFILL_PER_SEC`; the measured time includes all of
//! it and more. A run measured at that budget or longer, or one that met any gateway 429, could
//! have been served by tokens refilled into the bucket that refused: it proves neither separate
//! buckets nor a shared one, so [BR1], [BR2] and [BR3] fail it as INCONCLUSIVE, never pass.
//!
//! Break runs (each a full splitproof run, then reverted):
//! - `"/accounts/oidc"` dropped from gateway-svc's passthrough prefixes (`cmd/gateway-svc/src/addrs.rs`):
//!   BL1, BL2, BL4-BL7 and BR1 red as new (every hosted-form request 404 at the gateway), BR2 green;
//!   BL3 red too, but only because no `swarog_session` was minted to present (`cookie_present=false`):
//!   it has no once-wrong line of its own (2026-11-25, step 7).
//! - the accounts `TRUSTED_PROXY_CIDRS` dropped from the proof fleet (`tools/processctl/src/fleet.rs`)
//!   and `kill_player_families_tx`'s browser-session delete replaced by `0`
//!   (`modules/accounts/src/store.rs`): BR1, BR2 and BL6 red as new (every burst one client, the
//!   second client's requests 429; the cookie minted before the reset still 200 with its row kept),
//!   BL1-BL5 and BL7 green (2026-11-26, step 7).
//! - the Epic start router given its own `IpLimiter` ("accounts_login_epic", same rate and burst)
//!   instead of the hosted form's `login_limiter` (`modules/accounts/src/lib.rs`), run
//!   splitproof-62180: BR3 alone red as new (`epic codes=[200x5]` after the form drained the
//!   client, `run_gateway_429=0`), BL1-BL7, BR1 and BR2 green (2026-11-26, step 7).
//! - `create_browser_session_tx` also inserting the plaintext token into `accounts.sessions`
//!   (`modules/accounts/src/web/session.rs`), run splitproof-63840: BL3 alone red as new
//!   (`code=200 cookie_present=true`), BL1, BL2, BL4-BL7 and BR1-BR3 green (2026-11-26, step 7).

use std::time::{Duration, Instant};

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
const EPIC_START: &str = "/accounts/epic/start?client_id=dev-game";
const EPIC_CALLBACK: &str = "/accounts/epic/callback";
const GOOGLE_START: &str = "/accounts/oidc/google";
const GOOGLE_CALLBACK: &str = "/accounts/oidc/google/callback";
/// The redirect URI the Proof fleet pins on accounts-svc for google (gateway-svc's origin).
const GOOGLE_REDIRECT_URI: &str =
    concat!(processctl::split_gateway_origin!(), "/accounts/oidc/google/callback");
/// Google's authorize endpoint, accounts' default: the Proof fleet does not pin it.
const GOOGLE_AUTHORIZE: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// The cookie binding an OAuth state to the browser that started it, set on every start.
const BINDING: &str = "epic_oauth_binding";
/// What the sign-in form shows after `?google=error`.
const GOOGLE_FAILED: &str = "Sign-in with Google failed.";
/// The `return_to` the [BG*] starts carry, so the error redirect shows it came back off the state row.
const BG_RETURN_TO: &str = "/accounts/oidc/?bg=1";
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
/// The one client of [BR3], drained on the hosted form then sent to the Epic start.
const SHARED: &str = "203.0.113.75";
/// One client per [BG*] check, so none meets a sign-in bucket another check drew from.
const BG1_CLIENT: &str = "203.0.113.76";
const BG2_CLIENT: &str = "203.0.113.77";
const BG3_CLIENT: &str = "203.0.113.78";
/// Client B's requests, each of which must pass the bucket client A drained.
const B_REQUESTS: usize = 5;
/// The tokens a second accounts' sign-in bucket refills: the rate accounts builds its limiter from.
const REFILL_PER_SEC: f64 = accountsapi::LOGIN_LIMIT_PER_SEC;
/// Slack on top of the front-door bucket's refill-from-empty time, for request latency and timer
/// granularity.
const GATEWAY_REFILL_MARGIN: Duration = Duration::from_millis(500);

/// Long enough for gateway-svc's front-door bucket, keyed on this harness's loopback peer, to refill
/// from empty: its burst at its rate, as the Proof fleet pins them on gateway-svc
/// (`processctl::PROOF_GATEWAY_RATE_LIMIT_BURST` / `PROOF_GATEWAY_RATE_LIMIT_RPS`), plus
/// `GATEWAY_REFILL_MARGIN`.
fn gateway_refill() -> Duration {
    let from_empty = f64::from(processctl::PROOF_GATEWAY_RATE_LIMIT_BURST) / processctl::PROOF_GATEWAY_RATE_LIMIT_RPS;
    Duration::from_secs_f64(from_empty) + GATEWAY_REFILL_MARGIN
}

pub(super) async fn assertions(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    if let Err(e) = sign_in_checks(ctx, pool, p, front, admin, tag).await {
        p.check(&format!("[BL0{tag}] hosted browser sign-in pass completes"), false, format!("{e:#}"));
    }
    if let Err(e) = google_checks(ctx, pool, p, front, tag).await {
        p.check(&format!("[BG0{tag}] Google browser sign-in pass completes"), false, format!("{e:#}"));
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
    let (login_code, login) = post_json(ctx, &format!("{front}/accounts/login"), serde_json::json!({ "email": email, "password": password, "clientId": accountsapi::DEV_GAME_CLIENT_ID })).await?;
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

/// Google's browser sign-in through gateway-svc, configured by the split Proof fleet against the
/// loopback fixture with a dead token endpoint: the start, the failed callback, and a state another
/// provider minted.
async fn google_checks(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, tag: &str) -> Result<()> {
    // [BG1] the form offers the Google button; its POST with the login CSRF is a 303 to Google's
    // authorize URL for the pinned client and redirect URI, sets the binding cookie, and persists
    // a google `browser` state carrying the form's return_to.
    let page = browse(|| ctx.http_noredirect.get(format!("{front}{LOGIN}")).header("X-Forwarded-For", BG1_CLIENT)).await?;
    let button = page.body.contains(&format!("action=\"{GOOGLE_START}\"")) && text(&page.body).contains("Continue with Google");
    let started = google_start(ctx, front, BG1_CLIENT, &page).await?;
    let authorize = reqwest::Url::parse(&started.location).ok();
    let endpoint = authorize.as_ref().map(|u| format!("{}{}", u.origin().ascii_serialization(), u.path())).unwrap_or_default();
    let client_id = query_param(authorize.as_ref(), "client_id");
    let redirect_uri = query_param(authorize.as_ref(), "redirect_uri");
    let state = query_param(authorize.as_ref(), "state");
    let binding_cookie = set_cookie(&started.cookies, BINDING);
    let row = oauth_state(pool, &state).await?;
    let want_row = Some(("google".to_string(), "browser".to_string(), Some(BG_RETURN_TO.to_string())));
    p.check(
        &format!("[BG1{tag}] {LOGIN} shows Continue with Google; POST {GOOGLE_START} with the login CSRF -> 303 to Google's authorize URL (pinned client_id, redirect_uri, state) with {BINDING} and a google browser state row"),
        page.status == 200
            && button
            && started.status == 303
            && endpoint == GOOGLE_AUTHORIZE
            && client_id == processctl::PROOF_GOOGLE_CLIENT_ID
            && redirect_uri == GOOGLE_REDIRECT_URI
            && !state.is_empty()
            && !cookie_value(&binding_cookie).is_empty()
            && has_attrs(&binding_cookie, &["Path=/accounts", "HttpOnly"])
            && row == want_row,
        format!(
            "form={} button={button} start={} endpoint={endpoint:?} client_id={client_id:?} redirect_uri={redirect_uri:?} state={} binding={:?} row={row:?} body={}",
            page.status,
            started.status,
            !state.is_empty(),
            redact(&binding_cookie),
            cut(&text(&started.body))
        ),
    );

    // [BG2] the callback for a state this browser started, with a code the dead token endpoint can
    // never exchange: the state is consumed and the browser goes back to the form with Google's
    // error and the start's return_to, where the form shows that error.
    let page = browse(|| ctx.http_noredirect.get(format!("{front}{LOGIN}")).header("X-Forwarded-For", BG2_CLIENT)).await?;
    let started = google_start(ctx, front, BG2_CLIENT, &page).await?;
    let state = query_param(reqwest::Url::parse(&started.location).ok().as_ref(), "state");
    let binding = cookie_value(&set_cookie(&started.cookies, BINDING));
    let back = callback(ctx, front, GOOGLE_CALLBACK, BG2_CLIENT, &state, &binding).await?;
    let landed = reqwest::Url::parse("http://front.invalid").and_then(|base| base.join(&back.location)).ok();
    let landed_path = landed.as_ref().map(|u| u.path().to_string()).unwrap_or_default();
    let error_key = query_param(landed.as_ref(), "google");
    let return_to = query_param(landed.as_ref(), "return_to");
    let row_after = oauth_state(pool, &state).await?;
    let shown = browse(|| ctx.http_noredirect.get(format!("{front}{}", back.location)).header("X-Forwarded-For", BG2_CLIENT)).await?;
    p.check(
        &format!("[BG2{tag}] GET {GOOGLE_CALLBACK} with a bad code (dead-port token URL) -> 303 {LOGIN}?google=error&return_to=<the start's>, state consumed, the form shows Google's error"),
        !state.is_empty()
            && !binding.is_empty()
            && back.status == 303
            && landed_path == LOGIN
            && error_key == "error"
            && return_to == BG_RETURN_TO
            && row_after.is_none()
            && shown.status == 200
            && text(&shown.body).contains(GOOGLE_FAILED),
        format!(
            "state={} binding={} callback={} loc={:?} row_after={row_after:?} form={} shows_error={} body={}",
            !state.is_empty(),
            !binding.is_empty(),
            back.status,
            back.location,
            shown.status,
            text(&shown.body).contains(GOOGLE_FAILED),
            cut(&text(&back.body))
        ),
    );

    // [BG3] states are scoped to the provider that minted them: an Epic JSON start's state, with
    // its own binding, is refused on Google's callback and left unconsumed, so Epic's callback still
    // redeems it (its failed exchange answering Epic's error redirect, never the 400 of a missing
    // state).
    let epic = browse(|| ctx.http_noredirect.post(format!("{front}{EPIC_START}")).header("X-Forwarded-For", BG3_CLIENT)).await?;
    let epic_url = serde_json::from_str::<serde_json::Value>(&epic.body)
        .ok()
        .and_then(|v| v.get("authorize_url").and_then(|u| u.as_str()).map(str::to_string))
        .unwrap_or_default();
    let state = query_param(reqwest::Url::parse(&epic_url).ok().as_ref(), "state");
    let binding = cookie_value(&set_cookie(&epic.cookies, BINDING));
    let minted = oauth_state(pool, &state).await?;
    let foreign = callback(ctx, front, GOOGLE_CALLBACK, BG3_CLIENT, &state, &binding).await?;
    let kept = oauth_state(pool, &state).await?;
    let own = callback(ctx, front, EPIC_CALLBACK, BG3_CLIENT, &state, &binding).await?;
    let row_after = oauth_state(pool, &state).await?;
    let epic_provider = |row: &Option<(String, String, Option<String>)>| row.as_ref().is_some_and(|(provider, _, _)| provider == "epic");
    p.check(
        &format!("[BG3{tag}] a state minted by POST {EPIC_START}, presented with its binding on {GOOGLE_CALLBACK} -> 400 and the row kept; the same state on {EPIC_CALLBACK} -> 303 /?epic=error (redeemed)"),
        epic.status == 200
            && !state.is_empty()
            && !binding.is_empty()
            && epic_provider(&minted)
            && foreign.status == 400
            && epic_provider(&kept)
            && own.status == 303
            && own.location == "/?epic=error"
            && row_after.is_none(),
        format!(
            "start={} state={} binding={} minted={minted:?} google_callback={} kept={kept:?} epic_callback={} epic_loc={:?} row_after={row_after:?}",
            epic.status,
            !state.is_empty(),
            !binding.is_empty(),
            foreign.status,
            own.status,
            own.location
        ),
    );
    Ok(())
}

/// POSTs the Google start as the form `page` was served: its login-CSRF cookie and double-submitted
/// value, and [`BG_RETURN_TO`], from `client`.
async fn google_start(ctx: &Ctx, front: &str, client: &str, page: &Answer) -> Result<Answer> {
    let csrf = cookie_value(&set_cookie(&page.cookies, LOGIN_CSRF));
    let form = [("csrf", csrf.as_str()), ("return_to", BG_RETURN_TO)];
    browse(|| {
        ctx.http_noredirect
            .post(format!("{front}{GOOGLE_START}"))
            .header("X-Forwarded-For", client)
            .header(COOKIE, format!("{LOGIN_CSRF}={csrf}"))
            .form(&form)
    })
    .await
}

/// A provider's callback as the browser holding `binding` reaches it, with an authorization code
/// the dead token endpoint cannot exchange.
async fn callback(ctx: &Ctx, front: &str, path: &str, client: &str, state: &str, binding: &str) -> Result<Answer> {
    browse(|| {
        ctx.http_noredirect
            .get(format!("{front}{path}"))
            .query(&[("code", "splitproof-bad-code"), ("state", state)])
            .header("X-Forwarded-For", client)
            .header(COOKIE, format!("{BINDING}={binding}"))
    })
    .await
}

/// The first value of the query parameter `name`, empty when `url` or the parameter is absent.
fn query_param(url: Option<&reqwest::Url>, name: &str) -> String {
    url.and_then(|u| u.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned())).unwrap_or_default()
}

/// The `accounts.oauth_states` row for `state` as `(provider, intent, return_to)`, none when absent.
async fn oauth_state(pool: &PgPool, state: &str) -> Result<Option<(String, String, Option<String>)>> {
    sqlx::query_as("SELECT provider, intent, return_to FROM accounts.oauth_states WHERE state = $1")
        .bind(state)
        .fetch_optional(pool)
        .await
        .context("read the oauth state row")
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
        &format!("[BR1{tag}] a burst on POST {LOGIN} from one X-Forwarded-For client -> 429, a second client's posts -> 403 (bucket passed) within the refill budget"),
        br1.limited && br1.b.timely() && br1.b.codes.iter().all(|code| *code == 403),
        br1.detail(),
    );

    // [BR2] the JSON Epic start draws from the same bucket kind, per client: A refused, B's
    // starts all answered (200 with an authorize_url).
    let epic = |client: &str| ctx.http_noredirect.post(format!("{front}{EPIC_START}")).header("X-Forwarded-For", client);
    let br2 = two_clients(&epic, EPIC_A, EPIC_B, EPIC_REFUSAL).await?;
    p.check(
        &format!("[BR2{tag}] a burst on POST {EPIC_START} from one X-Forwarded-For client -> 429, a second client's starts -> 200 within the refill budget"),
        br2.limited && br2.b.timely() && br2.b.codes.iter().all(|code| *code == 200),
        br2.detail(),
    );

    // [BR3] the hosted form and the Epic start share ONE bucket per client: a client drained on the
    // form is refused on its very next Epic starts. A separate Epic limiter would grant this client
    // a fresh burst, so every start would answer 200.
    let (drained, refused, gateway, refused_at) = drain(|| hosted(SHARED), HOSTED_REFUSAL).await?;
    let starts = run(|| epic(SHARED), EPIC_REFUSAL, refused_at).await?;
    p.check(
        &format!("[BR3{tag}] one X-Forwarded-For client refused on POST {LOGIN} -> its next POST {EPIC_START} -> 429 (one shared bucket) within the refill budget"),
        refused && starts.timely() && starts.codes.contains(&429),
        format!("form_sent={drained} form_limited={refused} form_gateway_429={gateway} epic {}", starts.detail()),
    );
    Ok(())
}

/// One two-client burst: how many requests client A sent until accounts refused it, the
/// gateway's own 429s met on the way, and client B's run after A's refusal.
struct Burst {
    a_sent: u32,
    limited: bool,
    gateway: u32,
    b: Run,
}

impl Burst {
    fn detail(&self) -> String {
        format!("a_sent={} a_limited={} a_gateway_429={} {}", self.a_sent, self.limited, self.gateway, self.b.detail())
    }
}

/// Requests sent back to back right after a client was refused: accounts' answers, the gateway's own
/// 429s met among them, and the time from just before the refused request was sent to the last answer.
struct Run {
    codes: Vec<u16>,
    gateway: u32,
    elapsed: Duration,
}

impl Run {
    /// Whether the run can tell two buckets from one. Accounts' bucket refills `REFILL_PER_SEC`
    /// tokens a second and may hold a fraction of a token when it refuses, so a run of
    /// `B_REQUESTS` measured at `run_budget_secs` or longer could be served by tokens refilled
    /// into the very bucket that refused, and a gateway 429
    /// among them adds a pause the timer cannot vouch for: either way the run proves nothing.
    fn timely(&self) -> bool {
        self.gateway == 0 && self.elapsed.as_secs_f64() < run_budget_secs()
    }

    fn detail(&self) -> String {
        let verdict = if self.timely() { "" } else { " INCONCLUSIVE: the run met a gateway 429 or outlasted the refill budget" };
        format!(
            "codes={:?} run_ms={} budget_ms={} run_gateway_429={}{verdict}",
            self.codes,
            self.elapsed.as_millis(),
            (run_budget_secs() * 1000.0) as u64,
            self.gateway,
        )
    }
}

/// The seconds a `B_REQUESTS` run may measure before accounts' refill alone could have answered it.
/// The refused bucket keeps a fraction `f < 1` of a token, so a shared bucket needs
/// `(B_REQUESTS - f) / REFILL_PER_SEC` seconds after the refusal, more than this budget; the run is
/// timed from before the refused request was sent, so its measured time is never shorter than the
/// time since the server's refusal.
fn run_budget_secs() -> f64 {
    (B_REQUESTS - 1) as f64 / REFILL_PER_SEC
}

/// Client A sends until accounts refuses it, then client B sends `B_REQUESTS` back to back, timed
/// from just before A's refused request was sent.
async fn two_clients(build: impl Fn(&str) -> reqwest::RequestBuilder, a: &str, b: &str, refusal: &str) -> Result<Burst> {
    let (a_sent, limited, gateway, refused_at) = drain(|| build(a), refusal).await?;
    let run = run(|| build(b), refusal, refused_at).await?;
    Ok(Burst { a_sent, limited, gateway, b: run })
}

/// Sends until accounts refuses (`refusal` in a 429's body; at most 100 requests): the requests
/// accounts answered, whether it refused, the gateway's own 429s met, and the instant taken just
/// before sending the last request accounts answered (the refused one when it refused), which is at
/// or before the server's refusal. A 429 without `refusal`
/// is the gateway's front-door limiter, not accounts': that request is sent again after a short
/// pause and is not counted as sent. It first waits `gateway_refill()`, so the drain and the timed run
/// after it (`accountsapi::LOGIN_LIMIT_BURST` + 1 + `B_REQUESTS` requests) start on a full gateway bucket instead of the one the previous
/// burst emptied; a run that still meets a gateway 429 fails as inconclusive.
async fn drain(build: impl Fn() -> reqwest::RequestBuilder, refusal: &str) -> Result<(u32, bool, u32, Instant)> {
    tokio::time::sleep(gateway_refill()).await;
    let (mut sent, mut limited, mut gateway) = (0u32, false, 0u32);
    let mut answered_from = Instant::now();
    while sent < 100 && !limited {
        let sending = Instant::now();
        match met(build(), refusal).await? {
            None => gateway += 1,
            Some(code) => {
                sent += 1;
                limited = code == 429;
                answered_from = sending;
            }
        }
        if gateway > 50 {
            bail!("the gateway's front door refused {gateway} requests while draining (sent={sent} limited={limited})");
        }
    }
    Ok((sent, limited, gateway, answered_from))
}

/// `B_REQUESTS` requests accounts answered, sent back to back and timed from `refused_at`, the
/// instant `drain` took just before sending the request accounts refused. A gateway 429 is retried
/// as in `drain` and counted.
async fn run(build: impl Fn() -> reqwest::RequestBuilder, refusal: &str, refused_at: Instant) -> Result<Run> {
    let started = refused_at;
    let mut run = Run { codes: Vec::new(), gateway: 0, elapsed: Duration::ZERO };
    while run.codes.len() < B_REQUESTS {
        match met(build(), refusal).await? {
            None => run.gateway += 1,
            Some(code) => run.codes.push(code),
        }
        if run.gateway > 50 {
            run.elapsed = started.elapsed();
            bail!("the gateway's front door refused {} requests: {}", run.gateway, run.detail());
        }
    }
    run.elapsed = started.elapsed();
    Ok(run)
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
