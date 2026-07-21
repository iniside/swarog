use super::*;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// The RELATIVE CA paths every edge-dialing service carries in its
/// `[service.env]` (the D-PREPARE contract: `run/weles/edge-ca.{crt,key}`,
/// resolved against cwd = repo root where the `edge-ca` prepare hook writes it).
const CA_CERT: &str = "run/weles/edge-ca.crt";
const CA_KEY: &str = "run/weles/edge-ca.key";

/// Serializes the tests that mutate process-global env (passthrough forwarding):
/// same `OnceLock<Mutex>` shape as `prep_tests::env_guard`, copied not shared.
fn env_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The committed split fixture's services (was `split_fleet()`).
fn split() -> Vec<ServiceDef> {
    crate::fleet_toml::load_split_fixture().services
}

/// The committed monolith fixture's single service (was `monolith()`).
fn monolith() -> ServiceDef {
    crate::fleet_toml::load_monolith_fixture().services.into_iter().next().unwrap()
}

/// Owned `Addrs::Told` from literal peer tuples — the synthetic-fleet helper the
/// tests below build hand-shaped fleets with.
fn told(peers: &[(&str, &str, AddrKind)]) -> Addrs {
    Addrs::Told(peers.iter().map(|(k, p, kind)| (k.to_string(), p.to_string(), *kind)).collect())
}

/// A minimal owned [`ServiceDef`] for synthetic fleets (empty env).
fn owned_svc(
    name: &str,
    provider: Option<&str>,
    http_port: u16,
    edge_port: Option<u16>,
    addrs: Addrs,
) -> ServiceDef {
    ServiceDef {
        name: name.to_string(),
        pkg: name.to_string(),
        provider: provider.map(str::to_string),
        placement: None,
        http_port,
        edge_port,
        player_port: None,
        addrs,
        env: BTreeMap::new(),
    }
}

/// Removes allowlisted ambient-env keys from a composed env so a golden
/// assertion doesn't depend on the machine running the test (RUST_LOG, PATH,
/// etc. vary by shell).
///
/// COLLISION GUARD: if a fixture/composed key (PORT/EDGE_ADDR/service env/…)
/// ever collided with a [`SERVICE_ENV_ALLOWLIST`] name, this filter would
/// silently strip it from every golden assert. `no_manifest_key_collides_with_the_allowlist`
/// below pins that this cannot happen without a test failure.
fn strip_allowlist(env: &BTreeMap<OsString, OsString>) -> BTreeMap<OsString, OsString> {
    env.iter()
        .filter(|(key, _)| {
            !SERVICE_ENV_ALLOWLIST
                .iter()
                .any(|allowed| key.to_string_lossy().eq_ignore_ascii_case(allowed))
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn expected(pairs: &[(&str, &str)]) -> BTreeMap<OsString, OsString> {
    pairs
        .iter()
        .map(|(k, v)| (OsString::from(*k), OsString::from(*v)))
        .collect()
}

/// ONE table-driven golden over the COMPLETE composed env (modulo the allowlist
/// strip) for ALL 12 split services + the monolith — composed from the SHIPPED
/// `fleet.split.toml` / `fleet.monolith.toml`, not a Rust table. Deliberately
/// verbose: every expected map is written out in full, so ANY drifted key or
/// value — added, removed, or changed — fails this test by name.
///
/// Composed with an EMPTY passthrough, so `DATABASE_URL` (which reaches a
/// service only when weles's own env carries it and the fleet passes it through)
/// deterministically does not appear — its forwarding is proven separately in
/// `a_passthrough_key_is_forwarded_from_weles_own_env`. The CA paths and pool
/// cap are now literal `[service.env]` values (formerly injected), so they DO
/// appear.
#[test]
fn full_fleet_env_goldens() {
    // Derived from AGENT_PORT, not spelled `http://127.0.0.1:8300`: a literal
    // here would be a second authority for the agent's port.
    let agent = agent_url();
    let goldens: &[(&str, &[(&str, &str)])] = &[
        (
            "accounts-svc",
            &[
                ("PORT", ":8084"),
                ("EDGE_ADDR", ":9003"),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", CA_CERT),
                ("EDGE_CA_KEY", CA_KEY),
                ("ACCOUNTS_DEV_AUTH", "1"),
            ],
        ),
        (
            "apikeys-svc",
            &[
                ("PORT", ":8091"),
                ("EDGE_ADDR", ":9009"),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", CA_CERT),
                ("EDGE_CA_KEY", CA_KEY),
                ("APIKEYS_DEV_SEED", "1"),
            ],
        ),
        (
            "audit-svc",
            &[
                ("PORT", ":8086"),
                ("EDGE_ADDR", ":9004"),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", CA_CERT),
                ("EDGE_CA_KEY", CA_KEY),
            ],
        ),
        (
            // Deliberately NO SCHEDULER_ENABLED — Development-flavor parity.
            "scheduler-svc",
            &[
                ("PORT", ":8087"),
                ("EDGE_ADDR", ":9005"),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", CA_CERT),
                ("EDGE_CA_KEY", CA_KEY),
            ],
        ),
        (
            "rating-svc",
            &[
                ("PORT", ":8089"),
                ("EDGE_ADDR", ":9007"),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", CA_CERT),
                ("EDGE_CA_KEY", CA_KEY),
            ],
        ),
        (
            "leaderboard-svc",
            &[
                ("PORT", ":8090"),
                ("EDGE_ADDR", ":9008"),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", CA_CERT),
                ("EDGE_CA_KEY", CA_KEY),
            ],
        ),
        (
            "match-svc",
            &[
                ("PORT", ":8088"),
                ("EDGE_ADDR", ":9006"),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", CA_CERT),
                ("EDGE_CA_KEY", CA_KEY),
                ("RATING_EDGE_ADDR", "127.0.0.1:9007"),
            ],
        ),
        (
            "config-svc",
            &[
                ("PORT", ":8083"),
                ("EDGE_ADDR", ":9002"),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", CA_CERT),
                ("EDGE_CA_KEY", CA_KEY),
            ],
        ),
        (
            "characters-svc",
            &[
                ("PORT", ":8080"),
                ("EDGE_ADDR", ":9000"),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", CA_CERT),
                ("EDGE_CA_KEY", CA_KEY),
                ("CONFIG_EDGE_ADDR", "127.0.0.1:9002"),
            ],
        ),
        (
            "inventory-svc",
            &[
                ("PORT", ":8081"),
                ("EDGE_ADDR", ":9001"),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", CA_CERT),
                ("EDGE_CA_KEY", CA_KEY),
                ("CHARACTERS_EDGE_ADDR", "127.0.0.1:9000"),
                ("CONFIG_EDGE_ADDR", "127.0.0.1:9002"),
                ("INVENTORY_DEV_GRANT", "1"),
            ],
        ),
        (
            // Pure-transport front door (`Addrs::Asks`): no EDGE_ADDR of its own,
            // no pool cap, but it DOES carry the CA (dials every peer's edge) and
            // gets ORCHESTRATOR_URL — none of the eight address keys it used to
            // carry, only the URL it asks each of them for.
            "gateway-svc",
            &[
                ("PORT", ":8082"),
                ("EDGE_CA_CERT", CA_CERT),
                ("EDGE_CA_KEY", CA_KEY),
                ("PLAYER_EDGE_ADDR", ":9100"),
                ("TLS_MODE", "off"),
                ("ORCHESTRATOR_URL", agent.as_str()),
            ],
        ),
        (
            "admin-svc",
            &[
                ("PORT", ":8085"),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", CA_CERT),
                ("EDGE_CA_KEY", CA_KEY),
                ("CHARACTERS_EDGE_ADDR", "127.0.0.1:9000"),
                ("INVENTORY_EDGE_ADDR", "127.0.0.1:9001"),
                ("CONFIG_EDGE_ADDR", "127.0.0.1:9002"),
                ("ACCOUNTS_EDGE_ADDR", "127.0.0.1:9003"),
                ("AUDIT_EDGE_ADDR", "127.0.0.1:9004"),
                ("SCHEDULER_EDGE_ADDR", "127.0.0.1:9005"),
                ("APIKEYS_EDGE_ADDR", "127.0.0.1:9009"),
                ("ADMIN_COOKIE_SECURE", "0"),
                ("TRUSTED_PROXY_CIDRS", "127.0.0.1/32"),
            ],
        ),
    ];

    let fleet = split();
    assert_eq!(
        fleet.len(),
        goldens.len(),
        "every split service must have a golden (add the new one here)"
    );
    for (name, pairs) in goldens {
        let svc = fleet
            .iter()
            .find(|svc| svc.name == *name)
            .unwrap_or_else(|| panic!("{name} missing from the split fixture"));
        let env = strip_allowlist(&compose_env_with_fleet(svc, &[], &fleet));
        assert_eq!(env, expected(pairs), "composed env drifted for {name}");
    }

    // Monolith golden.
    let mono = monolith();
    let env = strip_allowlist(&compose_env_with_fleet(&mono, &[], std::slice::from_ref(&mono)));
    let want = expected(&[
        ("PORT", ":8080"),
        ("DATABASE_POOL_MAX_CONNECTIONS", "20"),
        ("EDGE_CA_CERT", CA_CERT),
        ("EDGE_CA_KEY", CA_KEY),
        ("PLAYER_EDGE_ADDR", ":9100"),
        ("APIKEYS_DEV_SEED", "1"),
        ("ACCOUNTS_DEV_AUTH", "1"),
        ("INVENTORY_DEV_GRANT", "1"),
        ("TLS_MODE", "off"),
        ("ADMIN_COOKIE_SECURE", "0"),
        ("TRUSTED_PROXY_CIDRS", "127.0.0.1/32"),
    ]);
    assert_eq!(env, want, "composed env drifted for monolith");
}

/// THE failing branch of the OLD injection: weles no longer synthesizes
/// `DATABASE_URL` / `EDGE_CA_*` / `DATABASE_POOL_MAX_CONNECTIONS` of its own.
/// With an empty passthrough and a service whose own env declares none, NONE of
/// them reach the composed env — the domain knowledge this module shed. A
/// regression that re-injected any of them would fail HERE.
#[test]
fn compose_injects_no_domain_env_without_operator_supplying_it() {
    let svc = owned_svc("bare-svc", Some("bare"), 8080, Some(9000), told(&[]));
    let env = compose_env_with_fleet(&svc, &[], std::slice::from_ref(&svc));
    for key in ["DATABASE_URL", "EDGE_CA_CERT", "EDGE_CA_KEY", "DATABASE_POOL_MAX_CONNECTIONS"] {
        assert!(
            !env.contains_key(&OsString::from(key)),
            "weles must not inject {key} — it is the operator's `env`/`passthrough` job"
        );
    }
}

/// The OTHER half: a `passthrough` key IS forwarded from weles's OWN environment
/// (the domain-blind channel — weles knows the key NAME, never its meaning). A
/// uniquely-named key so nothing else in the binary reads it; guarded because
/// process env is global.
#[test]
fn a_passthrough_key_is_forwarded_from_weles_own_env() {
    let _guard = env_guard();
    let key = "WELES_MANIFEST_TEST_PASSTHROUGH";
    let previous = std::env::var_os(key);
    std::env::set_var(key, "from-weles-env");

    let svc = owned_svc("bare-svc", Some("bare"), 8080, Some(9000), told(&[]));
    let env = compose_env_with_fleet(&svc, &[key.to_string()], std::slice::from_ref(&svc));
    assert_eq!(
        env.get(&OsString::from(key)),
        Some(&OsString::from("from-weles-env")),
        "a passthrough key present in weles's env must be forwarded to the service"
    );

    match previous {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
}

/// Guard for `strip_allowlist`'s blind spot: a composed key colliding with an
/// allowlist name would be silently stripped from every golden. Pin that no key
/// the composition introduces (synthesized in `compose_env_with_fleet` or listed
/// in a fixture's `env`) is an allowlist name.
#[test]
fn no_manifest_key_collides_with_the_allowlist() {
    let mut services = split();
    services.push(monolith());
    let synthesized = ["PORT", "EDGE_ADDR", ORCHESTRATOR_URL_ENV];
    for svc in &services {
        for key in synthesized
            .iter()
            .map(|s| s.to_string())
            .chain(svc.addrs.told().iter().map(|(k, _, _)| k.clone()))
            .chain(svc.env.keys().cloned())
        {
            assert!(
                !SERVICE_ENV_ALLOWLIST.iter().any(|a| a.eq_ignore_ascii_case(&key)),
                "{}: composed key {key} collides with SERVICE_ENV_ALLOWLIST — \
                 strip_allowlist would hide it from the goldens",
                svc.name
            );
        }
    }
}

/// A two-service synthetic fleet: `provider-svc` (both ports) and
/// `consumer-svc`, told the provider's address by both kinds. Only the ports
/// vary, so a test can move a port and watch the consumer's env.
fn synthetic_peer_fleet(provider_edge: Option<u16>, provider_http: u16) -> Vec<ServiceDef> {
    vec![
        owned_svc("provider-svc", Some("provider"), provider_http, provider_edge, told(&[])),
        owned_svc(
            "consumer-svc",
            Some("consumer"),
            1,
            None,
            told(&[
                ("PROVIDER_EDGE_ADDR", "provider", AddrKind::Edge),
                ("PROVIDER_HTTP_ADDR", "provider", AddrKind::Http),
            ]),
        ),
    ]
}

/// consumer-svc FIRST, provider-svc LAST — a consumer that boots BEFORE the one
/// peer it names, declared as `kind`.
fn consumer_before_provider(kind: AddrKind) -> Vec<ServiceDef> {
    let addrs = match kind {
        AddrKind::Edge => told(&[("PROVIDER_EDGE_ADDR", "provider", AddrKind::Edge)]),
        AddrKind::Http => told(&[("PROVIDER_HTTP_ADDR", "provider", AddrKind::Http)]),
    };
    vec![
        owned_svc("consumer-svc", Some("consumer"), 1, None, addrs),
        owned_svc("provider-svc", Some("provider"), 8080, Some(9000), told(&[])),
    ]
}

fn composed_value(fleet: &[ServiceDef], svc_name: &str, key: &str) -> String {
    let svc = fleet.iter().find(|svc| svc.name == svc_name).unwrap();
    compose_env_with_fleet(svc, &[], fleet)
        .get(&OsString::from(key))
        .unwrap_or_else(|| panic!("{svc_name} composed env has no {key}"))
        .to_string_lossy()
        .into_owned()
}

/// THE previously-broken branch. Before `peers`, a consumer's peer address was a
/// hand-written literal: moving the provider's port left the consumer pointing
/// at the old one. Prove the port field is now the authority — move it, and the
/// consumer's COMPOSED env moves with it.
#[test]
fn moving_a_providers_port_propagates_to_its_consumers_env() {
    let before = synthetic_peer_fleet(Some(9000), 8080);
    assert_eq!(composed_value(&before, "consumer-svc", "PROVIDER_EDGE_ADDR"), "127.0.0.1:9000");
    assert_eq!(composed_value(&before, "consumer-svc", "PROVIDER_HTTP_ADDR"), "127.0.0.1:8080");

    let after = synthetic_peer_fleet(Some(9999), 8888);
    assert_eq!(
        composed_value(&after, "consumer-svc", "PROVIDER_EDGE_ADDR"),
        "127.0.0.1:9999",
        "edge_port is the authority: moving it must reach the consumer's env"
    );
    assert_eq!(
        composed_value(&after, "consumer-svc", "PROVIDER_HTTP_ADDR"),
        "127.0.0.1:8888",
        "http_port is the authority: moving it must reach the consumer's env"
    );
}

/// The kinds are not interchangeable: the same provider, dialed both ways, must
/// yield its two DIFFERENT ports (real case: `accounts`, edge 9003 + http 8084).
#[test]
fn the_two_kinds_read_different_port_fields() {
    let fleet = synthetic_peer_fleet(Some(9000), 8080);
    assert_ne!(
        composed_value(&fleet, "consumer-svc", "PROVIDER_EDGE_ADDR"),
        composed_value(&fleet, "consumer-svc", "PROVIDER_HTTP_ADDR"),
    );

    // ...and the real fleet's dual-kind provider still proves it on LIVE data,
    // read where the gateway now actually reads it (`accounts` is dialed both
    // ways — edge 9003 + http 8084 — but gateway asks the agent for both).
    let real = PeerAddrs::from_fleet(&split());
    assert_eq!(real.lookup("accounts", AddrKind::Edge), vec!["127.0.0.1:9003".to_string()]);
    assert_eq!(real.lookup("accounts", AddrKind::Http), vec!["127.0.0.1:8084".to_string()]);
}

/// `AddrKind::Edge` against a service with `edge_port: None` is a programmer
/// error — it must fail LOUDLY and name the offender.
#[test]
#[should_panic(expected = "peer \"provider\" as AddrKind::Edge")]
fn edge_kind_against_a_service_without_an_edge_panics() {
    let fleet = synthetic_peer_fleet(None, 8080);
    composed_value(&fleet, "consumer-svc", "PROVIDER_EDGE_ADDR");
}

/// The same, phrased against the REAL def that has `edge_port: None`: nothing
/// about `admin` lets it be dialed as an edge peer.
#[test]
#[should_panic(expected = "peer \"admin\" as AddrKind::Edge")]
fn edge_kind_against_real_admin_svc_panics() {
    let fleet = split();
    let admin = fleet.iter().find(|svc| svc.provider.as_deref() == Some("admin")).unwrap();
    assert!(admin.edge_port.is_none(), "fixture assumption broken: it now serves an edge");
    peer_addr(&fleet, "some-consumer-svc", "admin", AddrKind::Edge);
}

/// An unknown provider is the other half of the same programmer error.
#[test]
#[should_panic(expected = "peer \"ghost\", which no service in this fleet provides")]
fn an_unknown_provider_panics() {
    peer_addr(&split(), "gateway-svc", "ghost", AddrKind::Edge);
}

// ---------------------------------------------------------------------------
// PeerAddrs — what the agent's `resolve` answers from
// ---------------------------------------------------------------------------

/// TWO instances of one provider must render as two DISTINCT addresses — the
/// whole point of the list shape, and the branch a name round-trip breaks.
#[test]
fn two_instances_of_one_provider_resolve_to_two_distinct_addresses() {
    let instance = |http_port, edge_port| {
        owned_svc("characters-svc", Some("characters"), http_port, Some(edge_port), told(&[]))
    };
    let fleet = vec![instance(8080, 9000), instance(8180, 9100)];
    let map = PeerAddrs::from_fleet(&fleet);

    assert_eq!(
        map.lookup("characters", AddrKind::Edge),
        vec!["127.0.0.1:9000".to_string(), "127.0.0.1:9100".to_string()],
        "each instance's address must come from its OWN edge_port"
    );
    assert_eq!(
        map.lookup("characters", AddrKind::Http),
        vec!["127.0.0.1:8080".to_string(), "127.0.0.1:8180".to_string()],
        "each instance's address must come from its OWN http_port"
    );
}

/// The kinds a def actually has, and no others: `edge_port: None` yields no
/// Edge entry at all.
#[test]
fn peer_addrs_omits_a_kind_a_service_does_not_serve() {
    let map = PeerAddrs::from_fleet(&split());
    assert!(
        map.lookup("admin", AddrKind::Edge).is_empty(),
        "admin serves no edge — there is no address to give out"
    );
    assert_eq!(map.lookup("admin", AddrKind::Http), vec!["127.0.0.1:8085".to_string()]);
    // The monolith is unresolvable as DATA (provider: None), not by a branch.
    let mono = PeerAddrs::from_fleet(std::slice::from_ref(&monolith()));
    for kind in [AddrKind::Edge, AddrKind::Http] {
        assert!(
            mono.lookup("characters", kind).is_empty() && mono.lookup("server", kind).is_empty(),
            "the monolith hosts every domain in-process — it is nameable as no provider"
        );
    }
}

/// A service's own `env` is applied AFTER the derived peer addresses, so an `env`
/// key repeating a peer key would silently override the derivation and restore
/// the two-authorities drift. The check is KEY-shaped (peer keys ∩ env keys = ∅).
#[test]
fn no_env_key_shadows_a_derived_peer_key() {
    let mut services = split();
    services.push(monolith());
    for svc in &services {
        for (key, value) in &svc.env {
            assert!(
                !svc.addrs.told().iter().any(|(peer_key, _, _)| peer_key == key),
                "{}: env {key} = {value:?} shadows the SAME key derived from a peer — \
                 env is applied last, so the derived address would be silently discarded",
                svc.name
            );
        }
    }
}

// ---------------------------------------------------------------------------
// managed — the process asks the agent instead of being told by env
// ---------------------------------------------------------------------------

/// A managed process is handed the agent's URL and NONE of the addresses it used
/// to be told: gateway-svc's eight keys are gone, `ORCHESTRATOR_URL` is there.
#[test]
fn a_managed_service_is_handed_the_agent_url_and_no_peer_addresses() {
    let fleet = split();
    let gateway = fleet.iter().find(|svc| svc.name == "gateway-svc").unwrap();
    assert!(gateway.addrs == Addrs::Asks, "fixture assumption: gateway-svc is the one that asks");
    let env = strip_allowlist(&compose_env_with_fleet(gateway, &[], &fleet));

    assert_eq!(
        env.get(&OsString::from("ORCHESTRATOR_URL")),
        Some(&OsString::from(format!("http://127.0.0.1:{AGENT_PORT}"))),
    );
    for key in [
        "CHARACTERS_EDGE_ADDR",
        "INVENTORY_EDGE_ADDR",
        "ACCOUNTS_EDGE_ADDR",
        "MATCH_EDGE_ADDR",
        "LEADERBOARD_EDGE_ADDR",
        "APIKEYS_EDGE_ADDR",
        "ADMIN_HTTP_ADDR",
        "ACCOUNTS_HTTP_ADDR",
    ] {
        assert!(
            !env.contains_key(&OsString::from(key)),
            "a managed process must not ALSO be told {key} by env: it resolves that address, \
             so the env copy is a second authority nobody reads"
        );
    }
}

/// An unmanaged process is handed no URL: the two modes are disjoint, and every
/// other service is still told by env exactly as before.
#[test]
fn an_unmanaged_service_is_handed_no_agent_url() {
    let fleet = split();
    let inventory = fleet.iter().find(|svc| svc.name == "inventory-svc").unwrap();
    let env = compose_env_with_fleet(inventory, &[], &fleet);

    assert_ne!(inventory.addrs, Addrs::Asks);
    assert!(!env.contains_key(&OsString::from("ORCHESTRATOR_URL")));
    assert_eq!(
        env.get(&OsString::from("CHARACTERS_EDGE_ADDR")),
        Some(&OsString::from("127.0.0.1:9000")),
    );
    // The monolith has no peers to resolve and no map to be answered from, so it
    // must never be managed.
    assert_ne!(monolith().addrs, Addrs::Asks);
}

/// The monolith is nameable as no single domain (it hosts all of them), which is
/// what makes it structurally unresolvable as a peer.
#[test]
fn the_monolith_provides_no_short_name_and_dials_no_peers() {
    let mono = monolith();
    assert_eq!(mono.provider, None);
    assert!(mono.addrs.told().is_empty());
}

/// Every split service IS nameable, and uniquely — `peers` and `resolve` key on
/// this, so a duplicate or missing short name would make a lookup ambiguous.
#[test]
fn every_split_service_has_a_unique_short_provider_name() {
    let fleet = split();
    let mut seen = std::collections::BTreeSet::new();
    for svc in &fleet {
        let provider =
            svc.provider.clone().unwrap_or_else(|| panic!("{}: no provider name", svc.name));
        assert!(seen.insert(provider.clone()), "duplicate provider short name {provider:?}");
        assert_eq!(
            svc.name,
            format!("{provider}-svc"),
            "{}: provider short name must be the process's own domain",
            svc.name
        );
    }
}

/// `compose_env_with_fleet` resolves a def against the fleet it is passed, NOT a
/// global assumption. A monolith-shaped def declaring a split-only peer, composed
/// against the monolith fleet, must FAIL rather than silently hand out a split
/// address for a process that topology never starts.
#[test]
#[should_panic(expected = "no service in this fleet provides")]
fn a_monolith_def_may_not_silently_resolve_a_split_only_provider() {
    let mut def = monolith();
    def.addrs = told(&[("CHARACTERS_EDGE_ADDR", "characters", AddrKind::Edge)]);
    let fleet = vec![def.clone()];
    compose_env_with_fleet(&def, &[], &fleet);
}

/// THE boot-order rule, as ONE function both tests below drive: every
/// `AddrKind::Edge` peer must appear strictly earlier in the Vec than the service
/// that declares it. Returns the violations and HOW MANY declarations it examined
/// — a rule whose loop never ran is vacuous, not green.
fn edge_peer_order_violations(fleet: &[ServiceDef]) -> (Vec<String>, usize) {
    let mut violations = Vec::new();
    let mut checked = 0;
    for (index, svc) in fleet.iter().enumerate() {
        for (key, provider, kind) in svc.addrs.told() {
            if *kind != AddrKind::Edge {
                continue;
            }
            checked += 1;
            let position = fleet
                .iter()
                .position(|peer| peer.provider.as_deref() == Some(provider.as_str()))
                .unwrap_or_else(|| panic!("no service provides {provider:?}"));
            if position >= index {
                violations.push(format!(
                    "{}: edge peer {provider:?} ({key}) must appear earlier in the fleet — \
                     the Vec order IS the boot order",
                    svc.name
                ));
            }
        }
    }
    (violations, checked)
}

/// Boot order is DERIVED from the `peers` field, never hand-listed beside it.
#[test]
fn boot_order_respects_edge_peer_dependencies() {
    let (violations, checked) = edge_peer_order_violations(&split());
    assert!(violations.is_empty(), "boot order violates a declared edge peer:\n{violations:#?}");
    // ELEVEN edge declarations: match(1) + characters(1) + inventory(2) +
    // admin(7). gateway declares none (it asks).
    assert_eq!(checked, 11, "expected 11 edge peer declarations across the fleet");
}

/// The asymmetry the boot-order rule depends on: an `AddrKind::Http` peer is a
/// passthrough ORIGIN dialed per request, not a boot dependency, so it may boot
/// LATER than its consumer. The real fleet has no Http peer left (gateway asks),
/// so this proves the asymmetry against the rule itself on synthetic data.
#[test]
fn an_http_peer_carries_no_boot_order_constraint() {
    let (edge_violations, edge_checked) =
        edge_peer_order_violations(&consumer_before_provider(AddrKind::Edge));
    assert_eq!(edge_checked, 1, "the Edge declaration must be examined");
    assert!(
        edge_violations.iter().any(|v| v.contains("provider")),
        "an Edge peer booting AFTER its consumer must be a violation: {edge_violations:?}"
    );

    let (http_violations, http_checked) =
        edge_peer_order_violations(&consumer_before_provider(AddrKind::Http));
    assert!(
        http_violations.is_empty(),
        "an Http peer may boot later than its consumer: {http_violations:?}"
    );
    assert_eq!(http_checked, 0, "an Http peer must not be counted as a boot dependency at all");

    assert!(
        split()
            .iter()
            .all(|svc| svc.addrs.told().iter().all(|(_, _, kind)| *kind == AddrKind::Edge)),
        "an AddrKind::Http peer is back in the real fleet — re-point the assertions above at it"
    );
}

#[test]
fn scheduler_svc_has_no_scheduler_enabled() {
    // Deliberate parity with devctl's Development flavor: SCHEDULER_ENABLED is
    // only set under FleetFlavor::Proof in tools/processctl/src/fleet.rs.
    let svc = split().into_iter().find(|s| s.name == "scheduler-svc").unwrap();
    assert!(!svc.env.keys().any(|k| k == "SCHEDULER_ENABLED"));
}

// ---------------------------------------------------------------------------
// AGENT_PORT — the agent endpoint's slot in the one port authority
// ---------------------------------------------------------------------------

#[test]
fn agent_port_collides_with_no_fleet_port() {
    let mono = monolith();
    let mut taken: Vec<(String, &str, u16)> = Vec::new();
    for svc in split().iter().chain(std::iter::once(&mono)) {
        taken.push((svc.name.clone(), "http_port", svc.http_port));
        if let Some(port) = svc.edge_port {
            taken.push((svc.name.clone(), "edge_port", port));
        }
        if let Some(port) = svc.player_port {
            taken.push((svc.name.clone(), "player_port", port));
        }
    }
    // Fail-proof: an empty/near-empty list would make the loop below vacuous.
    assert!(taken.len() > 20, "expected the real fleet's ports here, got {taken:?}");
    for (name, field, port) in taken {
        assert_ne!(
            port, AGENT_PORT,
            "{name}'s {field} collides with AGENT_PORT ({AGENT_PORT})"
        );
    }
}
