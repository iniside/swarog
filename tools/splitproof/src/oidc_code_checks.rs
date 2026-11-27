//! [OC1]-[OC11]: the OIDC authorization code flow accounts serves under `/accounts/oidc`, proven
//! through each front door: gateway-svc's passthrough into accounts-svc (and its op dispatch to
//! accounts-svc for `/accounts/refresh`, its own token verifier for `/accounts/me`) in the split, the
//! monolith's own router in parity.
//!
//! The pass creates its three clients through the portal's Create client action (`cp.invoke` into
//! accounts): a public one (P), a confidential one (C, its secret read off the one-shot reveal) and a
//! public one with a Lore audience (L), all registering `REDIRECT_URI`. The player registers over
//! the op path, signs in on the hosted form as a browser does (sent there by `/authorize`, brought
//! back by `return_to`), and every later `/authorize` carries that `swarog_session` by hand.
//!
//! - [OC1] the portal creates P, C and L: P and L with no secret digest, C with the digest of the
//!   secret the reveal shows, each storing `REDIRECT_URI` and its audience.
//! - [OC2] signed out, `/authorize` with PKCE sends the browser to the hosted form, the sign-in
//!   returns it to the same request, which answers 303 to `REDIRECT_URI` with a code and the state;
//!   `/token` exchanges the code for a Bearer JWT, a refresh token and an id_token, and the access
//!   token is accepted by `/accounts/me` as the player.
//! - [OC3] `/authorize` naming a redirect URI P does not register is a 400 page with no `Location`
//!   and mints no code (once-wrong: the `redirect_uris` guard of the client lookup).
//! - [OC4] a code exchanged with another `redirect_uri` than its `/authorize` named is `invalid_grant`
//!   (once-wrong: the `redirect_uri` compare of the code grant).
//! - [OC5] a code exchanged with a well-formed verifier that is not the one its challenge hashes is
//!   `invalid_grant` (once-wrong: the S256 compare of the code grant).
//! - [OC6] C, authenticated by Basic, presenting P's refresh token at `/token` is `invalid_grant` and
//!   leaves the token unconsumed (once-wrong: the `RefreshPresenter::Client` arm of
//!   `refresh_allowed_tx`).
//! - [OC7] P's refresh token, the one [OC6] presented, rotates at `/token` in the same family, and the
//!   new access token is accepted by `/accounts/me`.
//! - [OC8] C's code exchange with Basic mints; C's refresh token on the op path `/accounts/refresh` is
//!   exactly 401 with the token unconsumed, and it still rotates at `/token` (once-wrong: the
//!   `RefreshPresenter::OpPath` arm of `refresh_allowed_tx`, the Rotate verdict).
//! - [OC9] C's refresh token consumed by [OC8]'s rotation, its `used_at` moved an hour back so the
//!   presentation is a reuse outside the grace window (persisted state, no wall clock), on the op path
//!   is exactly 401 and kills nothing: C's family row stands, no family revocation is written, and its
//!   successor still rotates at `/token` (once-wrong: the Revoke verdict killing a confidential family
//!   for the op path, ba595de3).
//! - [OC10] L's code exchange mints an access token whose `aud` is `LORE_AUDIENCE`; `/accounts/me`
//!   refuses it with exactly 401 while C's Swarog-audience token, asked the same way, answers 200.
//! - [OC11] P revokes its [OC7] access token at `/revoke` (200): its family row goes with one family
//!   revocation, the bearer is refused by `/accounts/me` within `accounts_token_checks::REFUSAL_BOUND`
//!   (two `accountsapi::TOKEN_STATE_PULL_INTERVAL` periods, polled, never slept out), its refresh token
//!   is `invalid_grant` at `/token`, and C's bearer stays live.
//! - [OC0] / [OC0m] report only a fault no check records: a transport error, a failed database read,
//!   or a fixture the later checks need (a client, the browser session, a code) that a recorded check
//!   already showed missing.
//!
//! Break runs (each a full splitproof run, then reverted; 2026-11-27, step 6; green run
//! splitproof-44368, every [OC] and [SK] check green):
//! - The `redirect_uris` guard dropped from the client lookup (`modules/accounts/src/web/authorize.rs`)
//!   and the `redirect_uri` compare dropped from the code grant (`modules/accounts/src/web/token.rs`),
//!   run splitproof-48484: OC3, OC4, OC3m and OC4m the only new reds (`code=303` to the unregistered
//!   URI with a code minted; `code=200` with an access token), the rest green.
//! - The S256 compare of the code grant made `false &&` (`token.rs`), run splitproof-54184: OC5 and
//!   OC5m the only new reds (`code=200 access_token=true`), the rest green.
//! - The `RefreshPresenter::Client` arm of `refresh_allowed_tx` made `true ||`
//!   (`modules/accounts/src/lib.rs`), run splitproof-65028: OC6 and OC6m the only new reds
//!   (`code=200`, the token consumed); OC7 and OC7m green.
//! - The `RefreshPresenter::OpPath` arm made `true ||` (`lib.rs`), run splitproof-66856: OC8, OC9,
//!   OC8m and OC9m red (`op_path=200` with the token consumed; then the reuse revokes C's family,
//!   `family_rows=0 family_revocations=1`), and OC10, OC11, OC10m and OC11m red after them, because
//!   their Swarog-audience control is C's bearer, which that revocation killed (`control_me=401`,
//!   `other_me=401`; the Lore token is still 401). OC1-OC7 green.

use std::time::Instant;

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use reqwest::header::{COOKIE, LOCATION};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::accounts_admin_checks::toast;
use crate::accounts_browser_checks::{browse, cookie_value, set_cookie, Answer};
use crate::accounts_token_checks::until_refused;
use crate::apikeys_admin_checks::{fetch, patient, secret_of};
use crate::characters_admin_checks::{attr_values, cut, text};
use crate::{accounts_me, extract_form_fields, pass_nonce, post_json, Ctx, Proof};

/// The OIDC clients page, where Create client is a head action.
const CLIENTS_PAGE: &str = "/admin/p/accounts/clients";
const AUTHORIZE: &str = "/accounts/oidc/authorize";
const TOKEN: &str = "/accounts/oidc/token";
const REVOKE: &str = "/accounts/oidc/revoke";
const LOGIN: &str = "/accounts/oidc/login";
const SESSION: &str = "swarog_session";
const LOGIN_CSRF: &str = "swarog_login_csrf";
/// The redirect URI every client of the pass registers: http on a loopback host, never dialled,
/// since no request here follows a redirect.
const REDIRECT_URI: &str = "http://127.0.0.1:9/oc/callback";
/// A redirect URI on the same host that no client registers.
const FOREIGN_REDIRECT_URI: &str = "http://127.0.0.1:9/oc/elsewhere";
/// L's audience, which the gateway's verifier does not accept.
const LORE_AUDIENCE: &str = "lore";
/// The `X-Forwarded-For` client of the pass's hosted sign-in, from TEST-NET-3 and used by no
/// other check, so no sign-in bucket another check drained can refuse it.
const SIGN_IN_CLIENT: &str = "203.0.113.79";

pub(super) async fn assertions(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    if let Err(e) = checks(ctx, pool, p, front, admin, tag).await {
        p.check(&format!("[OC0{tag}] OIDC authorization code pass completes"), false, format!("{e:#}"));
    }
    Ok(())
}

async fn checks(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    let nonce = pass_nonce(tag);

    // [OC1] the three clients, created through the portal action.
    let grants_both = [("grant_types", "authorization_code"), ("grant_types", "refresh_token")];
    let mut public = vec![("redirect_uris", REDIRECT_URI), ("scopes", "openid")];
    public.extend(grants_both);
    let mut confidential = public.clone();
    confidential.push(("confidential", "true"));
    let lore = [("grant_types", "authorization_code"), ("redirect_uris", REDIRECT_URI), ("audience", LORE_AUDIENCE)];
    let pc = create_client(admin, front, pool, &format!("Oc public {nonce}"), &public).await?;
    let cc = create_client(admin, front, pool, &format!("Oc confidential {nonce}"), &confidential).await?;
    let lc = create_client(admin, front, pool, &format!("Oc lore {nonce}"), &lore).await?;
    let secret = cc.secret.clone().unwrap_or_default();
    let stored = |c: &Created, audience: &str, digest: Option<String>| {
        c.status == 303
            && c.row.as_ref().is_some_and(|r| {
                r.client_id.starts_with("c_")
                    && r.audience == audience
                    && r.redirect_uris == [REDIRECT_URI]
                    && r.secret_digest == digest
            })
    };
    p.check(
        &format!("[OC1{tag}] Create client through the portal: a public, a confidential (secret revealed once, its digest stored) and a Lore-audience client"),
        stored(&pc, accountsapi::SWAROG_AUDIENCE, None)
            && pc.toast.starts_with("Client created")
            && pc.secret.is_none()
            && secret.starts_with("cs_")
            && stored(&cc, accountsapi::SWAROG_AUDIENCE, Some(URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_bytes()))))
            && stored(&lc, LORE_AUDIENCE, None)
            && lc.secret.is_none(),
        format!(
            "public=({}) confidential=({} secret_shown={}) lore=({})",
            pc.detail(),
            cc.detail(),
            !secret.is_empty(),
            lc.detail()
        ),
    );
    let (Some(pr), Some(cr), Some(lr)) = (&pc.row, &cc.row, &lc.row) else {
        bail!("fixture: [OC1] created no row for one of the clients");
    };
    if secret.is_empty() {
        bail!("fixture: [OC1] revealed no secret for the confidential client {}", cr.client_id);
    }
    let (p_id, c_id, l_id) = (pr.client_id.as_str(), cr.client_id.as_str(), lr.client_id.as_str());
    let p_auth = Auth::Public(p_id);
    let c_auth = Auth::Basic(c_id, &secret);

    // The player, registered over the op path; the browser signs in with the same password.
    let email = format!("oc-{nonce}@test.local");
    let password = format!("pw-{nonce}");
    let register = serde_json::json!({ "email": email, "password": password, "displayName": "OcA", "clientId": accountsapi::DEV_GAME_CLIENT_ID });
    let (reg_code, reg) = post_json(ctx, &format!("{front}/accounts/register"), register).await?;
    let pid = s(&reg, "player_id");
    if !(200..300).contains(&reg_code) || pid.is_empty() {
        bail!("fixture: /accounts/register -> {reg_code} {reg}");
    }

    // [OC2] signed out: /authorize -> the hosted form -> back to the same request -> a code; /token
    // exchanges it, and /accounts/me accepts the access token.
    let v2 = verifier(&nonce, "p2");
    let state = format!("st-{nonce}");
    let ch2 = challenge(&v2);
    let request = authorize_request(p_id, REDIRECT_URI, "openid", &ch2, &state);
    let out = browse(|| ctx.http_noredirect.get(format!("{front}{AUTHORIZE}")).query(&request)).await?;
    let return_to = query(&format!("{front}{}", out.location), "return_to");
    let signed = sign_in(ctx, front, &out.location, &return_to, &email, &password).await?;
    let session = cookie_value(&set_cookie(&signed.cookies, SESSION));
    let back = browse(|| ctx.http_noredirect.get(format!("{front}{return_to}")).header(COOKIE, format!("{SESSION}={session}"))).await?;
    let code = query(&back.location, "code");
    let exchange = [("grant_type", "authorization_code"), ("code", code.as_str()), ("redirect_uri", REDIRECT_URI), ("code_verifier", v2.as_str())];
    let (tok_code, tok) = post_form(ctx, &format!("{front}{TOKEN}"), &p_auth, &exchange).await?;
    let (p_access, p_refresh) = (s(&tok, "access_token"), s(&tok, "refresh_token"));
    let (me_code, me) = accounts_me(ctx, front, &p_access).await?;
    let p_sid = s(&jwt_parts(&p_access).1, "sid");
    p.check(
        &format!("[OC2{tag}] signed out, /authorize with PKCE -> the hosted form -> back to /authorize -> 303 with a code; /token -> a Bearer JWT, refresh and id_token that /accounts/me accepts"),
        out.status == 303
            && out.location.starts_with(&format!("{LOGIN}?return_to="))
            && return_to.starts_with(&format!("{AUTHORIZE}?"))
            && signed.status == 303
            && signed.location == return_to
            && !session.is_empty()
            && back.status == 303
            && back.location.starts_with(&format!("{REDIRECT_URI}?"))
            && !code.is_empty()
            && query(&back.location, "state") == state
            && tok_code == 200
            && s(&tok, "token_type") == "Bearer"
            && p_access.split('.').count() == 3
            && !p_refresh.is_empty()
            && !s(&tok, "id_token").is_empty()
            && !p_sid.is_empty()
            && me_code == 200
            && s(&me, "player_id") == pid,
        format!(
            "authorize={} loc={:?} sign_in={} loc_is_return_to={} session={} back={} code={} state_ok={} token={tok_code} error={:?} \
             refresh={} id_token={} me={me_code} me_pid={:?}",
            out.status,
            cut(&out.location),
            signed.status,
            signed.location == return_to,
            !session.is_empty(),
            back.status,
            !code.is_empty(),
            query(&back.location, "state") == state,
            s(&tok, "error"),
            !p_refresh.is_empty(),
            !s(&tok, "id_token").is_empty(),
            s(&me, "player_id")
        ),
    );
    if session.is_empty() || p_access.is_empty() || p_refresh.is_empty() {
        bail!("fixture: [OC2] left no browser session or no P tokens for the later checks");
    }
    let browser = Browser { ctx, front, session: &session };

    // [OC3] a redirect URI P does not register: a page, never a redirect, and no code minted.
    let codes_before = count(pool, "SELECT count(*) FROM accounts.oidc_codes WHERE client_id = $1", p_id).await?;
    let foreign = browser.authorize(p_id, FOREIGN_REDIRECT_URI, "openid", &challenge(&verifier(&nonce, "p3")), "st3").await?;
    let codes_after = count(pool, "SELECT count(*) FROM accounts.oidc_codes WHERE client_id = $1", p_id).await?;
    p.check(
        &format!("[OC3{tag}] /authorize naming a redirect_uri the client does not register -> exactly 400, no Location, no code minted"),
        foreign.status == 400 && foreign.location.is_empty() && codes_after == codes_before,
        format!("code={} loc={:?} codes_before={codes_before} after={codes_after}", foreign.status, cut(&foreign.location)),
    );

    // [OC4] a code exchanged with another redirect_uri than its /authorize named.
    let v4 = verifier(&nonce, "p4");
    let code4 = browser.code(p_id, "openid", &v4).await?;
    let (c4, t4) = post_form(
        ctx,
        &format!("{front}{TOKEN}"),
        &p_auth,
        &[("grant_type", "authorization_code"), ("code", code4.as_str()), ("redirect_uri", FOREIGN_REDIRECT_URI), ("code_verifier", v4.as_str())],
    )
    .await?;
    p.check(
        &format!("[OC4{tag}] /token with a redirect_uri other than the code's -> 400 invalid_grant, no token"),
        c4 == 400 && s(&t4, "error") == "invalid_grant" && s(&t4, "access_token").is_empty(),
        format!("code={c4} error={:?} access_token={}", s(&t4, "error"), !s(&t4, "access_token").is_empty()),
    );

    // [OC5] a code exchanged with a well-formed verifier its challenge does not hash.
    let v5 = verifier(&nonce, "p5");
    let code5 = browser.code(p_id, "openid", &v5).await?;
    let wrong = verifier(&nonce, "p5-wrong");
    let (c5, t5) = post_form(
        ctx,
        &format!("{front}{TOKEN}"),
        &p_auth,
        &[("grant_type", "authorization_code"), ("code", code5.as_str()), ("redirect_uri", REDIRECT_URI), ("code_verifier", wrong.as_str())],
    )
    .await?;
    p.check(
        &format!("[OC5{tag}] /token with a well-formed code_verifier that is not the challenge's -> 400 invalid_grant, no token"),
        c5 == 400 && s(&t5, "error") == "invalid_grant" && s(&t5, "access_token").is_empty(),
        format!("code={c5} error={:?} access_token={}", s(&t5, "error"), !s(&t5, "access_token").is_empty()),
    );

    // [OC6] C presents P's refresh token: refused, and the token is left unconsumed.
    let refresh = |token: &str| [("grant_type", "refresh_token"), ("refresh_token", token)].map(|(k, v)| (k, v.to_string()));
    let (c6, t6) = post_form(ctx, &format!("{front}{TOKEN}"), &c_auth, &as_str(&refresh(&p_refresh))).await?;
    let p_unused = unused(pool, &p_refresh).await?;
    p.check(
        &format!("[OC6{tag}] a confidential client presenting another client's refresh token at /token -> 400 invalid_grant, the token unconsumed"),
        c6 == 400 && s(&t6, "error") == "invalid_grant" && s(&t6, "access_token").is_empty() && p_unused == Some(true),
        format!("code={c6} error={:?} access_token={} unused={p_unused:?}", s(&t6, "error"), !s(&t6, "access_token").is_empty()),
    );

    // [OC7] P's own refresh token rotates at /token in the same family.
    let (c7, t7) = post_form(ctx, &format!("{front}{TOKEN}"), &p_auth, &as_str(&refresh(&p_refresh))).await?;
    let (p_access2, p_refresh2) = (s(&t7, "access_token"), s(&t7, "refresh_token"));
    let (me7, me7_body) = accounts_me(ctx, front, &p_access2).await?;
    let sid7 = s(&jwt_parts(&p_access2).1, "sid");
    p.check(
        &format!("[OC7{tag}] the refresh grant at /token rotates the client's own refresh token in its family; the new bearer is accepted by /accounts/me"),
        c7 == 200
            && !p_refresh2.is_empty()
            && p_refresh2 != p_refresh
            && sid7 == p_sid
            && me7 == 200
            && s(&me7_body, "player_id") == pid,
        format!("code={c7} error={:?} new_refresh={} same_family={} me={me7}", s(&t7, "error"), !p_refresh2.is_empty() && p_refresh2 != p_refresh, sid7 == p_sid),
    );

    // [OC8] C's code exchange with Basic; its refresh token on the op path is refused unconsumed
    // and still rotates at /token.
    let v8 = verifier(&nonce, "c8");
    let code8 = browser.code(c_id, "openid", &v8).await?;
    let (c8, t8) = post_form(
        ctx,
        &format!("{front}{TOKEN}"),
        &c_auth,
        &[("grant_type", "authorization_code"), ("code", code8.as_str()), ("redirect_uri", REDIRECT_URI), ("code_verifier", v8.as_str())],
    )
    .await?;
    let c_refresh1 = s(&t8, "refresh_token");
    let c_sid = s(&jwt_parts(&s(&t8, "access_token")).1, "sid");
    let refresh_url = format!("{front}/accounts/refresh");
    let (op8, _) = post_json(ctx, &refresh_url, serde_json::json!({ "refresh_token": c_refresh1 })).await?;
    let c_unused = unused(pool, &c_refresh1).await?;
    let (rot8, rot8_body) = post_form(ctx, &format!("{front}{TOKEN}"), &c_auth, &as_str(&refresh(&c_refresh1))).await?;
    let c_refresh2 = s(&rot8_body, "refresh_token");
    p.check(
        &format!("[OC8{tag}] a confidential client's refresh token on the op path /accounts/refresh -> exactly 401, unconsumed; it still rotates at /token"),
        c8 == 200 && !c_refresh1.is_empty() && !c_sid.is_empty() && op8 == 401 && c_unused == Some(true) && rot8 == 200 && !c_refresh2.is_empty(),
        format!(
            "exchange={c8} error={:?} refresh={} op_path={op8} unused={c_unused:?} rotate={rot8} rotate_error={:?}",
            s(&t8, "error"),
            !c_refresh1.is_empty(),
            s(&rot8_body, "error")
        ),
    );
    if c_refresh1.is_empty() || c_refresh2.is_empty() {
        bail!("fixture: [OC8] left no rotated C refresh token for [OC9]");
    }

    // [OC9] the consumed C token, moved outside the grace window, reused on the op path: refused,
    // and the family it would have killed stands.
    let moved = sqlx::query(
        "UPDATE accounts.refresh_tokens SET used_at = now() - interval '1 hour' WHERE token = $1 AND used_at IS NOT NULL",
    )
    .bind(&c_refresh1)
    .execute(pool)
    .await
    .context("move the consumed C refresh token out of the grace window")?
    .rows_affected();
    let (op9, _) = post_json(ctx, &refresh_url, serde_json::json!({ "refresh_token": c_refresh1 })).await?;
    let family = count(pool, "SELECT count(*) FROM accounts.token_families WHERE id::text = $1", &c_sid).await?;
    let revocations = count(pool, "SELECT count(*) FROM accounts.token_revocations WHERE family_id::text = $1", &c_sid).await?;
    let (rot9, rot9_body) = post_form(ctx, &format!("{front}{TOKEN}"), &c_auth, &as_str(&refresh(&c_refresh2))).await?;
    let c_access3 = s(&rot9_body, "access_token");
    p.check(
        &format!("[OC9{tag}] a confidential client's consumed refresh token reused outside the grace window on /accounts/refresh -> exactly 401, its family kept"),
        moved == 1 && op9 == 401 && family == 1 && revocations == 0 && rot9 == 200 && !c_access3.is_empty(),
        format!("moved={moved} op_path={op9} family_rows={family} family_revocations={revocations} successor_rotate={rot9} error={:?}", s(&rot9_body, "error")),
    );

    // [OC10] a Lore-audience token from the same flow, refused by the gateway's verifier; C's
    // Swarog-audience token, asked the same way, is the control.
    let v10 = verifier(&nonce, "l10");
    let code10 = browser.code(l_id, "", &v10).await?;
    let (c10, t10) = post_form(
        ctx,
        &format!("{front}{TOKEN}"),
        &Auth::Public(l_id),
        &[("grant_type", "authorization_code"), ("code", code10.as_str()), ("redirect_uri", REDIRECT_URI), ("code_verifier", v10.as_str())],
    )
    .await?;
    let l_access = s(&t10, "access_token");
    let (l_header, l_claims) = jwt_parts(&l_access);
    let (lore_me, _) = accounts_me(ctx, front, &l_access).await?;
    let (control_me, _) = accounts_me(ctx, front, &c_access3).await?;
    p.check(
        &format!("[OC10{tag}] a Lore-audience access token from /token -> exactly 401 on /accounts/me; a Swarog-audience one -> 200"),
        c10 == 200
            && s(&l_claims, "aud") == LORE_AUDIENCE
            && s(&l_header, "typ") == "at+jwt"
            && s(&l_claims, "sub") == pid
            && lore_me == 401
            && control_me == 200,
        format!(
            "token={c10} error={:?} typ={:?} aud={:?} sub_is_player={} lore_me={lore_me} control_me={control_me}",
            s(&t10, "error"),
            s(&l_header, "typ"),
            s(&l_claims, "aud"),
            s(&l_claims, "sub") == pid
        ),
    );

    // [OC11] P revokes its access token: the family goes, the bearer is refused within the pull
    // bound, the refresh token is dead, and C's bearer stays live.
    let (c11, _) = post_form(ctx, &format!("{front}{REVOKE}"), &p_auth, &[("token", p_access2.as_str()), ("token_type_hint", "access_token")]).await?;
    let since = Instant::now();
    let p_family = count(pool, "SELECT count(*) FROM accounts.token_families WHERE id::text = $1", &p_sid).await?;
    let p_revocations = count(pool, "SELECT count(*) FROM accounts.token_revocations WHERE family_id::text = $1", &p_sid).await?;
    let bearer = until_refused(since, || async { Ok(accounts_me(ctx, front, &p_access2).await?.0) }).await?;
    let (dead, dead_body) = post_form(ctx, &format!("{front}{TOKEN}"), &p_auth, &as_str(&refresh(&p_refresh2))).await?;
    let (c_me, _) = accounts_me(ctx, front, &c_access3).await?;
    p.check(
        &format!("[OC11{tag}] /revoke of a client's access token -> 200; its family is revoked, the bearer refused within the pull bound, the refresh token invalid_grant; another family stays live"),
        c11 == 200
            && p_family == 0
            && p_revocations == 1
            && bearer.refused
            && dead == 400
            && s(&dead_body, "error") == "invalid_grant"
            && c_me == 200,
        format!(
            "revoke={c11} family_rows={p_family} family_revocations={p_revocations} bearer=({bearer}) refresh={dead} error={:?} other_me={c_me}",
            s(&dead_body, "error")
        ),
    );
    Ok(())
}

/// How a client authenticates at `/token` and `/revoke`: a public one by `client_id` in the form, a
/// confidential one by `client_secret_basic`.
enum Auth<'a> {
    Public(&'a str),
    Basic(&'a str, &'a str),
}

/// POSTs `form` as `auth` past the gateway's 429s; answers the status and the JSON body (`null`
/// for an empty or non-JSON one).
async fn post_form(ctx: &Ctx, url: &str, auth: &Auth<'_>, form: &[(&str, &str)]) -> Result<(u16, Value)> {
    let mut fields = form.to_vec();
    if let Auth::Public(id) = auth {
        fields.push(("client_id", *id));
    }
    let r = patient(|| {
        let req = ctx.http.post(url).form(&fields);
        match auth {
            Auth::Basic(id, secret) => req.basic_auth(id, Some(secret)),
            Auth::Public(_) => req,
        }
    })
    .await
    .with_context(|| format!("POST {url}"))?;
    let code = r.status().as_u16();
    let body = r.text().await.with_context(|| format!("read POST {url}"))?;
    Ok((code, serde_json::from_str(&body).unwrap_or(Value::Null)))
}

/// Borrows an owned form for [`post_form`].
fn as_str<'a>(form: &'a [(&'a str, String)]) -> Vec<(&'a str, &'a str)> {
    form.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

/// The signed-in browser of the pass: its front and the `swarog_session` it presents.
struct Browser<'a> {
    ctx: &'a Ctx,
    front: &'a str,
    session: &'a str,
}

impl Browser<'_> {
    /// `/authorize` as the signed-in browser asks it, never following the redirect.
    async fn authorize(&self, client_id: &str, redirect_uri: &str, scope: &str, challenge: &str, state: &str) -> Result<Answer> {
        let request = authorize_request(client_id, redirect_uri, scope, challenge, state);
        let cookie = format!("{SESSION}={}", self.session);
        browse(|| self.ctx.http_noredirect.get(format!("{}{AUTHORIZE}", self.front)).query(&request).header(COOKIE, &cookie)).await
    }

    /// A code for `client_id` at `REDIRECT_URI` bound to `verifier`'s challenge; a fixture the
    /// checks after [OC2] need, so its absence is an error.
    async fn code(&self, client_id: &str, scope: &str, verifier: &str) -> Result<String> {
        let back = self.authorize(client_id, REDIRECT_URI, scope, &challenge(verifier), "st").await?;
        let code = query(&back.location, "code");
        if back.status != 303 || code.is_empty() {
            bail!("fixture: /authorize for {client_id} -> {} loc={:?}: {}", back.status, cut(&back.location), cut(&text(&back.body)));
        }
        Ok(code)
    }
}

/// The query of an authorization request with PKCE S256; an empty `scope` counts as absent.
fn authorize_request<'a>(client_id: &'a str, redirect_uri: &'a str, scope: &'a str, challenge: &'a str, state: &'a str) -> [(&'static str, &'a str); 7] {
    [
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("scope", scope),
        ("state", state),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
    ]
}

/// The hosted form as a browser fills it after `/authorize` sent it to `page`: GET it for the login-CSRF
/// cookie, then POST the credentials with `return_to`, both as the pass's own `X-Forwarded-For` client.
async fn sign_in(ctx: &Ctx, front: &str, page: &str, return_to: &str, email: &str, password: &str) -> Result<Answer> {
    let form_page = browse(|| ctx.http_noredirect.get(format!("{front}{page}")).header("X-Forwarded-For", SIGN_IN_CLIENT)).await?;
    let csrf = cookie_value(&set_cookie(&form_page.cookies, LOGIN_CSRF));
    let form = [("csrf", csrf.as_str()), ("return_to", return_to), ("email", email), ("password", password)];
    browse(|| {
        ctx.http_noredirect
            .post(format!("{front}{LOGIN}"))
            .header("X-Forwarded-For", SIGN_IN_CLIENT)
            .header(COOKIE, format!("{LOGIN_CSRF}={csrf}"))
            .form(&form)
    })
    .await
}

/// A PKCE verifier of 43 or more unreserved characters, distinct per `label`.
fn verifier(nonce: &str, label: &str) -> String {
    format!("oc-{nonce}-{label}-0123456789abcdefghijklmnopqrstuvwxyz")
}

/// The S256 challenge of `verifier` (RFC 7636 section 4.2).
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// The decoded value of the query parameter `name` of the absolute `url`, empty when absent.
fn query(url: &str, name: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned()))
        .unwrap_or_default()
}

/// The decoded JOSE header and claims of a compact JWT, `null` for a part that does not decode.
fn jwt_parts(token: &str) -> (Value, Value) {
    let part = |i: usize| {
        token
            .split('.')
            .nth(i)
            .and_then(|p| URL_SAFE_NO_PAD.decode(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null)
    };
    (part(0), part(1))
}

/// The string member `k` of `v`, empty when absent.
fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
}

async fn count(pool: &PgPool, sql: &str, a: &str) -> Result<i64> {
    sqlx::query_scalar(sql).bind(a).fetch_one(pool).await.with_context(|| format!("count: {sql}"))
}

/// Whether the refresh token is held and unconsumed; `None` when no row holds it.
async fn unused(pool: &PgPool, token: &str) -> Result<Option<bool>> {
    sqlx::query_scalar("SELECT used_at IS NULL FROM accounts.refresh_tokens WHERE token = $1")
        .bind(token)
        .fetch_optional(pool)
        .await
        .context("read a refresh token row")
}

/// One client as accounts stores it.
struct ClientRow {
    client_id: String,
    audience: String,
    redirect_uris: Vec<String>,
    secret_digest: Option<String>,
}

/// What Create client answered: the POST's status, the toast it landed on, the revealed secret,
/// and the row it stored, read by its display name.
struct Created {
    status: u16,
    toast: String,
    secret: Option<String>,
    row: Option<ClientRow>,
}

impl Created {
    fn detail(&self) -> String {
        let row = self.row.as_ref().map(|r| {
            format!("id={} aud={} redirect_uris={:?} digest={}", r.client_id, r.audience, r.redirect_uris, r.secret_digest.is_some())
        });
        format!("post={} toast={:?} secret={} row={row:?}", self.status, self.toast, self.secret.is_some())
    }
}

/// Opens Create client on the clients page as the browser does, posts `fields` (a Multi field's key
/// repeated) with `display`, follows the PRG `Location` and reads its toast and any reveal.
async fn create_client(admin: &reqwest::Client, front: &str, pool: &PgPool, display: &str, fields: &[(&str, &str)]) -> Result<Created> {
    let (page_code, page) = fetch(admin, &format!("{front}{CLIENTS_PAGE}")).await?;
    let Some(opener) = attr_values(&page, "hx-get").into_iter().find(|u| u.starts_with("/admin/_action/accounts/create-client")) else {
        bail!("fixture: no accounts/create-client on {CLIENTS_PAGE}: status {page_code}: {}", cut(&text(&page)));
    };
    let (modal_code, modal) = fetch(admin, &format!("{front}{opener}")).await?;
    let modal_fields = extract_form_fields(&modal);
    let pick = |name: &str| modal_fields.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).unwrap_or_default();
    let (csrf, form_id) = (pick("_csrf"), pick("_form"));
    if modal_code != 200 || csrf.is_empty() || form_id.is_empty() {
        bail!("fixture: open {opener}: status {modal_code}: {}", cut(&text(&modal)));
    }
    let mut form = vec![("_csrf", csrf.as_str()), ("_form", form_id.as_str()), ("_return", CLIENTS_PAGE), ("display_name", display)];
    form.extend_from_slice(fields);
    let r = patient(|| admin.post(format!("{front}/admin/_action/accounts/create-client")).form(&form)).await?;
    let status = r.status().as_u16();
    let location = r.headers().get(LOCATION).and_then(|v| v.to_str().ok()).map(str::to_string);
    let posted = r.text().await.unwrap_or_default();
    let (toast, secret) = match location {
        Some(location) => {
            let (_, landed) = fetch(admin, &format!("{front}{location}")).await?;
            (toast(&landed), secret_of(&landed).filter(|s| !s.is_empty()))
        }
        None => (cut(&text(&posted)), None),
    };
    let row: Option<(String, String, Vec<String>, Option<String>)> = sqlx::query_as(
        "SELECT client_id, audience, redirect_uris, secret_digest FROM accounts.oidc_clients WHERE display_name = $1",
    )
    .bind(display)
    .fetch_optional(pool)
    .await
    .context("read a created client")?;
    let row = row.map(|(client_id, audience, redirect_uris, secret_digest)| ClientRow { client_id, audience, redirect_uris, secret_digest });
    Ok(Created { status, toast, secret, row })
}
