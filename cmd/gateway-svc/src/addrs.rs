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
//! right now" — a LIVENESS answer that M1's agent never emits and that a boot
//! path may not treat as final. So the BOOT snapshot refuses it loudly (edge:
//! [`nonempty_list`]; passthrough: [`exactly_one`]) instead of
//! `.first().cloned().unwrap_or_default()`-ing it into an empty address string:
//! an empty list is not an address, and folding it into the passthrough's
//! empty-origin rule would answer a liveness question with a topology decision.
//! Acting on it (waiting, re-resolving) is M2's job.
//!
//! # Two addresses is a LIST, not a refusal (C2 round-robin)
//!
//! An EDGE peer that resolves to TWO or more instances is the replica/round-robin
//! shape: [`nonempty_list`] hands the WHOLE set through to a `remote::Pool` (in
//! the capability stub) and into `opsapi::PEER_SLOT` (for the gateway route
//! table's own per-provider pool), so client-side load balancing spreads the
//! traffic across every live instance. The old `exactly_one` `n>1` refusal ("M1
//! does not load balance") is gone for edge peers. A PASSTHROUGH origin is still a
//! single reverse-proxy target, so [`exactly_one`] still refuses `n>1` there — two
//! `/admin` origins is a genuine ambiguity this front door cannot resolve.

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
    /// answers comparable pair-for-pair. The VALUE is a SET (C2): an edge peer
    /// carries ALL its live instances (N ≥ 1); a passthrough carries its single
    /// origin (0 elements = "no origin", the blank/drop case).
    pairs: Vec<(&'static str, Vec<String>)>,
}

impl ResolvedAddrs {
    fn addrs(&self, env_key: &str) -> &[String] {
        self.pairs
            .iter()
            .find(|(key, _)| *key == env_key)
            .map(|(_, addrs)| addrs.as_slice())
            .unwrap_or_else(|| panic!("{env_key} was never resolved (not in ADDR_SPECS?)"))
    }

    /// Feeds the addresses into `ProcessWiring` — the SAME struct, built the same
    /// way, in both modes. This is why the managed path needs no change below
    /// this line: `cmd/gateway-svc/src/lib.rs` (whose literal `Stub::new("<domain>"`
    /// calls archcheck rule 17 text-scans) and every `Stub` under it are handed
    /// addresses exactly as before, and stay blind to where they came from.
    ///
    /// An edge peer's whole instance SET goes to `with_peer_set` (a `remote::Pool`
    /// then round-robins across it, and the same set reaches the gateway route
    /// table via `PEER_SLOT`); a passthrough's single origin goes to
    /// `with_passthrough` (a 0-element set is the blank origin the proxy table
    /// drops — byte-identical to an unset env var).
    pub(crate) fn to_wiring(&self) -> ProcessWiring {
        let mut wiring = ProcessWiring::new();
        for spec in ADDR_SPECS {
            let addrs = self.addrs(spec.env_key);
            wiring = match spec.class {
                AddrClass::Edge => wiring.with_peer_set(spec.provider, addrs.to_vec()),
                AddrClass::Passthrough { prefix } => {
                    wiring.with_passthrough(prefix, addrs.first().cloned().unwrap_or_default())
                }
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
        let addrs = match source {
            AddrSource::Env => env_addrs(spec, env_lookup(spec.env_key)),
            AddrSource::Agent(_) => {
                managed_addrs(spec, ask_agent(spec.provider, spec.class.kind()).await)?
            }
        };
        pairs.push((spec.env_key, addrs));
    }
    Ok(ResolvedAddrs { pairs })
}

/// Standalone's per-spec set. An edge peer is a ONE-element set (the env value or its
/// default) — byte-identical to before, and `to_wiring` hands it to `with_peer_set` as a
/// pool-of-1 (`fixed`/`Reconnecting` downstream, no re-resolve). A passthrough is one
/// element, or ZERO when unset/blank (the blank-origin/drop case the proxy table renders
/// a 404 — preserved exactly).
fn env_addrs(spec: &AddrSpec, raw: Option<String>) -> Vec<String> {
    let addr = addr_or_default(raw, spec.env_default);
    match spec.class {
        AddrClass::Edge => vec![addr],
        AddrClass::Passthrough { .. } => {
            if addr.trim().is_empty() {
                Vec::new()
            } else {
                vec![addr]
            }
        }
    }
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
fn managed_addrs(spec: &AddrSpec, answer: WireAnswer) -> Result<Vec<String>> {
    match answer {
        // An edge peer takes the WHOLE instance set (C2 — a `remote::Pool` load-balances
        // across it); a passthrough collapses to its single reverse-proxy origin.
        Ok(addrs) => match spec.class {
            AddrClass::Edge => nonempty_list(spec, addrs),
            AddrClass::Passthrough { .. } => exactly_one(spec, addrs).map(|a| vec![a]),
        },
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
                // A ZERO-element set is that blank origin.
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
                    Ok(Vec::new())
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

/// A dial-time re-resolver for a MANAGED edge peer's INSTANCE LIST (A5 + C2): a
/// [`remote::PeerListResolver`] that re-asks the orchestrator agent for ALL of
/// `provider`'s live edge instances on the pool's refresh cadence, so both a moved peer
/// AND a scale up/down are picked up by the stub's `remote::Pool` without restarting this
/// front door. The boot snapshot the gateway route table reads is still the one
/// [`gateway_addrs`] resolved at start; THIS drives the capability stub's live pool.
///
/// It hands the agent's answer through VERBATIM (the old `exactly_one` collapse is gone):
/// the whole set is the pool's instance list, so it round-robins across every live
/// instance. An empty `[]` is passed through un-collapsed — the pool renders it
/// un-routable (503) rather than a silent first-pick, preserving the "not-yet-live is not
/// an address" boot signal at the LIVE layer too. A resolve failure (unreachable agent,
/// `unknown_peer`, malformed) is a human-string error the pool's refresh keeps the
/// existing set for and retries — an unresolvable list is as unavailable as an
/// unreachable one.
///
/// F2 (latent, M2): a LIVE `Ok(vec![])` is MORE destructive than an `Err` here —
/// `Pool::refresh` reconciles to the empty set, tearing down every HEALTHY instance's
/// conn + probe (503), whereas an `Err` KEEPS the existing set for a retry. This is
/// dormant on M1's fleet (the agent never emits `[]`); when liveness-tracked `resolve`
/// (`200 {addrs:[]}`) lands in M2, the `[]` case needs revisiting so a transient "nothing
/// live right now" does not tear down a working pool.
pub(crate) fn edge_list_resolver(
    agent_url: &str,
    provider: &'static str,
) -> remote::PeerListResolver {
    let agent_url = agent_url.to_string();
    std::sync::Arc::new(move || {
        let agent_url = agent_url.clone();
        let fut = async move {
            match remote::resolve_peer(&agent_url, provider, AddrKind::Edge).await {
                Ok(addrs) => Ok(addrs),
                Err(error) => Err(format!(
                    "re-resolve edge peer {provider:?} from the orchestrator: {error}"
                )),
            }
        };
        let boxed: std::pin::Pin<
            Box<
                dyn std::future::Future<Output = std::result::Result<Vec<String>, String>> + Send,
            >,
        > = Box::pin(fut);
        boxed
    })
}

/// The EDGE boot-snapshot shape (C2): ANY non-empty set is accepted — the whole list
/// flows to a `remote::Pool` (capability stub) and `PEER_SLOT` (route table) so client-side
/// round-robin spreads traffic across every live instance. Only the un-actionable answers
/// are refused: an EMPTY list (a liveness answer M1 never emits and this boot path cannot
/// act on — M2's territory) and a BLANK member (not an address a Stub can dial). The old
/// `exactly_one` `n>1` refusal is intentionally absent — two instances is a LIST now.
fn nonempty_list(spec: &AddrSpec, addrs: Vec<String>) -> Result<Vec<String>> {
    if addrs.is_empty() {
        bail!(
            "the agent answered an EMPTY address list for {} ({:?}): that is a LIVENESS \
             answer (\"it is a thing; nothing is live right now\"), NOT the topology \
             refusal a 404 carries, and it is not an address. M1's agent never emits it \
             and this boot path cannot act on it — waiting/re-resolving is M2's job.",
            spec.env_key,
            spec.provider,
        );
    }
    if addrs.iter().any(|a| a.trim().is_empty()) {
        bail!(
            "the agent answered a BLANK address among the instances for {} ({:?}): a blank \
             is not an address a Stub can dial.",
            spec.env_key,
            spec.provider,
        );
    }
    Ok(addrs)
}

/// The single-origin shape for a PASSTHROUGH: a reverse-proxy origin is one target, so
/// zero (a liveness answer M1 never emits), a blank, or MANY (a genuine ambiguity this
/// front door cannot resolve — unlike an edge peer, a proxy does not load-balance here)
/// are all refused. Edge peers use [`nonempty_list`] instead, which accepts the set.
fn exactly_one(spec: &AddrSpec, addrs: Vec<String>) -> Result<String> {
    match addrs.len() {
        1 => {
            let addr = addrs.into_iter().next().expect("len == 1");
            if addr.trim().is_empty() {
                bail!(
                    "the agent answered a BLANK origin for {} ({:?}): a blank is not an \
                     address — for a passthrough it would silently unroute the prefix.",
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
            "the agent answered {n} origins for {} ({:?}): a passthrough is a single \
             reverse-proxy target — choosing between origins is not something this front \
             door does, and silently taking the first would send part of the traffic \
             nowhere while looking healthy.",
            spec.env_key,
            spec.provider,
        ),
    }
}

#[cfg(test)]
#[path = "addrs_tests.rs"]
mod addrs_tests;
