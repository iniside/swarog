//! Where this front door's eight addresses come from — the ONE deterministic
//! decision this process makes at start.
//!
//! ```text
//! ORCHESTRATOR_URL unset ⇒ standalone: read the eight env vars (as always).
//! ORCHESTRATOR_URL set   ⇒ managed:    ask the local agent (`remote::resolve_peer`).
//! ```
//!
//! # The two modes are DISJOINT — there is no layering, precedence, or fallback
//!
//! Recorded in `docs/reference/weles-design.md` ("Two disjoint boot modes") and
//! implemented here as the shape of [`AddrSource`]: a managed boot carries a URL
//! and reads no address env at all; a standalone boot has no URL and can reach no
//! agent. A managed boot that failed BACK to env would not be a graceful
//! degradation — it would silently boot this front door pointed at
//! `127.0.0.1:9000`-shaped defaults, i.e. addresses nobody is on, in exactly the
//! deployment where the agent (not this file) decides where peers live. Managed
//! mode fails; it never becomes standalone.
//!
//! # The failure policy is PER CLASS, and branches on the CODE — never on prose
//!
//! The eight addresses are two classes ([`AddrClass`]), and "unresolvable" does
//! not mean the same thing to both:
//!
//! * An **edge peer** with no address ⇒ this process DIES. There is no benign
//!   value: the default is an address nobody is on, and a `Stub` pointed at it
//!   fails one dial at a time, in another process, far from here.
//! * A **passthrough origin** with no address ⇒ an EMPTY origin, which is
//!   precisely today's blank-default semantics: `ProxyTable::from_routes`
//!   (`modules/gateway/src/proxy.rs`) drops a blank-origin route, so the prefix
//!   is unrouted and the request 404s. Fail-closed must not delete a behaviour
//!   the standalone default already has.
//!
//! What makes that expressible without string-matching is [`remote::ErrorCode`]:
//!
//! * [`ErrorCode::UnknownPeer`] is a fact about the FLEET — this
//!   `(provider, kind)` is not a thing in this topology. THIS is the one the
//!   per-class policy branches on.
//! * [`ErrorCode::UnknownRoute`] is a fact about the AGENT — it does not speak
//!   this contract (an older agent, a wrong URL). It says nothing about any
//!   service, so it is fatal for EVERY class, passthroughs included. Reading it
//!   as "admin has no origin" would boot a silently broken front door out of an
//!   agent that never answered the question.
//! * `BadRequest` (this client's bug), `Internal`, `Unreachable` and `Malformed`
//!   are likewise fatal for every class.
//!
//! # `404 unknown_peer` is not `200 {"addrs":[]}`, and neither is defaulted away
//!
//! `resolve_peer` hands back `Ok(vec![])` for "it is a thing; nothing is live
//! right now" — a LIVENESS answer that M1's agent never emits and that a caller
//! may not treat as final. So [`exactly_one`] refuses it loudly instead of
//! `.first().cloned().unwrap_or_default()`-ing it into an empty address string:
//! an empty list is not an address, and folding it into the passthrough's
//! empty-origin rule would answer a liveness question with a topology decision.
//! Acting on it (waiting, re-resolving) is M2's job. Two addresses is refused for
//! the same reason from the other side: choosing between instances is load
//! balancing, which M1 does not do — and silently taking the first would send
//! half a fleet's traffic nowhere while looking healthy.

use std::future::Future;

use anyhow::{bail, Result};
use lifecycle::ProcessWiring;
use remote::{AddrKind, ErrorCode, ResolveError};

/// What `remote::resolve_peer` answers. Aliased because it appears in three
/// signatures and `anyhow::Result` shadows plain `Result` in this file.
type WireAnswer = std::result::Result<Vec<String>, ResolveError>;

/// The env var that decides the mode. Read HERE in the composition root:
/// `core/remote` never reads env (`core/remote/src/lib.rs`), so the URL is
/// threaded into `resolve_peer` the way `peer_addr` is threaded into
/// `Stub::new`.
const ORCHESTRATOR_URL_ENV: &str = "ORCHESTRATOR_URL";

/// Which of the front door's two address classes an entry is — and, with it,
/// what an unresolvable answer MEANS for that entry.
///
/// The class is a FIELD of [`AddrSpec`], never inferred from the env key's
/// `_EDGE_`/`_HTTP_` spelling: guessing from the key would make the env NAME the
/// authority for what an address is, and `accounts` is both classes at once
/// (edge 9003 + http 8084) under two different keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AddrClass {
    /// A `remote::Stub` peer, dialed over the internal mTLS edge.
    /// `ProcessWiring::with_peer`.
    Edge,
    /// An HTTP reverse-proxy origin for `prefix`. `ProcessWiring::with_passthrough`.
    Passthrough { prefix: &'static str },
}

impl AddrClass {
    /// The class as the agent's wire asks it. The two enums are not the same
    /// question — this one also carries the route prefix — so the mapping is
    /// explicit rather than a shared type.
    fn kind(self) -> AddrKind {
        match self {
            AddrClass::Edge => AddrKind::Edge,
            AddrClass::Passthrough { .. } => AddrKind::Http,
        }
    }
}

/// One of the eight addresses: its env key (standalone), the provider short name
/// the agent knows it by (managed), its class, and its standalone default.
struct AddrSpec {
    env_key: &'static str,
    /// The SHORT domain name — the one `Stub::new("characters", …)` and
    /// `weles::manifest`'s `ServiceDef::provider` already use.
    provider: &'static str,
    class: AddrClass,
    /// The value a standalone boot uses when the env var is unset or blank.
    /// Managed mode never reads it: an agent that cannot answer is not an
    /// invitation to guess.
    env_default: &'static str,
}

/// THE table — the single declaration of what this front door needs, which both
/// modes iterate. A mode that walked its own list could resolve a different set
/// than the other builds wiring from.
///
/// Order is today's `ProcessWiring` construction order, and the passthrough order
/// is load-bearing: `ProcessWiring::passthrough()` hands the module its pairs in
/// registration order.
const ADDR_SPECS: &[AddrSpec] = &[
    AddrSpec {
        env_key: "CHARACTERS_EDGE_ADDR",
        provider: "characters",
        class: AddrClass::Edge,
        env_default: "127.0.0.1:9000",
    },
    AddrSpec {
        env_key: "INVENTORY_EDGE_ADDR",
        provider: "inventory",
        class: AddrClass::Edge,
        env_default: "127.0.0.1:9001",
    },
    AddrSpec {
        env_key: "ACCOUNTS_EDGE_ADDR",
        provider: "accounts",
        class: AddrClass::Edge,
        env_default: "127.0.0.1:9003",
    },
    AddrSpec {
        env_key: "APIKEYS_EDGE_ADDR",
        provider: "apikeys",
        class: AddrClass::Edge,
        env_default: "127.0.0.1:9009",
    },
    // Step 10: match + leaderboard front-door routing. Their `remote_factories`
    // contribute only `route_bindings` (no provide), so the front routes
    // `POST /match/report` -> match-svc (:9006) and `GET /leaderboard` ->
    // leaderboard-svc (:9008) Remote over the mTLS edge.
    AddrSpec {
        env_key: "MATCH_EDGE_ADDR",
        provider: "match",
        class: AddrClass::Edge,
        env_default: "127.0.0.1:9006",
    },
    AddrSpec {
        env_key: "LEADERBOARD_EDGE_ADDR",
        provider: "leaderboard",
        class: AddrClass::Edge,
        env_default: "127.0.0.1:9008",
    },
    // The two passthrough ORIGINS: `/admin` → admin-svc, `/accounts/epic` → the
    // Epic web OAuth flow on accounts-svc. A blank default drops the prefix (the
    // proxy table skips empties), so an unset var leaves that route a 404 — the
    // semantics the managed path's `unknown_peer` arm reproduces exactly.
    AddrSpec {
        env_key: "ADMIN_HTTP_ADDR",
        provider: "admin",
        class: AddrClass::Passthrough { prefix: "/admin" },
        env_default: "",
    },
    AddrSpec {
        env_key: "ACCOUNTS_HTTP_ADDR",
        provider: "accounts",
        class: AddrClass::Passthrough {
            prefix: "/accounts/epic",
        },
        env_default: "",
    },
];

/// The decided source of the eight addresses. Two variants, no third: there is
/// no "managed with an env fallback", because the type that would express it is
/// the bug (see the module doc).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AddrSource {
    /// Standalone: the eight env vars, exactly as before weles existed.
    Env,
    /// Managed: ask the agent at this URL.
    Agent(String),
}

impl AddrSource {
    /// The agent's URL, or `None` in standalone — where there is nothing to ask,
    /// and therefore nothing to dial.
    pub(crate) fn agent_url(&self) -> Option<&str> {
        match self {
            AddrSource::Env => None,
            AddrSource::Agent(url) => Some(url),
        }
    }
}

/// Reads [`ORCHESTRATOR_URL_ENV`] and decides the mode. Called once, at start.
pub(crate) fn addr_source_from_env() -> Result<AddrSource> {
    addr_source_from_value(std::env::var(ORCHESTRATOR_URL_ENV).ok().as_deref())
}

/// The testable decision body (the shape `admission_budget_from_value` uses:
/// the raw value in, so no test ever mutates process env).
///
/// Unset ⇒ [`AddrSource::Env`]. Set ⇒ [`AddrSource::Agent`].
///
/// Set-but-BLANK fails startup loudly rather than falling back to standalone —
/// unlike the address vars below it, where blank means "unset" and the answer is
/// a documented default. This var does not select a value, it selects the
/// AUTHORITY for eight values; inferring "standalone" from a blank one is the
/// silent-fallback this whole file exists to refuse, and the operator who
/// exported an empty `ORCHESTRATOR_URL` meant to be managed.
fn addr_source_from_value(raw: Option<&str>) -> Result<AddrSource> {
    match raw {
        None => Ok(AddrSource::Env),
        Some(url) if url.trim().is_empty() => bail!(
            "{ORCHESTRATOR_URL_ENV} is set but blank: managed boot has no agent to ask, \
             and blank is NOT standalone — the two modes are disjoint, so guessing \
             standalone here would boot this front door against default addresses \
             nobody is on. Unset the var for standalone, or give the agent's URL."
        ),
        Some(url) => Ok(AddrSource::Agent(url.trim().to_string())),
    }
}

/// The eight `(env key → address)` pairs, whatever they were resolved FROM.
///
/// Its own type, deliberately, rather than `ProcessWiring`: this is the value the
/// two modes must agree on, so it needs `PartialEq`/`Debug` to be compared in a
/// test — and widening a `core/lifecycle` type (no `PartialEq`, private `peers`,
/// read-only through `peer_or`) for this crate's test would be the tail wagging
/// the dog. It is also the smaller claim: the pairs are what the modes decide,
/// [`ResolvedAddrs::to_wiring`] is the mechanical part they share.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedAddrs {
    /// Keyed by env key even in managed mode: the key is this table's stable
    /// identity for an address, and naming it that way keeps the two modes'
    /// answers comparable pair-for-pair.
    pairs: Vec<(&'static str, String)>,
}

impl ResolvedAddrs {
    fn addr(&self, env_key: &str) -> &str {
        self.pairs
            .iter()
            .find(|(key, _)| *key == env_key)
            .map(|(_, addr)| addr.as_str())
            .unwrap_or_else(|| panic!("{env_key} was never resolved (not in ADDR_SPECS?)"))
    }

    /// Feeds the addresses into `ProcessWiring` — the SAME struct, built the same
    /// way, in both modes. This is why the managed path needs no change below
    /// this line: `cmd/gateway-svc/src/lib.rs` (whose literal `Stub::new("<domain>"`
    /// calls archcheck rule 17 text-scans) and every `Stub` under it are handed
    /// addresses exactly as before, and stay blind to where they came from.
    pub(crate) fn to_wiring(&self) -> ProcessWiring {
        let mut wiring = ProcessWiring::new();
        for spec in ADDR_SPECS {
            let addr = self.addr(spec.env_key);
            wiring = match spec.class {
                AddrClass::Edge => wiring.with_peer(spec.provider, addr),
                AddrClass::Passthrough { prefix } => wiring.with_passthrough(prefix, addr),
            };
        }
        wiring
    }
}

/// Resolves all eight addresses from `source`.
///
/// Both effects are injected, which is what makes this file's claims provable:
/// `env_lookup` is `std::env::var` in production, and `ask_agent` is
/// `remote::resolve_peer` bound to the agent's URL. In [`AddrSource::Env`] the
/// `ask_agent` closure is never called — not "usually not": the standalone arm
/// has no await in it at all, which `env_mode_asks_no_agent` pins with a
/// resolver that panics if touched.
pub(crate) async fn gateway_addrs<L, F, Fut>(
    source: &AddrSource,
    env_lookup: L,
    ask_agent: F,
) -> Result<ResolvedAddrs>
where
    L: Fn(&'static str) -> Option<String>,
    F: Fn(&'static str, AddrKind) -> Fut,
    Fut: Future<Output = WireAnswer>,
{
    let mut pairs = Vec::with_capacity(ADDR_SPECS.len());
    for spec in ADDR_SPECS {
        let addr = match source {
            AddrSource::Env => addr_or_default(env_lookup(spec.env_key), spec.env_default),
            AddrSource::Agent(_) => {
                managed_addr(spec, ask_agent(spec.provider, spec.class.kind()).await)?
            }
        };
        pairs.push((spec.env_key, addr));
    }
    Ok(ResolvedAddrs { pairs })
}

/// Standalone's rule, unchanged: the env value, falling back to `default` when
/// unset or blank — generalizing `characters-svc`'s bespoke
/// `characters_edge_addr()` to any provider's peer address (a NUMERIC
/// `host:port`, e.g. `127.0.0.1:9000`; Rust's `SocketAddr` needs a literal IP,
/// unlike Go's dialer).
fn addr_or_default(raw: Option<String>, default: &str) -> String {
    raw.filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Managed's rule: ONE pure decision over the agent's typed answer, per class.
///
/// Pure and total, so every branch below is exercisable with no I/O and no fake
/// server — the taxonomy is the whole reason this is decidable at all (nothing
/// here reads [`ResolveError`]'s prose; `code` is the discriminator, `message`
/// is for the operator).
fn managed_addr(spec: &AddrSpec, answer: WireAnswer) -> Result<String> {
    match answer {
        Ok(addrs) => exactly_one(spec, addrs),
        // The ONE code the per-class policy branches on: a fact about the fleet.
        Err(ResolveError::Refused { code: ErrorCode::UnknownPeer, status, message }) => {
            match spec.class {
                AddrClass::Edge => bail!(
                    "the agent answered unknown_peer for edge peer {:?} ({}): this topology \
                     does not run it. Refusing to boot — the standalone default ({:?}) is \
                     an address nobody is on, and a Stub pointed at it would fail one dial \
                     at a time in another process, far from here. [{status}: {message}]",
                    spec.provider,
                    spec.env_key,
                    spec.env_default,
                ),
                // Exactly today's blank-default semantics (`env_default: ""`):
                // the proxy table drops a blank-origin route, so the prefix is
                // unrouted and 404s. The agent said this origin is not a thing in
                // this topology — which is the same fact an unset env var states.
                AddrClass::Passthrough { prefix } => {
                    tracing::warn!(
                        prefix,
                        provider = spec.provider,
                        status,
                        %message,
                        "no HTTP origin in this topology — {prefix} will 404 (the agent \
                         answered unknown_peer; same as an unset {})",
                        spec.env_key,
                    );
                    Ok(String::new())
                }
            }
        }
        // Everything else is fatal for EVERY class — passthroughs included.
        // `unknown_route` most of all: it means the agent does not speak this
        // contract, which is a statement about the AGENT and never "that service
        // has no origin". A managed boot whose agent cannot be asked the question
        // has not learned that the answer is "nothing".
        Err(error) => bail!(
            "cannot resolve {} ({:?}, {:?}) from the orchestrator: {error}. Managed boot \
             does not fall back to env — the modes are disjoint.",
            spec.env_key,
            spec.provider,
            spec.class,
        ),
    }
}

/// A dial-time re-resolver for a MANAGED edge peer (A5): a [`remote::PeerResolver`] that
/// re-asks the orchestrator agent for `provider`'s edge address on EVERY dial, so a
/// moved peer is picked up by the stub's reconnecting caller without restarting this
/// front door. The boot snapshot the gateway route table reads is still the one
/// [`gateway_addrs`] resolved at start; THIS drives the capability stub's live dials.
///
/// Single-address in this phase: it reduces the agent's answer through the SAME
/// [`exactly_one`] policy the boot path uses, so a topology that grew a second instance
/// fails one dial at a time (503) rather than silently sending half the traffic nowhere
/// — the load-balancing that would accept the list is a later phase. A resolve failure
/// (unreachable agent, `unknown_peer`, malformed) is a `host:port`-string error the
/// dialer maps to a 503, which is exactly what an unresolvable peer is.
pub(crate) fn edge_resolver(agent_url: &str, provider: &'static str) -> remote::PeerResolver {
    let agent_url = agent_url.to_string();
    std::sync::Arc::new(move || {
        let agent_url = agent_url.clone();
        let fut = async move {
            let answer = remote::resolve_peer(&agent_url, provider, AddrKind::Edge).await;
            let spec = edge_spec(provider);
            match answer {
                Ok(addrs) => exactly_one(spec, addrs).map_err(|e| e.to_string()),
                Err(error) => Err(format!(
                    "re-resolve edge peer {provider:?} from the orchestrator: {error}"
                )),
            }
        };
        let boxed: std::pin::Pin<
            Box<dyn std::future::Future<Output = std::result::Result<String, String>> + Send>,
        > = Box::pin(fut);
        boxed
    })
}

/// The [`AddrSpec`] for an EDGE `provider` (a peer with a `_EDGE_ADDR`). `accounts` is
/// both an edge peer and a passthrough origin, so the class filter is load-bearing —
/// [`edge_resolver`] only ever resolves the edge address.
fn edge_spec(provider: &str) -> &'static AddrSpec {
    ADDR_SPECS
        .iter()
        .find(|s| s.provider == provider && s.class == AddrClass::Edge)
        .unwrap_or_else(|| panic!("edge_resolver called for non-edge provider {provider:?}"))
}

/// The list shape, decided rather than defaulted (see the module doc): M1 answers
/// exactly one address, and neither zero nor many is an address this boot path
/// can act on.
fn exactly_one(spec: &AddrSpec, addrs: Vec<String>) -> Result<String> {
    match addrs.len() {
        1 => {
            let addr = addrs.into_iter().next().expect("len == 1");
            if addr.trim().is_empty() {
                bail!(
                    "the agent answered a BLANK address for {} ({:?}): a blank is not an \
                     address — for a passthrough it would silently unroute the prefix, and \
                     for an edge peer it is a Stub that cannot dial.",
                    spec.env_key,
                    spec.provider,
                );
            }
            Ok(addr)
        }
        0 => bail!(
            "the agent answered an EMPTY address list for {} ({:?}): that is a LIVENESS \
             answer (\"it is a thing; nothing is live right now\"), NOT the topology \
             refusal a 404 carries, and it is not an address. M1's agent never emits it \
             and this boot path cannot act on it — waiting/re-resolving is M2's job.",
            spec.env_key,
            spec.provider,
        ),
        n => bail!(
            "the agent answered {n} addresses for {} ({:?}): choosing between instances is \
             load balancing, which this front door does not do in M1 — silently taking the \
             first would look healthy while sending part of the traffic nowhere.",
            spec.env_key,
            spec.provider,
        ),
    }
}

#[cfg(test)]
#[path = "addrs_tests.rs"]
mod addrs_tests;
