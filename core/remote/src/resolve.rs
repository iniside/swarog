//! The client half of the orchestrator agent's `resolve` verb: *where does
//! provider X listen, in the topology that is actually running?*
//!
//! # This file cannot prove interop, and must not be mistaken for proof
//!
//! The server is `weles::agentapi`. `remote` is in the SHIPPING graph
//! (`core/`, `api/`, `modules/`, `cmd/`), and the shipping graph may never
//! import `weles` — not even as a dev-dependency (`docs/reference/
//! weles-design.md`, Non-negotiables; verification tooling like `verifyctl` is
//! the *narrow* exception, and it does not extend here). So this client is
//! tested against a fake agent this crate stands up itself, and weles's server
//! is tested against a fake client of its own. **Neither side's tests can
//! observe the other**, so nothing in THIS crate can catch a drift between the
//! two spellings.
//!
//! **A stage that sits above both does.** `verifyctl` MAY import weles (the
//! narrow verification-tooling exception), and its BLOCKING `weles-wire-contract`
//! stage (`tools/verifyctl/src/stages/weles_wire_contract.rs`) imports this
//! crate AND weles, then drives the real derives on both sides: every
//! [`AddrKind`]/[`ErrorCode`] variant's wire bytes are compared, weles's
//! [`ErrorCode`] bytes are deserialized INTO this crate's enum (the property
//! that actually matters — that this client can read what that server writes),
//! and each body is round-tripped through the far side's parser so the FIELD
//! names (`provider`/`kind`/`addrs`/`code`/`error`) and [`RESOLVE_PATH`] are
//! pinned too. It is in-memory, so it runs under `--fast`. The `drift_probe_*`
//! functions at the bottom of this file are that stage's seam into this crate's
//! private wire types.
//!
//! That gate is the CHEAP, EARLY half — it runs under `--fast` and catches a
//! drift before anything boots — but it is no longer the whole proof. The
//! blocking `weles-managed-gateway` stage
//! (`tools/verifyctl/src/stages/weles_managed_gateway.rs`) boots weles on the
//! real split fleet and drives this client over a real socket. What THAT adds,
//! precisely — the claim is worth being exact about, because an overstatement
//! here is the same disease as the understatement it replaced:
//!
//! * the HTTP **method**. A `GET` here against the server's `(&Method::POST,
//!   RESOLVE_PATH)` arm is a `404 unknown_route` no in-memory comparison of
//!   types can see.
//! * that `cmd/gateway-svc`'s `main` is wired to THIS function at all.
//!   `addrs::gateway_addrs` is proven with an injected resolver, so its own tests
//!   cannot see whether the real main passes the real one.
//!
//! **NOT the status↔code pairing — nothing pins that, and nothing needs to.**
//! [`resolve_peer_within`] branches on the `code` field ALONE (`status` is
//! carried for logs), and so does `cmd/gateway-svc`'s `managed_addr`. If weles
//! moved `unknown_peer` from 404 to 400 tomorrow, every caller would behave
//! identically: there is no pairing to break. The live stage could not reach a
//! refusal arm anyway — a healthy fleet resolves all eight, and its dead-port
//! decoy exercises [`ResolveError::Unreachable`], not a refusal.
//!
//! # The wire (as the server implements it)
//!
//! ```text
//! POST <orchestrator_url>/resolve  {"provider":"characters","kind":"edge"}
//!   200 {"addrs":["127.0.0.1:9000"]}
//!   non-2xx {"code":"<ErrorCode>","error":"<prose>"}
//! ```
//!
//! The `code` is the ONLY thing a caller may branch on; `error` is operator
//! prose and nothing may parse it. That is why [`ResolveError::Refused`] hands
//! back a typed [`ErrorCode`] instead of a message.
//!
//! # The two 404s are different facts (this is the whole point)
//!
//! * [`ErrorCode::UnknownRoute`] is a fact about **the agent**: it does not
//!   serve this route, i.e. it does not speak the contract this client expects
//!   (an older agent, a wrong URL). It is NEVER a statement about any service,
//!   and it is fatal for every caller.
//! * [`ErrorCode::UnknownPeer`] is a fact about **the fleet**: this
//!   `(provider, kind)` is not a thing in this topology. Closed-world and
//!   manifest-derived, so it is knowably not coming — but what it *means* is
//!   the caller's decision, not this function's (`cmd/gateway-svc` treats it as
//!   fatal for an edge peer and as "no passthrough origin" for an HTTP one).
//!
//! Collapsing these two into one error would let a gateway read "this agent
//! predates the verb" as "admin has no HTTP origin" and boot with a silently
//! empty passthrough instead of dying.
//!
//! # `404 unknown_peer` is NOT `200 {"addrs":[]}`
//!
//! Also pinned by the design (`weles-design.md`, "M1 scope"), and preserved
//! here rather than flattened for the caller's convenience:
//!
//! * `404 unknown_peer` ⇒ `Err(Refused { code: UnknownPeer, .. })` — a
//!   topology fact a caller MAY treat as fatal-and-final.
//! * `200 {"addrs":[]}` ⇒ `Ok(vec![])` — *it is a thing; nothing is live right
//!   now*. A liveness fact (M2 territory; M1's server never emits it) that a
//!   caller may NOT treat as final.
//!
//! An `Ok(vec![])` folded into the 404 would erase that distinction at the one
//! place where the milestone's whole deliverable is the shape.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Upper bound on one [`resolve_peer`] call, dial included.
///
/// A core-leaf constant, NEVER env (Hard Constraint 1/5): if a deployment ever
/// needs another value, thread it through the call the way `orchestrator_url`
/// already is. The value matches `edge::client::DIAL_DEADLINE` (5s) on purpose:
/// this is the same class of question — a loopback peer either answers about
/// now, or is not there. There is NO retry behind it. A retry policy here would
/// be a second authority beside `opsapi::RetryMode`, so a caller that wants one
/// asks again itself; the shipped callers fail closed instead.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// Cap on the agent's answer, enforced as it arrives rather than after.
///
/// Same value and the same argument as the server's own `MAX_BODY_BYTES`
/// (`weles::agentapi`): *a body cannot be trusted to be small just because
/// every honest peer's is*. That argument is symmetric, and the code must be
/// too — this half is SHIPPING code (`core/remote` links into every
/// `cmd/*-svc`), so weles's trusted-local-operator threat model does not cover
/// it. An unbounded read is capped only by [`RESOLVE_TIMEOUT`], which is five
/// seconds of whatever a confused (or hostile) URL cares to send. Over the cap
/// is [`ResolveError::Malformed`] — the answer is not this contract — never a
/// truncated parse.
const MAX_ANSWER_BYTES: usize = 8 * 1024;

/// The path this client POSTs the question to — the third hand-copied fact of
/// this contract, after [`AddrKind`] and [`ErrorCode`]. The server's copy is
/// `weles::agentapi::RESOLVE_PATH`, and the `weles-wire-contract` verify stage
/// compares the two (see the module doc).
///
/// A drift here is not silent — weles would answer `404 unknown_route`, which
/// [`ErrorCode::UnknownRoute`] exists to make fatal-and-loud at gateway boot
/// rather than mistakable for a fact about a service. It is pinned anyway
/// because it costs one const to turn a boot-time outage into a `--fast` FAIL.
///
/// `pub` so the stage can read it: this is the value [`resolve_peer_within`]
/// actually builds its URL from, never a copy declared beside it.
#[doc(hidden)]
pub const RESOLVE_PATH: &str = "/resolve";

/// Which of a provider's addresses is being asked for.
///
/// A provider can have both: `accounts` is an edge peer (9003) AND an HTTP
/// passthrough origin (8084), while `admin` is only ever an origin. So `kind`
/// is a parameter of the question, not something derivable from `provider`.
///
/// This is deliberately a SECOND spelling of `weles::manifest::AddrKind`, which
/// weles keeps as the single serde authority on its side. Zero-sharing forbids
/// sharing the type across the two, so no test in EITHER crate can catch a drift
/// between the spellings — the `weles-wire-contract` verify stage above both is
/// what catches it (see the module doc), and it is why `lowercase` here is no
/// longer unfalsifiable. `Edge`/`Http` still render identically under
/// `lowercase` and `snake_case`, so the spelling only becomes observable at the
/// first multi-word variant (`ServiceDef` already carries a `player_port`),
/// where a copied `snake_case` would silently diverge `"playeredge"` from
/// `"player_edge"`. That stage compares this enum's bytes against weles's
/// variant-by-variant, so that day is a FAIL and not an outage.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AddrKind {
    /// The provider's internal mTLS QUIC edge.
    Edge,
    /// The provider's HTTP surface (a passthrough origin).
    Http,
}

/// Why the agent refused — the closed contract enum, and the only part of a
/// refusal a caller may branch on.
///
/// Closed on purpose (no `#[non_exhaustive]`, no catch-all variant): the wire
/// contract is a closed set, and a code this client does not know means the
/// agent is not speaking this contract. That surfaces as
/// [`ResolveError::Malformed`], not as a silently-tolerated unknown.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The agent serves no such route: it does not speak this contract. A fact
    /// about the AGENT, never about a service — fatal for every caller.
    UnknownRoute,
    /// The question was well-formed; this topology has no such
    /// `(provider, kind)`. A fact about the FLEET — the caller decides what it
    /// means.
    UnknownPeer,
    /// The question itself was malformed (this client's bug, or a contract
    /// drift the agent rejected).
    BadRequest,
    /// The agent failed on its own account.
    Internal,
}

/// Everything [`resolve_peer`] can answer other than addresses.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// The agent never answered: no connection, or no reply inside
    /// [`RESOLVE_TIMEOUT`]. Also covers this process failing to build its own
    /// HTTP client — from the caller's side the distinction is nil, because in
    /// every case the answer did not happen and no retry will be attempted.
    #[error("orchestrator did not answer: {0}")]
    Unreachable(String),
    /// The agent answered a refusal, with a machine-readable [`ErrorCode`].
    /// Branch on `code`; `message` is operator prose — never parse it.
    #[error("orchestrator refused ({status}, {code:?}): {message}")]
    Refused {
        /// The HTTP status, for logs. `code` — not this — is the discriminator:
        /// both 404s share this number.
        status: u16,
        /// The contract code. THIS is what a caller branches on.
        code: ErrorCode,
        /// Operator prose. Nothing may branch on it.
        message: String,
    },
    /// The agent answered something this client cannot read as the contract: a
    /// 2xx that is not `{"addrs":[…]}`, a refusal that is not
    /// `{"code":…,"error":…}`, or a `code` outside [`ErrorCode`]. It is not
    /// this contract, so it is fatal — never guessed at.
    #[error("orchestrator answered something that is not this contract: {0}")]
    Malformed(String),
}

/// The question, spelled exactly as `weles::agentapi::ResolveRequest` reads it
/// (`deny_unknown_fields` over there, so an extra field here would be a 400).
#[derive(Debug, Serialize)]
struct ResolveRequest<'a> {
    /// The provider's SHORT name (`"characters"`) — the same spelling
    /// `Stub::new("characters", …)` uses. Never a `-svc` package name.
    provider: &'a str,
    kind: AddrKind,
}

/// The answer. Tolerant of unknown fields ON PURPOSE — the inverse of the
/// server's strictness, and not an oversight: contracts here evolve additively,
/// and a client that rejected a field a later agent added would turn an
/// additive change into an outage. `addrs` itself is required.
#[derive(Debug, Deserialize)]
struct ResolveResponse {
    addrs: Vec<String>,
}

/// A refusal envelope.
#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    code: ErrorCode,
    #[serde(default)]
    error: String,
}

/// Asks the orchestrator agent at `orchestrator_url` (e.g.
/// `http://127.0.0.1:8099`) for every address of `kind` that `provider` has, in
/// the topology that is actually running.
///
/// Bounded by [`RESOLVE_TIMEOUT`] and never retried. `Ok` may be EMPTY — see
/// the module doc: an empty list is a liveness answer and is NOT the same
/// answer as [`ErrorCode::UnknownPeer`], so a caller must not treat it as
/// final.
///
/// **Nothing here proves this client and `weles::agentapi` agree** — see the
/// module doc. Zero-sharing means each side is tested against its own fake; the
/// live `weles-managed-gateway` stage is the only interop proof.
///
/// `core/*` never reads env, so `ORCHESTRATOR_URL` is read by
/// `cmd/gateway-svc`'s main and threaded in — the same way `peer_addr` reaches
/// [`crate::Stub::new`].
pub async fn resolve_peer(
    orchestrator_url: &str,
    provider: &str,
    kind: AddrKind,
) -> Result<Vec<String>, ResolveError> {
    resolve_peer_within(orchestrator_url, provider, kind, RESOLVE_TIMEOUT).await
}

/// [`resolve_peer`] with the time budget drivable, so a test can prove the
/// timeout branch (an agent that accepts and never answers) without sitting
/// through the real budget. Test-only: production always passes
/// [`RESOLVE_TIMEOUT`].
async fn resolve_peer_within(
    orchestrator_url: &str,
    provider: &str,
    kind: AddrKind,
    budget: Duration,
) -> Result<Vec<String>, ResolveError> {
    let endpoint = format!("{}{RESOLVE_PATH}", orchestrator_url.trim_end_matches('/'));
    // Per call, not cached: this runs a handful of times at boot, and a free
    // function with no hidden state is the smaller surface. `timeout` covers
    // the WHOLE call (connect through body), which is what makes "never a
    // hang" a property of this function rather than of its caller.
    let client = reqwest::Client::builder()
        .timeout(budget)
        // `reqwest::Client::builder()` defaults `auto_sys_proxy: true`, which
        // reads `ALL_PROXY`/`HTTP_PROXY`/`http_proxy`/… at `build()` and has NO
        // automatic loopback bypass (only an explicit `NO_PROXY` grants one).
        // That would make "core/* never reads env" a lie told one dependency
        // deep, and it is not hypothetical: `processctl`'s fleet env
        // deliberately forwards `http_proxy`/`all_proxy` into every spawned
        // process (and `weles::manifest::SERVICE_ENV_ALLOWLIST` copies that
        // list — the `weles-fleet-parity` stage proves them equal), so the one
        // env var that hijacks this call is plumbed into gateway-svc BY DESIGN.
        // A local dev proxy would send the resolve question to whatever it
        // pleases — including the gateway's own HTTP port — and any 200 whose
        // body happens to carry `addrs` would hand back addresses the agent
        // never authored. A wrong address is strictly worse than no address.
        // Pinned by `a_proxy_in_the_env_cannot_hijack_the_resolve_question`.
        .no_proxy()
        // The house convention every other `reqwest::Client::builder()` in this
        // repo follows (`modules/gateway/src/proxy.rs`, `tools/splitproof`,
        // `modules/accounts`), and it is load-bearing HERE specifically: a
        // followed `302` becomes a GET (reqwest drops the body), weles serves
        // no `GET /resolve`, and the answer comes back `404 unknown_route` —
        // i.e. "this agent does not speak the contract", a lie about the agent,
        // which is the exact confusion `ErrorCode` exists to prevent. A `307`
        // would be worse: it replays the body and yields `Ok(addrs)` from a
        // host that is not the agent. Unfollowed, a 3xx carries no envelope and
        // lands in `Malformed` — "not this contract", which is the truth.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| ResolveError::Unreachable(format!("build http client: {error}")))?;
    let mut response = client
        .post(&endpoint)
        .json(&ResolveRequest { provider, kind })
        .send()
        .await
        .map_err(|error| ResolveError::Unreachable(format!("POST {endpoint}: {error}")))?;
    let status = response.status().as_u16();
    let body = read_capped(&mut response, &endpoint).await?;
    if (200..300).contains(&status) {
        let answer: ResolveResponse = serde_json::from_slice(&body).map_err(|error| {
            ResolveError::Malformed(format!("{status} body is not {{\"addrs\":[…]}}: {error}"))
        })?;
        // Possibly empty, and handed back as-is. See the module doc.
        return Ok(answer.addrs);
    }
    // Every non-2xx carries the envelope, so a status we cannot read as one is
    // not this contract — NOT a refusal with a guessed code (guessing here is
    // exactly how `unknown_route` would get read as `unknown_peer`).
    let envelope: ErrorEnvelope = serde_json::from_slice(&body).map_err(|error| {
        ResolveError::Malformed(format!(
            "{status} body is not {{\"code\":…,\"error\":…}} with a known code: {error}"
        ))
    })?;
    Err(ResolveError::Refused { status, code: envelope.code, message: envelope.error })
}

/// Reads the answer chunk-by-chunk, refusing at [`MAX_ANSWER_BYTES`] BEFORE the
/// over-cap bytes are retained — the point of a cap is to not hold the thing
/// it caps, so this can never be a `bytes()` followed by a length check.
///
/// A cap breach is [`ResolveError::Malformed`], not `Unreachable`: the peer did
/// answer, and what it answered is not this contract (both bodies in it are one
/// short name and a tiny list).
async fn read_capped(
    response: &mut reqwest::Response,
    endpoint: &str,
) -> Result<Vec<u8>, ResolveError> {
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| ResolveError::Unreachable(format!("read {endpoint} answer: {error}")))?
    {
        if body.len() + chunk.len() > MAX_ANSWER_BYTES {
            return Err(ResolveError::Malformed(format!(
                "answer exceeds {MAX_ANSWER_BYTES} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

// ---------------------------------------------------------------------------
// Drift-gate seams for `verifyctl`'s `weles-wire-contract` stage.
//
// The mirror image of `weles::agentapi`'s `drift_probe_*` trio, and they exist
// for the reason the module doc opens with: nothing IN this crate can observe
// weles, so nothing in this crate can catch a drift. The stage above both can,
// and these are the smallest surface that lets it drive the REAL derives here —
// the wire structs stay private, so what the stage pins is what this client
// actually writes and reads, not a fourth copy of the field names. Never called
// by production code (`resolve_peer` builds the same types directly).
// ---------------------------------------------------------------------------

/// Renders a `resolve` question exactly as [`resolve_peer_within`] puts it on
/// the wire (`.json(&ResolveRequest { .. })` is `serde_json::to_vec`).
#[doc(hidden)]
pub fn drift_probe_encode_resolve_request(provider: &str, kind: AddrKind) -> Vec<u8> {
    serde_json::to_vec(&ResolveRequest { provider, kind })
        .expect("ResolveRequest is a &str plus a Copy enum — serializing it cannot fail")
}

/// Parses a 2xx `resolve` answer exactly as [`resolve_peer_within`] does.
#[doc(hidden)]
pub fn drift_probe_parse_resolve_response(body: &[u8]) -> Result<Vec<String>, String> {
    serde_json::from_slice::<ResolveResponse>(body)
        .map(|answer| answer.addrs)
        .map_err(|error| error.to_string())
}

/// Parses a refusal envelope exactly as [`resolve_peer_within`] does, handing
/// back the two fields it reads. An [`ErrorCode`] this client does not know is
/// an `Err` here for the same reason it is [`ResolveError::Malformed`] there.
#[doc(hidden)]
pub fn drift_probe_parse_error_envelope(body: &[u8]) -> Result<(ErrorCode, String), String> {
    serde_json::from_slice::<ErrorEnvelope>(body)
        .map(|envelope| (envelope.code, envelope.error))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod resolve_tests;
