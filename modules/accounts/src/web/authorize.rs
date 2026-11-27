//! `GET /accounts/oidc/authorize`: the authorization endpoint of the code flow, with PKCE S256
//! for every client. A signed-in browser gets a code at once, with no consent screen; a
//! signed-out one signs in on the hosted form and comes back to the same request.

use std::fmt::Write as _;
use std::sync::Arc;

use accountsapi::grant_types::AUTHORIZATION_CODE;
use axum::extract::{RawQuery, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use serde::Serialize;

use super::{held_session_digest, see_other, Web, LOGIN, MAX_RETURN_TO_BYTES};
use crate::clients::{canonical_scopes, ClientRow};
use crate::codes::NewCode;

pub(crate) const AUTHORIZE: &str = "/accounts/oidc/authorize";
pub(crate) const MAX_STATE_BYTES: usize = 256;
pub(crate) const MAX_NONCE_BYTES: usize = 128;
const CODE_CHALLENGE_CHARS: usize = 43;

const INVALID_REQUEST: &str = "invalid_request";
const UNAUTHORIZED_CLIENT: &str = "unauthorized_client";
const UNSUPPORTED_RESPONSE_TYPE: &str = "unsupported_response_type";
const INVALID_SCOPE: &str = "invalid_scope";
const LOGIN_REQUIRED: &str = "login_required";

const BAD_CLIENT_ERROR: &str = "This sign-in link names an unknown application, or an address it does not return to.";
const BUSY_ERROR: &str = "Sign-in is unavailable. Please try again in a moment.";

/// The parameters a request is read for, in the order a rebuilt request carries them.
const PARAMS: [&str; 9] = [
    "response_type",
    "client_id",
    "redirect_uri",
    "scope",
    "state",
    "code_challenge",
    "code_challenge_method",
    "nonce",
    "prompt",
];

#[derive(Default)]
struct AuthRequest {
    values: [Option<String>; PARAMS.len()],
    repeated: [bool; PARAMS.len()],
}

impl AuthRequest {
    /// The known parameters of `query`; an empty value counts as absent (RFC 6749 §3.1).
    fn parse(query: &str) -> AuthRequest {
        let mut req = AuthRequest::default();
        for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
            let Some(i) = PARAMS.iter().position(|p| *p == name) else { continue };
            if value.is_empty() {
                continue;
            }
            req.repeated[i] |= req.values[i].is_some();
            req.values[i] = Some(value.into_owned());
        }
        req
    }

    /// The value of `name` when it was sent exactly once.
    fn single(&self, name: &str) -> Option<&str> {
        let i = PARAMS.iter().position(|p| *p == name)?;
        if self.repeated[i] {
            return None;
        }
        self.values[i].as_deref()
    }

    fn any_repeated(&self) -> bool {
        self.repeated.iter().any(|r| *r)
    }

    /// The request rebuilt as a path under the prefix, every value percent-encoded, so no
    /// `//`, `#` or backslash survives into it and `safe_return_to` keeps it.
    fn return_to(&self) -> String {
        let mut out = format!("{AUTHORIZE}?");
        for (name, value) in PARAMS.iter().zip(&self.values) {
            if let Some(value) = value {
                if !out.ends_with('?') {
                    out.push('&');
                }
                out.push_str(name);
                out.push('=');
                out.push_str(&encode(value));
            }
        }
        out
    }
}

/// A request that passed every check: what its code is bound to.
struct Checked<'a> {
    code_challenge: &'a str,
    scope: String,
    nonce: Option<&'a str>,
    prompt_none: bool,
}

/// The error a valid client's request is redirected back with, if any.
fn check<'a>(req: &'a AuthRequest, client: &ClientRow) -> Result<Checked<'a>, &'static str> {
    if req.any_repeated() || req.single("state").is_some_and(|s| s.len() > MAX_STATE_BYTES) {
        return Err(INVALID_REQUEST);
    }
    if !client.grant_types.iter().any(|g| g == AUTHORIZATION_CODE) {
        return Err(UNAUTHORIZED_CLIENT);
    }
    match req.single("response_type") {
        None => return Err(INVALID_REQUEST),
        Some("code") => {}
        Some(_) => return Err(UNSUPPORTED_RESPONSE_TYPE),
    }
    if req.single("code_challenge_method") != Some("S256") {
        return Err(INVALID_REQUEST);
    }
    let Some(code_challenge) = req.single("code_challenge").filter(|c| {
        c.len() == CODE_CHALLENGE_CHARS && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    }) else {
        return Err(INVALID_REQUEST);
    };
    let nonce = req.single("nonce");
    if nonce.is_some_and(|n| n.len() > MAX_NONCE_BYTES) {
        return Err(INVALID_REQUEST);
    }
    let requested: Vec<String> =
        req.single("scope").unwrap_or_default().split(' ').filter(|s| !s.is_empty()).map(str::to_string).collect();
    if requested.iter().any(|s| !client.scopes.contains(s)) {
        return Err(INVALID_SCOPE);
    }
    let prompt_none = req.single("prompt").is_some_and(|p| p.split(' ').any(|v| v == "none"));
    Ok(Checked { code_challenge, scope: canonical_scopes(&requested).join(" "), nonce, prompt_none })
}

/// Percent-encodes every byte but the RFC 3986 unreserved ones.
pub(super) fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

#[derive(Serialize)]
struct ErrorView<'a> {
    message: &'a str,
}

fn error_page(web: &Web, status: StatusCode, message: &str) -> Response {
    web.render("error.html", status, ErrorView { message })
}

/// A 303 to the client's `redirect_uri` with `params` added to its query; a `None` value is
/// left out.
fn redirect_back(web: &Web, redirect_uri: &str, params: &[(&str, Option<&str>)]) -> Response {
    let mut location = redirect_uri.to_string();
    let mut sep = match redirect_uri.find('?') {
        None => "?",
        Some(_) if redirect_uri.ends_with(['?', '&']) => "",
        Some(_) => "&",
    };
    for (name, value) in params {
        if let Some(value) = value {
            location.push_str(sep);
            location.push_str(name);
            location.push('=');
            location.push_str(&encode(value));
            sep = "&";
        }
    }
    match HeaderValue::from_str(&location) {
        Ok(location) => (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response(),
        Err(_) => error_page(web, StatusCode::BAD_REQUEST, BAD_CLIENT_ERROR),
    }
}

/// `GET /accounts/oidc/authorize`. An unknown client or a `redirect_uri` not byte-equal to
/// one it registered is a page, never a redirect; every later error goes back to the
/// client with `state`.
pub(crate) async fn authorize(
    State(web): State<Arc<Web>>,
    jar: CookieJar,
    RawQuery(query): RawQuery,
) -> Response {
    let req = AuthRequest::parse(query.as_deref().unwrap_or_default());
    let (Some(client_id), Some(redirect_uri)) = (req.single("client_id"), req.single("redirect_uri")) else {
        return error_page(&web, StatusCode::BAD_REQUEST, BAD_CLIENT_ERROR);
    };
    let client = match web.svc.store.client(client_id).await {
        Ok(Some(client)) if client.redirect_uris.iter().any(|u| u == redirect_uri) => client,
        Ok(_) => return error_page(&web, StatusCode::BAD_REQUEST, BAD_CLIENT_ERROR),
        Err(e) => {
            tracing::error!(err = %e, "authorize: client lookup failed");
            return error_page(&web, StatusCode::SERVICE_UNAVAILABLE, BUSY_ERROR);
        }
    };
    let state = req.single("state").filter(|s| s.len() <= MAX_STATE_BYTES);
    let checked = match check(&req, &client) {
        Ok(checked) => checked,
        Err(error) => return redirect_back(&web, redirect_uri, &[("error", Some(error)), ("state", state)]),
    };
    let Some(session) = held_session_digest(&jar) else {
        return signed_out(&web, &req, &checked, redirect_uri, state);
    };
    if let Err(e) = web.svc.store.prune_oidc_codes(&web.svc.store.pool).await {
        tracing::warn!(err = %e, "authorize: code prune failed");
    }
    let code = crate::store::new_token();
    let new = NewCode {
        client_id,
        redirect_uri,
        code_challenge: checked.code_challenge,
        scope: &checked.scope,
        nonce: checked.nonce,
    };
    match web.svc.store.insert_oidc_code(&code, &session, &new).await {
        Ok(true) => redirect_back(&web, redirect_uri, &[("code", Some(&code)), ("state", state)]),
        Ok(false) => signed_out(&web, &req, &checked, redirect_uri, state),
        Err(e) => {
            tracing::error!(err = %e, "authorize: code insert failed");
            error_page(&web, StatusCode::SERVICE_UNAVAILABLE, BUSY_ERROR)
        }
    }
}

/// No live session: `login_required` for `prompt=none`, else the hosted login returning to
/// this request.
fn signed_out(web: &Web, req: &AuthRequest, checked: &Checked<'_>, redirect_uri: &str, state: Option<&str>) -> Response {
    if checked.prompt_none {
        return redirect_back(web, redirect_uri, &[("error", Some(LOGIN_REQUIRED)), ("state", state)]);
    }
    let return_to = req.return_to();
    if return_to.len() > MAX_RETURN_TO_BYTES {
        return redirect_back(web, redirect_uri, &[("error", Some(INVALID_REQUEST)), ("state", state)]);
    }
    see_other(&format!("{LOGIN}?return_to={}", encode(&return_to)))
}
