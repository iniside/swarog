//! Pins the managed/standalone boot decision (`addrs.rs`).
//!
//! # Why these live inside the bin target
//!
//! `cmd/gateway-svc/tests/` is an INTEGRATION target: it reaches the
//! `gateway_svc` LIB and nothing else — never `main.rs` or the modules it
//! declares. So the proof for a `main.rs`-side decision goes in a `#[cfg(test)]
//! mod` within the bin crate, the way `tlsenv_tests` and `admission_budget_tests`
//! already do.
//!
//! # What the fakes are, and what they are not
//!
//! Both effects are injected as closures, so every branch below runs with NO I/O:
//! the env lookup is a table, and the agent is a [`FakeAgent`] answering from a
//! canned one. Deliberate — what THIS step decides is the per-class policy over
//! `remote`'s TYPED answers, and standing up a fake HTTP server here would
//! re-test `core/remote`'s client (which has its own fake agent) while proving
//! nothing more about the decision. The two halves meet for real only in the live
//! `weles-managed-gateway` stage (the plan's Step 6); neither this file nor
//! `core/remote`'s tests are interop proof.

use std::cell::RefCell;

use remote::{AddrKind, ErrorCode, ResolveError};

use super::{addr_source_from_value, gateway_addrs, AddrSource, ResolvedAddrs};

/// What the fake hands back for one question.
type Answer = Result<Vec<String>, ResolveError>;

/// The eight addresses the split fleet actually runs at — `weles::manifest`'s
/// ports, which is where BOTH modes' answers come from in a managed rollout (the
/// composed env and the agent's `resolve` map are derived from the one port
/// authority). Written once and fed to BOTH fakes, which is what makes the
/// equivalence assertion mean "same fleet, two ways of learning it".
const FLEET: &[(&str, &str)] = &[
    ("CHARACTERS_EDGE_ADDR", "127.0.0.1:9000"),
    ("INVENTORY_EDGE_ADDR", "127.0.0.1:9001"),
    ("ACCOUNTS_EDGE_ADDR", "127.0.0.1:9003"),
    ("APIKEYS_EDGE_ADDR", "127.0.0.1:9009"),
    ("MATCH_EDGE_ADDR", "127.0.0.1:9006"),
    ("LEADERBOARD_EDGE_ADDR", "127.0.0.1:9008"),
    ("ADMIN_HTTP_ADDR", "127.0.0.1:8085"),
    ("ACCOUNTS_HTTP_ADDR", "127.0.0.1:8084"),
];

fn addr(env_key: &str) -> String {
    FLEET
        .iter()
        .find(|(key, _)| *key == env_key)
        .map(|(_, addr)| addr.to_string())
        .expect("FLEET covers every key")
}

/// An env fake: `key -> value`, with a missing key reading as unset.
fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&'static str) -> Option<String> {
    let owned: Vec<(String, String)> =
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    move |key| owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

/// An env fake where nothing is set — the defaults branch.
fn env_unset() -> impl Fn(&'static str) -> Option<String> {
    |_| None
}

/// A resolver that must never be called. The standalone claim is "nothing is
/// asked of any agent", and the only way to prove a call did NOT happen is a
/// decoy that fails the test if it did.
fn decoy_agent() -> impl Fn(&'static str, AddrKind) -> std::future::Ready<Answer> {
    |provider, kind| {
        panic!("standalone boot asked the agent for ({provider:?}, {kind:?}) — it must ask nobody")
    }
}

/// The mirror of [`decoy_agent`]: managed mode must read no address env either.
/// The modes are disjoint in BOTH directions, so a managed boot silently topped
/// up from env would be the same defect wearing the other hat.
fn decoy_env() -> impl Fn(&'static str) -> Option<String> {
    |key| panic!("managed boot read address env {key} — the two modes are disjoint")
}

/// A canned agent. Records every question asked, so a test can assert not only
/// the answers but that the RIGHT `(provider, kind)` pairs were asked —
/// `accounts` is asked twice as two different classes, and a table that asked
/// `Http` for an edge peer would still yield perfectly plausible addresses.
///
/// A scanned `Vec`, not a `HashMap`: `remote::AddrKind` is deliberately not
/// `Hash`, and a fixture's convenience is no reason to widen a shipping type's
/// derives. Eight entries do not need a data structure with an opinion.
struct FakeAgent {
    answers: Vec<((&'static str, AddrKind), Answer)>,
    asked: RefCell<Vec<(&'static str, AddrKind)>>,
}

impl FakeAgent {
    /// Answers every `(provider, kind)` the real split fleet has, from [`FLEET`].
    fn healthy() -> Self {
        let answers = vec![
            (("characters", AddrKind::Edge), Ok(vec![addr("CHARACTERS_EDGE_ADDR")])),
            (("inventory", AddrKind::Edge), Ok(vec![addr("INVENTORY_EDGE_ADDR")])),
            (("accounts", AddrKind::Edge), Ok(vec![addr("ACCOUNTS_EDGE_ADDR")])),
            (("apikeys", AddrKind::Edge), Ok(vec![addr("APIKEYS_EDGE_ADDR")])),
            (("match", AddrKind::Edge), Ok(vec![addr("MATCH_EDGE_ADDR")])),
            (("leaderboard", AddrKind::Edge), Ok(vec![addr("LEADERBOARD_EDGE_ADDR")])),
            (("admin", AddrKind::Http), Ok(vec![addr("ADMIN_HTTP_ADDR")])),
            (("accounts", AddrKind::Http), Ok(vec![addr("ACCOUNTS_HTTP_ADDR")])),
        ];
        Self { answers, asked: RefCell::new(Vec::new()) }
    }

    /// Replaces ONE answer — the failure under test — leaving the other seven
    /// healthy, so a fatal outcome is attributable to this answer and not to a
    /// fixture that answers nothing.
    fn with(mut self, provider: &'static str, kind: AddrKind, answer: Answer) -> Self {
        let slot = self
            .answers
            .iter_mut()
            .find(|((name, entry_kind), _)| *name == provider && *entry_kind == kind)
            .unwrap_or_else(|| panic!("no ({provider:?}, {kind:?}) answer to replace"));
        slot.1 = answer;
        self
    }

    /// Answers as `resolve_peer` would. An unstubbed question PANICS rather than
    /// refusing: a fixture that quietly answered `unknown_peer` would make every
    /// "fatal" assertion below pass for the wrong reason.
    fn ask(&self, provider: &'static str, kind: AddrKind) -> std::future::Ready<Answer> {
        self.asked.borrow_mut().push((provider, kind));
        let found = self
            .answers
            .iter()
            .find(|((name, entry_kind), _)| *name == provider && *entry_kind == kind);
        let answer = match found {
            Some((_, Ok(addrs))) => Ok(addrs.clone()),
            Some((_, Err(error))) => Err(clone_error(error)),
            None => panic!("fixture has no answer for ({provider:?}, {kind:?})"),
        };
        std::future::ready(answer)
    }
}

/// `ResolveError` is a real error type, not a fixture type, so it is not `Clone`.
fn clone_error(error: &ResolveError) -> ResolveError {
    match error {
        ResolveError::Unreachable(message) => ResolveError::Unreachable(message.clone()),
        ResolveError::Malformed(message) => ResolveError::Malformed(message.clone()),
        ResolveError::Refused { status, code, message } => {
            ResolveError::Refused { status: *status, code: *code, message: message.clone() }
        }
    }
}

fn refused(code: ErrorCode) -> Answer {
    Err(ResolveError::Refused { status: 404, code, message: "prose nothing may parse".into() })
}

fn managed() -> AddrSource {
    AddrSource::Agent("http://127.0.0.1:8300".to_string())
}

/// Resolves in managed mode against `agent` — and, by way of [`decoy_env`],
/// against no env at all.
async fn resolve_managed(agent: &FakeAgent) -> anyhow::Result<ResolvedAddrs> {
    gateway_addrs(&managed(), decoy_env(), |provider, kind| agent.ask(provider, kind)).await
}

/// Resolves in standalone mode from `pairs` — and, by way of [`decoy_agent`],
/// from no agent at all.
async fn resolve_env(pairs: &[(&str, &str)]) -> ResolvedAddrs {
    gateway_addrs(&AddrSource::Env, env_of(pairs), decoy_agent()).await.unwrap()
}

/// The passthrough pairs the gateway module is handed, in registration order.
fn passthroughs(addrs: &ResolvedAddrs) -> Vec<(String, String)> {
    addrs.to_wiring().passthrough().to_vec()
}

// ---------------------------------------------------------------------------
// The mode switch
// ---------------------------------------------------------------------------

#[test]
fn unset_is_standalone_and_a_url_is_managed() {
    assert_eq!(addr_source_from_value(None).unwrap(), AddrSource::Env);
    assert_eq!(
        addr_source_from_value(Some("http://127.0.0.1:8300")).unwrap(),
        AddrSource::Agent("http://127.0.0.1:8300".to_string()),
    );
    // Trimmed, so a stray newline out of a spawn env is the same URL.
    assert_eq!(
        addr_source_from_value(Some(" http://127.0.0.1:8300\n")).unwrap(),
        AddrSource::Agent("http://127.0.0.1:8300".to_string()),
    );
    // Standalone carries no URL: it is structurally unable to ask anyone.
    assert_eq!(AddrSource::Env.agent_url(), None);
}

/// Set-but-blank is NOT standalone. Falling back here would be the silent
/// fallback the design refuses: it would boot the front door against
/// `127.0.0.1:9000` defaults in the one deployment where the agent — not this
/// process — decides where peers live.
#[test]
fn a_blank_orchestrator_url_fails_startup_instead_of_falling_back() {
    for raw in ["", "   "] {
        let error = addr_source_from_value(Some(raw)).unwrap_err().to_string();
        assert!(error.contains("ORCHESTRATOR_URL"), "{error}");
        assert!(error.contains("blank"), "{error}");
        assert!(error.contains("disjoint"), "{error}");
    }
}

// ---------------------------------------------------------------------------
// Standalone: byte-identical to before, and silent
// ---------------------------------------------------------------------------

/// Today's values, verbatim: the six edge defaults and the two BLANK passthrough
/// origins (`env_addr("ADMIN_HTTP_ADDR", "")` — a blank drops the prefix, leaving
/// that route a 404).
#[tokio::test]
async fn standalone_unset_env_is_todays_defaults() {
    let addrs = gateway_addrs(&AddrSource::Env, env_unset(), decoy_agent()).await.unwrap();
    let wiring = addrs.to_wiring();

    assert_eq!(wiring.peer_or("characters", "unset"), "127.0.0.1:9000");
    assert_eq!(wiring.peer_or("inventory", "unset"), "127.0.0.1:9001");
    assert_eq!(wiring.peer_or("accounts", "unset"), "127.0.0.1:9003");
    assert_eq!(wiring.peer_or("apikeys", "unset"), "127.0.0.1:9009");
    assert_eq!(wiring.peer_or("match", "unset"), "127.0.0.1:9006");
    assert_eq!(wiring.peer_or("leaderboard", "unset"), "127.0.0.1:9008");
    assert_eq!(
        passthroughs(&addrs),
        vec![
            ("/admin".to_string(), String::new()),
            ("/accounts/epic".to_string(), String::new()),
        ],
        "an unset origin stays BLANK — the proxy table drops the route and it 404s",
    );
}

/// A set var wins over the default, and a blank one reads as unset — the
/// `env_addr` rule this refactor carried over unchanged.
#[tokio::test]
async fn standalone_reads_the_env_var_and_treats_blank_as_unset() {
    let addrs = resolve_env(&[
        ("CHARACTERS_EDGE_ADDR", "10.0.0.5:9000"),
        ("INVENTORY_EDGE_ADDR", "   "),
        ("ADMIN_HTTP_ADDR", "127.0.0.1:8085"),
    ])
    .await;

    let wiring = addrs.to_wiring();
    assert_eq!(wiring.peer_or("characters", "unset"), "10.0.0.5:9000");
    assert_eq!(wiring.peer_or("inventory", "unset"), "127.0.0.1:9001", "blank means unset");
    assert_eq!(passthroughs(&addrs)[0], ("/admin".to_string(), "127.0.0.1:8085".to_string()));
}

/// THE standalone claim: with `ORCHESTRATOR_URL` unset, nothing is asked of any
/// agent — no HTTP, no dial, no I/O. Proven by a decoy resolver that panics if it
/// is ever called, not by the absence of an error.
#[tokio::test]
async fn env_mode_asks_no_agent() {
    let addrs = resolve_env(FLEET).await;
    assert_eq!(addrs.to_wiring().peer_or("characters", "unset"), "127.0.0.1:9000");
}

// ---------------------------------------------------------------------------
// Managed: the same eight pairs, learned from the agent
// ---------------------------------------------------------------------------

/// THE equivalence: for one fleet, "told by env" and "asked the agent" produce
/// the SAME eight pairs. That is the whole M1 claim at this seam — the plaster
/// changes where the answer comes from, and nothing else.
#[tokio::test]
async fn managed_resolves_the_same_eight_pairs_as_env() {
    let agent = FakeAgent::healthy();
    let from_agent = resolve_managed(&agent).await.unwrap();
    let from_env = resolve_env(FLEET).await;

    assert_eq!(from_agent, from_env, "managed and standalone must agree for the same fleet");

    // ...and the questions were the right ones: eight, one per address, with
    // `accounts` asked twice as its TWO classes (edge 9003 + http 8084). A table
    // that asked Http for an edge peer would still have produced eight pairs.
    assert_eq!(
        *agent.asked.borrow(),
        vec![
            ("characters", AddrKind::Edge),
            ("inventory", AddrKind::Edge),
            ("accounts", AddrKind::Edge),
            ("apikeys", AddrKind::Edge),
            ("match", AddrKind::Edge),
            ("leaderboard", AddrKind::Edge),
            ("admin", AddrKind::Http),
            ("accounts", AddrKind::Http),
        ],
    );
}

// ---------------------------------------------------------------------------
// The per-class failure policy
// ---------------------------------------------------------------------------

/// `unknown_peer` on an EDGE peer ⇒ the process dies. A silent fallback to
/// `127.0.0.1:9000` would be worse than death: it is an address nobody is on.
#[tokio::test]
async fn unknown_peer_on_an_edge_peer_is_fatal() {
    let agent =
        FakeAgent::healthy().with("characters", AddrKind::Edge, refused(ErrorCode::UnknownPeer));
    let error = resolve_managed(&agent)
        .await
        .expect_err("an edge peer with no address must not boot")
        .to_string();

    assert!(error.contains("characters"), "{error}");
    assert!(error.contains("unknown_peer"), "{error}");
    assert!(
        error.contains("127.0.0.1:9000"),
        "the default must be named as the thing NOT used: {error}"
    );
}

/// `unknown_peer` on a PASSTHROUGH ⇒ an empty origin, not death: the fleet fact
/// "this topology has no admin" is exactly what an unset `ADMIN_HTTP_ADDR`
/// states, and fail-closed must not delete that behaviour.
///
/// "The prefix drops exactly as a blank env does today" is proven as EQUALITY
/// with the blank-env wiring: the module is handed byte-identical
/// `(prefix, origin)` pairs, and `ProxyTable::from_routes` filters blank origins
/// out — so the route is unrouted and 404s in both.
#[tokio::test]
async fn unknown_peer_on_a_passthrough_is_an_empty_origin() {
    let agent = FakeAgent::healthy().with("admin", AddrKind::Http, refused(ErrorCode::UnknownPeer));
    let from_agent = resolve_managed(&agent).await.expect("a missing origin is not fatal");

    assert_eq!(
        passthroughs(&from_agent)[0],
        ("/admin".to_string(), String::new()),
        "no origin ⇒ blank ⇒ the proxy table drops /admin and it 404s",
    );

    // Byte-identical to today's blank-default boot, pair for pair: the env fleet
    // WITHOUT an ADMIN_HTTP_ADDR is the same wiring the agent's refusal produces.
    let env_without_admin: Vec<(&str, &str)> =
        FLEET.iter().copied().filter(|(key, _)| *key != "ADMIN_HTTP_ADDR").collect();
    assert_eq!(from_agent, resolve_env(&env_without_admin).await);

    // The OTHER seven are untouched: a fatal-vs-blank decision is per address,
    // never a whole-boot switch.
    assert_eq!(from_agent.to_wiring().peer_or("characters", "unset"), "127.0.0.1:9000");
    assert_eq!(
        passthroughs(&from_agent)[1],
        ("/accounts/epic".to_string(), "127.0.0.1:8084".to_string()),
    );
}

/// `unknown_route` is a fact about the AGENT — it does not speak this contract —
/// so it is fatal for EVERY class, passthroughs included. This is the branch the
/// two-404s taxonomy exists for: read as `unknown_peer` it would mean "admin has
/// no origin" and boot a silently broken front door out of an agent that never
/// answered the question.
#[tokio::test]
async fn unknown_route_is_fatal_even_for_a_passthrough() {
    for (provider, kind) in [("admin", AddrKind::Http), ("accounts", AddrKind::Http)] {
        let agent = FakeAgent::healthy().with(provider, kind, refused(ErrorCode::UnknownRoute));
        let error = resolve_managed(&agent)
            .await
            .expect_err("an agent that does not speak the contract is fatal for a passthrough too")
            .to_string();
        assert!(error.contains("UnknownRoute"), "{error}");
        assert!(error.contains("does not fall back to env"), "{error}");
    }
}

/// The remaining classes are fatal for both classes too: a client bug
/// (`bad_request`), the agent's own failure (`internal`), no answer at all
/// (`Unreachable`), and an answer that is not this contract (`Malformed`). Swept
/// together because the defect would be one defect: a class quietly read as "no
/// address".
#[tokio::test]
async fn every_other_failure_is_fatal_for_both_classes() {
    let cases = || -> Vec<Answer> {
        vec![
            refused(ErrorCode::BadRequest),
            refused(ErrorCode::Internal),
            Err(ResolveError::Unreachable("connection refused".into())),
            Err(ResolveError::Malformed("200 body is not {\"addrs\":[…]}".into())),
        ]
    };
    for (provider, kind) in [("characters", AddrKind::Edge), ("admin", AddrKind::Http)] {
        for answer in cases() {
            let agent = FakeAgent::healthy().with(provider, kind, answer);
            resolve_managed(&agent)
                .await
                .expect_err(&format!("{provider}/{kind:?} must not boot on this answer"));
        }
    }
}

// ---------------------------------------------------------------------------
// The list shape: `[]` is not `unknown_peer`, and neither is defaulted away
// ---------------------------------------------------------------------------

/// `200 {"addrs":[]}` is a LIVENESS answer, not the topology fact `unknown_peer`
/// — so it is neither an address nor a blank origin. The tempting
/// `.first().cloned().unwrap_or_default()` would turn it into `""` and, for the
/// passthrough, silently reproduce "this route 404s forever" out of an answer
/// that says the opposite. Fatal for BOTH classes, named as M2's territory.
#[tokio::test]
async fn an_empty_address_list_is_not_an_address() {
    for (provider, kind) in [("characters", AddrKind::Edge), ("admin", AddrKind::Http)] {
        let agent = FakeAgent::healthy().with(provider, kind, Ok(vec![]));
        let error = resolve_managed(&agent)
            .await
            .expect_err("an empty list must not be defaulted into an empty address")
            .to_string();
        assert!(error.contains("EMPTY"), "{error}");
        assert!(error.contains("LIVENESS"), "{error}");
        // The distinction IS the deliverable: an empty list must never be
        // reported as the topology refusal, which is what collapsing the two
        // would do.
        assert!(!error.contains("unknown_peer"), "{error}");
    }
}

/// A blank string inside a one-element list is not an address either — the same
/// defaulting hole, one layer in.
#[tokio::test]
async fn a_blank_address_is_not_an_address() {
    let agent = FakeAgent::healthy().with("admin", AddrKind::Http, Ok(vec!["  ".to_string()]));
    let error = resolve_managed(&agent).await.unwrap_err().to_string();
    assert!(error.contains("BLANK"), "{error}");
}

/// Two instances is M2's LB shape, which this front door does not implement:
/// taking the first would look healthy while half the traffic went nowhere.
#[tokio::test]
async fn two_addresses_are_refused_rather_than_silently_halved() {
    let agent = FakeAgent::healthy().with(
        "characters",
        AddrKind::Edge,
        Ok(vec!["127.0.0.1:9000".to_string(), "127.0.0.1:9100".to_string()]),
    );
    let error = resolve_managed(&agent).await.unwrap_err().to_string();
    assert!(error.contains("2 addresses"), "{error}");
    assert!(error.contains("load balancing"), "{error}");
}
