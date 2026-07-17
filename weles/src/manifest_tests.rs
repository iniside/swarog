use super::*;
use std::collections::BTreeMap;
use std::ffi::OsString;

fn fake_inputs() -> RuntimeInputs {
    RuntimeInputs {
        database_url: "postgres://gamebackend:gamebackend@localhost:5432/gamebackend".to_string(),
        ca_cert: PathBuf::from("/fake/ca-cert.pem"),
        ca_key: PathBuf::from("/fake/ca-key.pem"),
    }
}

const FAKE_DB: &str = "postgres://gamebackend:gamebackend@localhost:5432/gamebackend";
const FAKE_CERT: &str = "/fake/ca-cert.pem";
const FAKE_KEY: &str = "/fake/ca-key.pem";

/// Removes allowlisted ambient-env keys from a composed env so a golden
/// assertion doesn't depend on the machine running the test (RUST_LOG, PATH,
/// etc. vary by shell).
///
/// COLLISION GUARD: if a future manifest key (PORT/EDGE_ADDR/env_extra/…)
/// ever collided with a [`SERVICE_ENV_ALLOWLIST`] name, this filter would
/// silently strip it from every golden assert and the goldens would go
/// blind to it. `no_manifest_key_collides_with_the_allowlist` below pins
/// that this cannot happen without a test failure.
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

/// ONE table-driven golden over the COMPLETE composed env (modulo the
/// allowlist strip) for ALL 12 split services + the monolith. Deliberately
/// verbose: every expected map is written out in full, so ANY drifted key or
/// value — added, removed, or changed — fails this test by name.
#[test]
fn full_fleet_env_goldens() {
    let inputs = fake_inputs();
    let goldens: &[(&str, &[(&str, &str)])] = &[
        (
            "accounts-svc",
            &[
                ("PORT", ":8084"),
                ("EDGE_ADDR", ":9003"),
                ("DATABASE_URL", FAKE_DB),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", FAKE_CERT),
                ("EDGE_CA_KEY", FAKE_KEY),
                ("ACCOUNTS_DEV_AUTH", "1"),
            ],
        ),
        (
            "apikeys-svc",
            &[
                ("PORT", ":8091"),
                ("EDGE_ADDR", ":9009"),
                ("DATABASE_URL", FAKE_DB),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", FAKE_CERT),
                ("EDGE_CA_KEY", FAKE_KEY),
                ("APIKEYS_DEV_SEED", "1"),
            ],
        ),
        (
            "audit-svc",
            &[
                ("PORT", ":8086"),
                ("EDGE_ADDR", ":9004"),
                ("DATABASE_URL", FAKE_DB),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", FAKE_CERT),
                ("EDGE_CA_KEY", FAKE_KEY),
            ],
        ),
        (
            // Deliberately NO SCHEDULER_ENABLED — Development-flavor parity.
            "scheduler-svc",
            &[
                ("PORT", ":8087"),
                ("EDGE_ADDR", ":9005"),
                ("DATABASE_URL", FAKE_DB),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", FAKE_CERT),
                ("EDGE_CA_KEY", FAKE_KEY),
            ],
        ),
        (
            "rating-svc",
            &[
                ("PORT", ":8089"),
                ("EDGE_ADDR", ":9007"),
                ("DATABASE_URL", FAKE_DB),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", FAKE_CERT),
                ("EDGE_CA_KEY", FAKE_KEY),
            ],
        ),
        (
            "leaderboard-svc",
            &[
                ("PORT", ":8090"),
                ("EDGE_ADDR", ":9008"),
                ("DATABASE_URL", FAKE_DB),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", FAKE_CERT),
                ("EDGE_CA_KEY", FAKE_KEY),
            ],
        ),
        (
            "match-svc",
            &[
                ("PORT", ":8088"),
                ("EDGE_ADDR", ":9006"),
                ("DATABASE_URL", FAKE_DB),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", FAKE_CERT),
                ("EDGE_CA_KEY", FAKE_KEY),
                ("RATING_EDGE_ADDR", "127.0.0.1:9007"),
            ],
        ),
        (
            "config-svc",
            &[
                ("PORT", ":8083"),
                ("EDGE_ADDR", ":9002"),
                ("DATABASE_URL", FAKE_DB),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", FAKE_CERT),
                ("EDGE_CA_KEY", FAKE_KEY),
            ],
        ),
        (
            "characters-svc",
            &[
                ("PORT", ":8080"),
                ("EDGE_ADDR", ":9000"),
                ("DATABASE_URL", FAKE_DB),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", FAKE_CERT),
                ("EDGE_CA_KEY", FAKE_KEY),
                ("CONFIG_EDGE_ADDR", "127.0.0.1:9002"),
            ],
        ),
        (
            "inventory-svc",
            &[
                ("PORT", ":8081"),
                ("EDGE_ADDR", ":9001"),
                ("DATABASE_URL", FAKE_DB),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", FAKE_CERT),
                ("EDGE_CA_KEY", FAKE_KEY),
                ("CHARACTERS_EDGE_ADDR", "127.0.0.1:9000"),
                ("CONFIG_EDGE_ADDR", "127.0.0.1:9002"),
                ("INVENTORY_DEV_GRANT", "1"),
            ],
        ),
        (
            // Pure-transport front door: no EDGE_ADDR of its own, no
            // DATABASE_URL/DATABASE_POOL_MAX_CONNECTIONS, but DOES get the
            // CA material (dials every peer's internal mTLS edge).
            "gateway-svc",
            &[
                ("PORT", ":8082"),
                ("EDGE_CA_CERT", FAKE_CERT),
                ("EDGE_CA_KEY", FAKE_KEY),
                ("PLAYER_EDGE_ADDR", ":9100"),
                ("TLS_MODE", "off"),
                ("CHARACTERS_EDGE_ADDR", "127.0.0.1:9000"),
                ("INVENTORY_EDGE_ADDR", "127.0.0.1:9001"),
                ("ACCOUNTS_EDGE_ADDR", "127.0.0.1:9003"),
                ("MATCH_EDGE_ADDR", "127.0.0.1:9006"),
                ("LEADERBOARD_EDGE_ADDR", "127.0.0.1:9008"),
                ("APIKEYS_EDGE_ADDR", "127.0.0.1:9009"),
                ("ADMIN_HTTP_ADDR", "127.0.0.1:8085"),
                ("ACCOUNTS_HTTP_ADDR", "127.0.0.1:8084"),
            ],
        ),
        (
            "admin-svc",
            &[
                ("PORT", ":8085"),
                ("DATABASE_URL", FAKE_DB),
                ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
                ("EDGE_CA_CERT", FAKE_CERT),
                ("EDGE_CA_KEY", FAKE_KEY),
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

    let fleet = split_fleet();
    assert_eq!(
        fleet.len(),
        goldens.len(),
        "every split service must have a golden (add the new one here)"
    );
    for (name, pairs) in goldens {
        let svc = fleet
            .iter()
            .find(|svc| svc.name == *name)
            .unwrap_or_else(|| panic!("{name} missing from split_fleet"));
        let env = strip_allowlist(&compose_env(svc, &inputs));
        assert_eq!(env, expected(pairs), "composed env drifted for {name}");
    }

    // Monolith golden.
    let env = strip_allowlist(&compose_env(&monolith(), &inputs));
    let want = expected(&[
        ("PORT", ":8080"),
        ("DATABASE_URL", FAKE_DB),
        ("DATABASE_POOL_MAX_CONNECTIONS", "20"),
        ("EDGE_CA_CERT", FAKE_CERT),
        ("EDGE_CA_KEY", FAKE_KEY),
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

/// Guard for `strip_allowlist`'s blind spot: a manifest-composed key that
/// collided with an allowlist name would be silently stripped from every
/// golden assert. Pin that no key the manifest introduces (synthesized in
/// `compose_env` or listed in `env_extra`) is an allowlist name.
#[test]
fn no_manifest_key_collides_with_the_allowlist() {
    let mut services = split_fleet();
    services.push(monolith());
    let synthesized = [
        "PORT",
        "EDGE_ADDR",
        "DATABASE_URL",
        "DATABASE_POOL_MAX_CONNECTIONS",
        "EDGE_CA_CERT",
        "EDGE_CA_KEY",
        ORCHESTRATOR_URL_ENV,
    ];
    for svc in &services {
        for key in synthesized
            .iter()
            .copied()
            .chain(svc.addrs.told().iter().map(|(k, _, _)| *k))
            .chain(svc.env_extra.iter().map(|(k, _)| *k))
        {
            assert!(
                !SERVICE_ENV_ALLOWLIST.iter().any(|a| a.eq_ignore_ascii_case(key)),
                "{}: manifest key {key} collides with SERVICE_ENV_ALLOWLIST — \
                 strip_allowlist would hide it from the goldens",
                svc.name
            );
        }
    }
}

/// A two-service synthetic fleet: `provider-svc` (both ports) and
/// `consumer-svc`, which is handed the provider's address by both kinds. Only
/// the ports vary, so a test can move a port and watch the consumer's env.
fn synthetic_peer_fleet(provider_edge: Option<u16>, provider_http: u16) -> Vec<ServiceDef> {
    vec![
        ServiceDef {
            name: "provider-svc",
            pkg: "provider-svc",
            provider: Some("provider"),
            http_port: provider_http,
            edge_port: provider_edge,
            player_port: None,
            has_db: false,
            pool_max: 0,
            addrs: Addrs::Told(&[]),
            env_extra: &[],
        },
        ServiceDef {
            name: "consumer-svc",
            pkg: "consumer-svc",
            provider: Some("consumer"),
            http_port: 1,
            edge_port: None,
            player_port: None,
            has_db: false,
            pool_max: 0,
            addrs: Addrs::Told(&[
                ("PROVIDER_EDGE_ADDR", "provider", AddrKind::Edge),
                ("PROVIDER_HTTP_ADDR", "provider", AddrKind::Http),
            ]),
            env_extra: &[],
        },
    ]
}

fn composed_value(fleet: &[ServiceDef], svc_name: &str, key: &str) -> String {
    let svc = fleet.iter().find(|svc| svc.name == svc_name).unwrap();
    compose_env_with_fleet(svc, &fake_inputs(), fleet)
        .get(&OsString::from(key))
        .unwrap_or_else(|| panic!("{svc_name} composed env has no {key}"))
        .to_string_lossy()
        .into_owned()
}

/// THE previously-broken branch. Before `peers`, a consumer's peer address
/// was a hand-written literal in `env_extra`: moving the provider's port left
/// the consumer pointing at the old one, silently. Prove the port field is now
/// the authority — move it, and the consumer's COMPOSED env moves with it.
///
/// Driven against a synthetic fleet because the real fleet is `'static` and a
/// port therefore cannot be moved in-process (same reason `fleet_pg_budget`
/// takes a slice). Reintroducing a literal for `PROVIDER_EDGE_ADDR` freezes
/// the value and fails this test.
#[test]
fn moving_a_providers_port_propagates_to_its_consumers_env() {
    let before = synthetic_peer_fleet(Some(9000), 8080);
    assert_eq!(composed_value(&before, "consumer-svc", "PROVIDER_EDGE_ADDR"), "127.0.0.1:9000");
    assert_eq!(composed_value(&before, "consumer-svc", "PROVIDER_HTTP_ADDR"), "127.0.0.1:8080");

    // Move BOTH of the provider's ports; change nothing about the consumer.
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

/// The kinds are not interchangeable: the same provider, dialed both ways,
/// must yield its two DIFFERENT ports (real case: `accounts`, edge 9003 +
/// http 8084). A derivation that read one field for both would pass the
/// propagation test above and fail here.
#[test]
fn the_two_kinds_read_different_port_fields() {
    let fleet = synthetic_peer_fleet(Some(9000), 8080);
    assert_ne!(
        composed_value(&fleet, "consumer-svc", "PROVIDER_EDGE_ADDR"),
        composed_value(&fleet, "consumer-svc", "PROVIDER_HTTP_ADDR"),
    );

    // ...and the real fleet's dual-kind provider proves it on live data.
    let real = split_fleet();
    assert_eq!(composed_value(&real, "gateway-svc", "ACCOUNTS_EDGE_ADDR"), "127.0.0.1:9003");
    assert_eq!(composed_value(&real, "gateway-svc", "ACCOUNTS_HTTP_ADDR"), "127.0.0.1:8084");
}

/// `AddrKind::Edge` against a service with `edge_port: None` (real case:
/// `admin`, an HTTP passthrough origin that serves no internal edge) is a
/// programmer error while adding a service — it must fail LOUDLY and name the
/// offender, never silently synthesize an address nobody listens on.
///
/// The `expected` substring is unique to the TARGET panic (it quotes the
/// offender inside the edge-specific sentence), so an unrelated panic that
/// merely mentions the name cannot green this test.
#[test]
#[should_panic(expected = "peer \"provider\" as AddrKind::Edge")]
fn edge_kind_against_a_service_without_an_edge_panics() {
    let fleet = synthetic_peer_fleet(None, 8080);
    composed_value(&fleet, "consumer-svc", "PROVIDER_EDGE_ADDR");
}

/// The same, phrased against the REAL def that has `edge_port: None`: nothing
/// about `admin` lets it be dialed as an edge peer.
///
/// The fixture guard below deliberately does NOT name admin: a `should_panic`
/// matching only the bare name would be satisfied by the guard's OWN panic if
/// admin-svc ever gained an edge_port, greening a test that proved nothing.
#[test]
#[should_panic(expected = "peer \"admin\" as AddrKind::Edge")]
fn edge_kind_against_real_admin_svc_panics() {
    let fleet = split_fleet();
    let admin = fleet.iter().find(|svc| svc.provider == Some("admin")).unwrap();
    assert!(admin.edge_port.is_none(), "fixture assumption broken: it now serves an edge");
    peer_addr(&fleet, "some-consumer-svc", "admin", AddrKind::Edge);
}

/// An unknown provider is the other half of the same programmer error.
#[test]
#[should_panic(expected = "peer \"ghost\", which no service in this fleet provides")]
fn an_unknown_provider_panics() {
    peer_addr(&split_fleet(), "gateway-svc", "ghost", AddrKind::Edge);
}

// ---------------------------------------------------------------------------
// PeerAddrs — what the agent's `resolve` answers from
// ---------------------------------------------------------------------------

/// TWO instances of one provider must render as two DISTINCT addresses.
///
/// This is the whole point of the list shape, and the branch that a name
/// round-trip breaks: `find(|svc| svc.provider == Some(provider))` takes the
/// FIRST match, so a map that re-looked-up the provider it already held would
/// format both entries from the first def's port — `["127.0.0.1:9000",
/// "127.0.0.1:9000"]`, a list that looks like two healthy instances and is one
/// address twice, sending half an LB's traffic at a port nobody is on.
///
/// Unreachable in the real fleet today (`every_split_service_has_a_unique_short_
/// provider_name`) — but that guard is precisely what M2's replicas must
/// delete, and this test is what stays behind when it goes. Synthetic by
/// necessity: the branch cannot be expressed by real data that a sibling test
/// forbids.
#[test]
fn two_instances_of_one_provider_resolve_to_two_distinct_addresses() {
    let instance = |http_port, edge_port| ServiceDef {
        name: "characters-svc",
        pkg: "characters-svc",
        provider: Some("characters"),
        http_port,
        edge_port: Some(edge_port),
        player_port: None,
        has_db: true,
        pool_max: 3,
        addrs: Addrs::Told(&[]),
        env_extra: &[],
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
/// Edge entry at all, so the lookup finds nothing rather than falling back to
/// the HTTP port.
#[test]
fn peer_addrs_omits_a_kind_a_service_does_not_serve() {
    let map = PeerAddrs::from_fleet(&split_fleet());
    assert!(
        map.lookup("admin", AddrKind::Edge).is_empty(),
        "admin serves no edge — there is no address to give out"
    );
    assert_eq!(map.lookup("admin", AddrKind::Http), vec!["127.0.0.1:8085".to_string()]);
    // The monolith is unresolvable as DATA (provider: None), not by a branch.
    let mono = PeerAddrs::from_fleet(&[monolith()]);
    for kind in [AddrKind::Edge, AddrKind::Http] {
        assert!(
            mono.lookup("characters", kind).is_empty() && mono.lookup("server", kind).is_empty(),
            "the monolith hosts every domain in-process — it is nameable as no provider"
        );
    }
}

/// `env_extra` is applied AFTER the derived peer addresses, so an `env_extra`
/// key that repeats a `peers` key silently overrides the derivation and
/// restores the two-authorities drift — invisibly, because the composed env
/// still contains a plausible address.
///
/// The check is KEY-shaped (`peers` keys ∩ `env_extra` keys = ∅), not a scan
/// for address-looking values: a value scan has holes (`localhost:9000`,
/// `[::1]:9000`, `10.0.0.5:9000`, any hostname) that this cannot have.
///
/// Together with `full_fleet_env_goldens` the two guards leave no gap: the
/// goldens fail on any env_extra addition that CHANGES a composed value, and
/// this fails on one that shadows a derived key without changing it.
#[test]
fn no_env_extra_key_shadows_a_derived_peer_key() {
    let mut services = split_fleet();
    services.push(monolith());
    for svc in &services {
        for (key, value) in svc.env_extra {
            assert!(
                !svc.addrs.told().iter().any(|(peer_key, _, _)| peer_key == key),
                "{}: env_extra {key} = {value:?} shadows the SAME key derived from \
                 `peers` — env_extra is applied last, so the derived address would be \
                 silently discarded. Delete the literal; `peers` is the authority.",
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
///
/// The URL is asserted against `AGENT_PORT` rather than the literal `8300`, so
/// moving the port moves this expectation with it — a hardcoded URL here would
/// restore exactly the two-authorities drift the `peers` seam was built to kill.
#[test]
fn a_managed_service_is_handed_the_agent_url_and_no_peer_addresses() {
    let fleet = split_fleet();
    let gateway = fleet.iter().find(|svc| svc.name == "gateway-svc").unwrap();
    assert!(gateway.addrs == Addrs::Asks, "fixture assumption: gateway-svc is the one that asks");
    let env = strip_allowlist(&compose_env(gateway, &fake_inputs()));

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
             so the env copy is a second authority nobody reads — and an unread value drifts \
             silently until someone believes it"
        );
    }
}

/// An unmanaged process is handed no URL: the two modes are disjoint, and every
/// other service is still told by env exactly as before.
#[test]
fn an_unmanaged_service_is_handed_no_agent_url() {
    let fleet = split_fleet();
    let inventory = fleet.iter().find(|svc| svc.name == "inventory-svc").unwrap();
    let env = compose_env(inventory, &fake_inputs());

    assert_ne!(inventory.addrs, Addrs::Asks);
    assert!(!env.contains_key(&OsString::from("ORCHESTRATOR_URL")));
    assert_eq!(
        env.get(&OsString::from("CHARACTERS_EDGE_ADDR")),
        Some(&OsString::from("127.0.0.1:9000")),
    );
    // The monolith has no peers to resolve and no map to be answered from
    // (`provider: None` ⇒ empty `PeerAddrs` ⇒ every resolve 404s), so it must
    // never be managed.
    assert_ne!(monolith().addrs, Addrs::Asks);
}

/// The monolith is nameable as no single domain (it hosts all of them), which
/// is what makes it structurally unresolvable as a peer — the data fact the
/// future topology-aware `resolve` map rests on.
#[test]
fn the_monolith_provides_no_short_name_and_dials_no_peers() {
    let mono = monolith();
    assert_eq!(mono.provider, None);
    assert!(mono.addrs.told().is_empty());
}

/// Every split service IS nameable, and uniquely — `peers` and `resolve` key
/// on this, so a duplicate or missing short name would make a lookup ambiguous
/// or impossible.
#[test]
fn every_split_service_has_a_unique_short_provider_name() {
    let fleet = split_fleet();
    let mut seen = std::collections::BTreeSet::new();
    for svc in &fleet {
        let provider = svc.provider.unwrap_or_else(|| panic!("{}: no provider name", svc.name));
        assert!(seen.insert(provider), "duplicate provider short name {provider:?}");
        // The short name is the module/api directory name the wire already
        // uses (`Stub::new("characters", …)`) — pinned against `name` here so
        // the two can't drift into two naming authorities.
        assert_eq!(
            svc.name,
            format!("{provider}-svc"),
            "{}: provider short name must be the process's own domain",
            svc.name
        );
    }
}

/// `compose_env` resolves a def against the manifest that def belongs to, NOT
/// against `split_fleet()` by assumption. A monolith-shaped def that declared
/// a split-only peer must therefore FAIL rather than silently hand out a split
/// address for a process the monolith topology never starts.
#[test]
#[should_panic(expected = "no service in this fleet provides")]
fn a_monolith_def_may_not_silently_resolve_a_split_only_provider() {
    let mono = ServiceDef {
        addrs: Addrs::Told(&[("CHARACTERS_EDGE_ADDR", "characters", AddrKind::Edge)]),
        ..monolith()
    };
    compose_env(&mono, &fake_inputs());
}

/// A def from neither real manifest has no discoverable home fleet; the public
/// convenience must say so rather than guess one.
#[test]
#[should_panic(expected = "belongs to neither")]
fn compose_env_refuses_a_def_from_no_real_manifest() {
    compose_env(&synthetic_db_svc("stranger-svc", 1), &fake_inputs());
}

/// Boot order is DERIVED from the `peers` field, never hand-listed beside it:
/// a copied list would drift from the declaration it copies — the exact defect
/// class `peers` exists to kill.
///
/// Only `AddrKind::Edge` peers constrain boot order (see
/// `an_http_peer_carries_no_boot_order_constraint` for the other half — a
/// sweep that forgot this filter would fail on gateway→admin).
#[test]
fn boot_order_respects_edge_peer_dependencies() {
    let fleet = split_fleet();
    let position = |provider: &str| {
        fleet
            .iter()
            .position(|svc| svc.provider == Some(provider))
            .unwrap_or_else(|| panic!("no service provides {provider:?}"))
    };

    let mut checked = 0;
    for (index, svc) in fleet.iter().enumerate() {
        for (key, provider, kind) in svc.addrs.told() {
            if *kind != AddrKind::Edge {
                continue;
            }
            assert!(
                position(provider) < index,
                "{}: edge peer {provider:?} ({key}) must appear earlier in split_fleet() — \
                 the Vec order IS the boot order",
                svc.name
            );
            checked += 1;
        }
    }
    // The loop must actually have run: a `peers` field emptied by a bad
    // refactor would make every assertion above vacuous.
    assert_eq!(checked, 17, "expected 17 edge peer declarations across the fleet");
}

/// The asymmetry the boot-order rule depends on: an `AddrKind::Http` peer is a
/// passthrough ORIGIN dialed per request, not a boot dependency, so it may
/// boot LATER than its consumer. gateway-svc names `admin` as
/// `ADMIN_HTTP_ADDR`, and admin-svc boots LAST — a universal
/// "peers boot earlier" sweep would fail on exactly this entry, which is why
/// `boot_order_respects_edge_peer_dependencies` filters on the kind.
#[test]
fn an_http_peer_carries_no_boot_order_constraint() {
    let fleet = split_fleet();
    let index = |name: &str| fleet.iter().position(|svc| svc.name == name).unwrap();

    assert_eq!(fleet.last().unwrap().name, "admin-svc", "admin-svc boots last");
    assert!(
        index("admin-svc") > index("gateway-svc"),
        "fixture: admin must boot AFTER the gateway that names it"
    );

    let gateway = fleet.iter().find(|svc| svc.name == "gateway-svc").unwrap();
    let (_, _, kind) = gateway
        .addrs
        .told()
        .iter()
        .find(|(key, _, _)| *key == "ADMIN_HTTP_ADDR")
        .expect("gateway must declare ADMIN_HTTP_ADDR as a peer");
    assert_eq!(*kind, AddrKind::Http, "a LATER-booting peer is only legal as an Http origin");
}

#[test]
fn scheduler_svc_has_no_scheduler_enabled() {
    // Deliberate parity with devctl's Development flavor: SCHEDULER_ENABLED
    // is only set under FleetFlavor::Proof in tools/processctl/src/fleet.rs.
    let svc = split_fleet().into_iter().find(|s| s.name == "scheduler-svc").unwrap();
    assert!(!svc.env_extra.iter().any(|(k, _)| *k == "SCHEDULER_ENABLED"));
}

#[test]
fn validate_disk_green_on_real_repo() {
    // Run from the weles crate dir: CARGO_MANIFEST_DIR/../cmd.
    let cmd_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("cmd");
    validate_disk(&cmd_dir).expect("real repo cmd/ must match the canonical split fleet");
}

#[test]
fn validate_disk_red_reports_both_directions() {
    let dir = std::env::temp_dir().join(format!(
        "weles-manifest-test-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // Missing every real -svc dir; add one that doesn't belong.
    std::fs::create_dir_all(dir.join("bogus-svc")).unwrap();

    let err = validate_disk(&dir).expect_err("mismatched disk layout must fail");
    let message = err.to_string();

    // missing-in-manifest direction: the fake dir must be called out.
    assert!(message.contains("bogus-svc"), "{message}");
    // missing-on-disk direction: at least one real canonical service must be
    // reported absent.
    assert!(message.contains("accounts-svc"), "{message}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn validate_pg_budget_green_for_real_fleet() {
    validate_pg_budget().expect("the real fleet must fit PG_SESSION_BUDGET");
}

fn synthetic_db_svc(name: &'static str, pool_max: u32) -> ServiceDef {
    ServiceDef {
        name,
        pkg: "synthetic-svc",
        provider: Some("synthetic"),
        http_port: 1,
        edge_port: None,
        player_port: None,
        has_db: true,
        pool_max,
        addrs: Addrs::Told(&[]),
        env_extra: &[],
    }
}

#[test]
fn service_pg_budget_charges_plane_dedicated_for_db_services() {
    let (pool, dedicated) = service_pg_budget(&synthetic_db_svc("some-svc", 3));
    assert_eq!(pool, 3);
    assert_eq!(dedicated, PLANE_DEDICATED_SESSIONS);
}

#[test]
fn service_pg_budget_scheduler_charges_one_more_dedicated() {
    let (_, dedicated) = service_pg_budget(&synthetic_db_svc("scheduler-svc", 3));
    assert_eq!(dedicated, PLANE_DEDICATED_SESSIONS + SCHEDULER_FIRE_SESSIONS);
}

#[test]
fn service_pg_budget_monolith_charges_scheduler_fire_session() {
    // The monolith hosts the scheduler module too — it must carry the fire
    // session on top of the plane dedicateds.
    let (pool, dedicated) = service_pg_budget(&monolith());
    assert_eq!(pool, 20);
    assert_eq!(dedicated, PLANE_DEDICATED_SESSIONS + SCHEDULER_FIRE_SESSIONS);
}

#[test]
fn service_pg_budget_charges_nothing_for_dbless_service() {
    let svc = ServiceDef {
        name: "gateway-svc",
        pkg: "gateway-svc",
        provider: Some("gateway"),
        http_port: 1,
        edge_port: None,
        player_port: None,
        has_db: false,
        pool_max: 0,
        addrs: Addrs::Told(&[]),
        env_extra: &[],
    };
    assert_eq!(service_pg_budget(&svc), (0, 0));
}

/// Executes the validator's FAILING branch on the split path and proves the
/// dedicated term is load-bearing: a synthetic fleet whose POOL-ONLY sum
/// fits the budget but whose pool+dedicated sum does not must be rejected
/// by `fleet_pg_budget` with the exact over-budget total. A regression to
/// pool-only summation makes this fleet pass and fails this test.
#[test]
fn fleet_pg_budget_rejects_a_fleet_that_passes_pool_only() {
    // 25 synthetic DB-backed services, each pool_max = 3 (matches the real
    // SPLIT_SERVICE_POOL_MAX). Pool-only sum = 75, under the 87 budget. But
    // every DB service also charges PLANE_DEDICATED_SESSIONS(4), so the true
    // reservation is 25 * (3 + 4) = 175 — over budget.
    let synthetic: Vec<ServiceDef> = (0..25)
        .map(|i| {
            synthetic_db_svc(
                Box::leak(format!("synthetic-{i}-svc").into_boxed_str()),
                SPLIT_SERVICE_POOL_MAX,
            )
        })
        .collect();

    let pool_only_sum: u32 = synthetic.iter().map(|svc| svc.pool_max).sum();
    assert!(
        pool_only_sum <= PG_SESSION_BUDGET,
        "test fixture must be pool-only-green to prove the dedicated term matters"
    );

    let err = fleet_pg_budget(&synthetic)
        .expect_err("pool+dedicated over budget must fail even when pool-only fits");
    match &err {
        ManifestError::PoolBudgetExceeded { total, budget, breakdown } => {
            assert_eq!(*total, 175);
            assert_eq!(*budget, PG_SESSION_BUDGET);
            assert!(breakdown.contains("synthetic-0-svc"), "{breakdown}");
        }
        other => panic!("expected PoolBudgetExceeded, got {other}"),
    }
    // The message carries the numbers an operator needs.
    let message = err.to_string();
    assert!(message.contains("175"), "{message}");
    assert!(message.contains(&PG_SESSION_BUDGET.to_string()), "{message}");
}

/// Executes the validator's FAILING branch on the monolith path: an oversized
/// single-process pool must be rejected, with the scheduler fire session
/// charged on top of the plane dedicateds (pkg == "server").
#[test]
fn fleet_pg_budget_rejects_an_oversized_monolith() {
    let mut mono = monolith();
    mono.pool_max = 100;
    let err = fleet_pg_budget(std::slice::from_ref(&mono))
        .expect_err("an oversized monolith pool must fail the budget");
    match err {
        ManifestError::PoolBudgetExceeded { total, budget, .. } => {
            // 100 pool + 4 plane dedicated + 1 scheduler fire.
            assert_eq!(total, 105);
            assert_eq!(budget, PG_SESSION_BUDGET);
        }
        other => panic!("expected PoolBudgetExceeded, got {other}"),
    }
}

// ---------------------------------------------------------------------------
// AGENT_PORT — the agent endpoint's slot in the one port authority
// ---------------------------------------------------------------------------

#[test]
fn agent_port_collides_with_no_fleet_port() {
    // DERIVED from the manifest, never a hand-listed copy of the port bands: a
    // service added with http_port 8099 must fail HERE, at the one place ports
    // are decided, rather than as a bind conflict at boot. (`weles up` also
    // checks AGENT_PORT for a stale listener before binding, but that catches a
    // foreign process — not a manifest that collides with itself.)
    let mut taken: Vec<(&str, &str, u16)> = Vec::new();
    for svc in split_fleet().iter().chain(std::iter::once(&monolith())) {
        taken.push((svc.name, "http_port", svc.http_port));
        if let Some(port) = svc.edge_port {
            taken.push((svc.name, "edge_port", port));
        }
        if let Some(port) = svc.player_port {
            taken.push((svc.name, "player_port", port));
        }
    }
    // Fail-proof: an empty/near-empty list would make the loop below vacuous.
    assert!(
        taken.len() > 20,
        "expected the real fleet's ports here, got {taken:?}"
    );
    for (name, field, port) in taken {
        assert_ne!(
            port, AGENT_PORT,
            "{name}'s {field} collides with AGENT_PORT ({AGENT_PORT}) — the agent and that \
             service would race for the same loopback port at boot"
        );
    }
}
