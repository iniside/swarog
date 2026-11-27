use std::collections::{BTreeMap, BTreeSet};

use crate::{
    check_pg_session_floor, game_backend_fleet, game_backend_fleet_with_environment,
    game_backend_monolith, EnvironmentSnapshot, FleetError, FleetFlavor, FleetInputs, FleetSpec,
    PgSessionCapacity, PoolBudget, ServiceSpec, HARNESS_RESERVE, PG_SESSION_BUDGET,
    PROOF_ACCOUNTS_ISSUER_MONOLITH, PROOF_ACCOUNTS_ISSUER_SPLIT, PROOF_GATEWAY_RATE_LIMIT_BURST,
    PROOF_GATEWAY_RATE_LIMIT_RPS, PROOF_GOOGLE_CLIENT_ID, PROOF_OIDC_CLIENT_ID, PROOF_OIDC_ISSUER,
    PROOF_OIDC_JWKS_URL, REQUIRED_MAX_CONNECTIONS,
};
use crate::fleet::{
    ACCOUNTS_OVERRIDEABLE_ENV, DEV_ACCOUNTS_ISSUER_SPLIT, DEV_ACCOUNTS_SIGNING_KEK,
    EPIC_ENV_PREFIX, GOOGLE_ENV_PREFIX, MONOLITH_OVERRIDEABLE_ENV,
};
use accountsapi::{DEFAULT_OIDC_ISSUER, EPIC_ENV_KEYS, GOOGLE_ENV_KEYS};
#[cfg(windows)]
use crate::build_environment;

fn inputs() -> FleetInputs {
    FleetInputs {
        database_url: "postgres://proof".into(),
        edge_ca_cert: "run/edge-ca.crt".into(),
        edge_ca_key: "run/edge-ca.key".into(),
    }
}

#[test]
fn apple_toolchain_selection_is_preserved_only_in_build_environment() {
    let selected = [
        ("DEVELOPER_DIR", "/operator-selected/Xcode Preview.app/Contents/Developer"),
        ("SDKROOT", "/operator-selected/SDKs/MacOSX.sdk"),
    ];
    let environment = EnvironmentSnapshot::from_values(
        selected.map(|(key, value)| (key.into(), value.into())),
    );
    let build = environment.build_environment();
    let runtime = environment.runtime_environment();

    for (key, value) in selected {
        assert_eq!(build.get(key).map(String::as_str), Some(value), "{key}");
        assert!(!runtime.contains_key(key), "{key} is build-only");
    }
}

#[test]
fn program_files_root_is_preserved_only_in_build_environment() {
    let environment = EnvironmentSnapshot::from_values([(
        "ProgramFiles".to_string(),
        "C:\\Program Files".to_string(),
    )]);
    let build = environment.build_environment();
    let runtime = environment.runtime_environment();

    assert_eq!(build.get("ProgramFiles").map(String::as_str), Some("C:\\Program Files"));
    assert!(!runtime.contains_key("ProgramFiles"), "ProgramFiles is build-only");
}

#[test]
fn apple_toolchain_selection_is_not_discovered_when_absent() {
    let environment = EnvironmentSnapshot::from_values([]);
    let build = environment.build_environment();
    let runtime = environment.runtime_environment();

    for key in ["DEVELOPER_DIR", "SDKROOT"] {
        assert!(!build.contains_key(key), "{key} must come from the snapshot");
        assert!(!runtime.contains_key(key), "{key} is build-only");
    }
}

#[test]
fn inherited_application_overrides_are_explicit_and_cannot_rewire_topology() {
    let environment = EnvironmentSnapshot::from_values([
        ("ACCOUNTS_OPEN_REGISTRATION".into(), "0".into()),
        ("PORT".into(), ":9999".into()),
        ("EDGE_ADDR".into(), ":9998".into()),
        ("EDGE_CA_KEY".into(), "attacker-key".into()),
        ("HTTP_PROXY".into(), "http://proxy".into()),
    ]);
    let fleet = game_backend_fleet_with_environment(
        &inputs(), FleetFlavor::Development, &environment,
    );
    let accounts = fleet.service("accounts-svc").unwrap();
    assert_eq!(accounts.env.get("ACCOUNTS_OPEN_REGISTRATION").map(String::as_str), Some("0"));
    // accounts-svc serves the browser cookie on plain http and trusts the gateway-svc host, as admin-svc does.
    assert_eq!(accounts.env.get("ACCOUNTS_COOKIE_SECURE").map(String::as_str), Some("0"));
    assert_eq!(accounts.env.get("TRUSTED_PROXY_CIDRS").map(String::as_str), Some("127.0.0.1/32"));
    assert_eq!(accounts.env.get("PORT").map(String::as_str), Some("127.0.0.1:8084"));
    assert_eq!(accounts.env.get("EDGE_ADDR").map(String::as_str), Some("127.0.0.1:9003"));
    assert_eq!(accounts.env.get("EDGE_CA_KEY").map(String::as_str), Some("run/edge-ca.key"));
    assert!(!accounts.env.contains_key("HTTP_PROXY"));

    let monolith = game_backend_monolith(&inputs(), FleetFlavor::Development, &environment);
    assert_eq!((monolith.http_port, monolith.player_port), (8080, Some(9100)));
    assert_eq!(monolith.env.get("PORT").map(String::as_str), Some("127.0.0.1:8080"));
    assert_eq!(monolith.env.get("PLAYER_EDGE_ADDR").map(String::as_str), Some("127.0.0.1:9100"));
    assert_eq!(monolith.env.get("EDGE_CA_KEY").map(String::as_str), Some("run/edge-ca.key"));
}

/// The Proof fleet's accounts-svc trusts the gateway-svc host, so the split proof's two
/// `X-Forwarded-For` clients (splitproof [BR1]/[BR2]) are two sign-in buckets; without it every
/// request gateway-svc proxies to accounts-svc is one client. Built on an empty snapshot so no
/// ambient override stands in for the fleet's own value.
#[test]
fn proof_fleet_accounts_trusts_the_gateway_host() {
    let environment = EnvironmentSnapshot::from_values([]);
    let fleet = game_backend_fleet_with_environment(&inputs(), FleetFlavor::Proof, &environment);
    let accounts = fleet.service("accounts-svc").unwrap();
    assert_eq!(accounts.env.get("TRUSTED_PROXY_CIDRS").map(String::as_str), Some("127.0.0.1/32"));
    // The hosted pages' cookies ride plain http in the proof.
    assert_eq!(accounts.env.get("ACCOUNTS_COOKIE_SECURE").map(String::as_str), Some("0"));
    // [BR2] bursts the JSON Epic start, which accounts mounts only with the confidential client.
    assert!(accounts.env.contains_key("EPIC_CLIENT_SECRET"));
}

fn signing_env(env: &BTreeMap<String, String>) -> (Option<&str>, Option<&str>) {
    (
        env.get("ACCOUNTS_SIGNING_KEK").map(String::as_str),
        env.get("ACCOUNTS_OIDC_ISSUER").map(String::as_str),
    )
}

/// Development fleets seal with the dev KEK unless the operator's shell overrides it or the
/// issuer; the split one names its gateway as issuer, the monolith keeps accounts' default
/// (its own port). Proof fleets pin both over any ambient value, each topology its own issuer.
#[test]
fn accounts_signing_env_in_both_topologies() {
    const KEK: &str = "ambient-kek";
    const ISSUER: &str = "https://ambient.example/accounts/oidc";
    let bare = EnvironmentSnapshot::from_values([]);
    let ambient = EnvironmentSnapshot::from_values([
        ("ACCOUNTS_SIGNING_KEK".into(), KEK.into()),
        ("ACCOUNTS_OIDC_ISSUER".into(), ISSUER.into()),
    ]);
    let dev = Some(DEV_ACCOUNTS_SIGNING_KEK);
    let dev_split = Some(DEV_ACCOUNTS_ISSUER_SPLIT);
    let split = Some(PROOF_ACCOUNTS_ISSUER_SPLIT);
    let mono = Some(PROOF_ACCOUNTS_ISSUER_MONOLITH);
    let cases = [
        (&bare, FleetFlavor::Development, (dev, dev_split), (dev, Some(DEFAULT_OIDC_ISSUER))),
        (&ambient, FleetFlavor::Development, (Some(KEK), Some(ISSUER)), (Some(KEK), Some(ISSUER))),
        (&bare, FleetFlavor::Proof, (dev, split), (dev, mono)),
        (&ambient, FleetFlavor::Proof, (dev, split), (dev, mono)),
    ];
    for (environment, flavor, want_split, want_monolith) in cases {
        let fleet = game_backend_fleet_with_environment(&inputs(), flavor, environment);
        // The split's front door is gateway-svc, whatever port it binds.
        let front = format!(":{}/accounts/oidc", fleet.service("gateway-svc").unwrap().http_port);
        let issuer = want_split.1.unwrap();
        assert!(issuer == ISSUER || issuer.ends_with(&front), "{issuer} is not the gateway");
        let accounts = fleet.service("accounts-svc").unwrap();
        assert_eq!(signing_env(&accounts.env), want_split, "split {flavor:?}");
        let monolith = game_backend_monolith(&inputs(), flavor, environment);
        let (kek, issuer) = signing_env(&monolith.env);
        let effective = (kek, Some(issuer.unwrap_or(DEFAULT_OIDC_ISSUER)));
        assert_eq!(effective, want_monolith, "monolith {flavor:?}");
        // The monolith's front door is the port the server binds.
        let front = format!(":{}/accounts/oidc", monolith.http_port);
        let issuer = want_monolith.1.unwrap();
        assert!(issuer == ISSUER || issuer.ends_with(&front), "{issuer} is not the monolith");
    }
}

#[cfg(windows)]
#[test]
fn inherited_windows_baseline_lookup_is_case_insensitive() {
    let environment = EnvironmentSnapshot::from_values([
        ("Path".into(), "typed-path".into()),
        ("SystemRoot".into(), "typed-root".into()),
    ]);
    let runtime = environment.runtime_environment();
    assert_eq!(runtime.get("PATH").map(String::as_str), Some("typed-path"));
    assert_eq!(runtime.get("SYSTEMROOT").map(String::as_str), Some("typed-root"));
}

/// One service as this snapshot pins it: name, package, ports, and hard dependencies.
type ServiceRow = (&'static str, &'static str, u16, Option<u16>, Option<u16>, Vec<&'static str>);

/// The canonical fleet, hand-written: deriving it from `FleetSpec` would make the test
/// tautological. The per-service diff below is what a whole-vector `assert_eq!` cannot
/// give — a mismatch that names the service that moved.
#[test]
fn proof_fleet_matches_the_canonical_service_snapshot() {
    let fleet = game_backend_fleet(&inputs(), FleetFlavor::Proof);
    let actual: Vec<ServiceRow> = fleet
        .services()
        .iter()
        .map(|service| {
            (
                service.name,
                service.executable_package,
                service.http_port,
                service.edge_port,
                service.player_port,
                service.dependencies.clone(),
            )
        })
        .collect();
    let expected: Vec<ServiceRow> = vec![
        ("accounts-svc", "accounts-svc", 8084, Some(9003), None, vec![]),
        ("apikeys-svc", "apikeys-svc", 8091, Some(9009), None, vec![]),
        ("audit-svc", "audit-svc", 8086, Some(9004), None, vec![]),
        ("scheduler-svc", "scheduler-svc", 8087, Some(9005), None, vec![]),
        ("rating-svc", "rating-svc", 8089, Some(9007), None, vec![]),
        ("leaderboard-svc", "leaderboard-svc", 8090, Some(9008), None, vec![]),
        ("match-svc", "match-svc", 8088, Some(9006), None, vec!["rating-svc"]),
        ("config-svc", "config-svc", 8083, Some(9002), None, vec![]),
        ("characters-svc", "characters-svc", 8080, Some(9000), None, vec!["config-svc"]),
        ("inventory-svc", "inventory-svc", 8081, Some(9001), None, vec!["characters-svc", "config-svc"]),
        ("wallet-svc", "wallet-svc", 8092, Some(9010), None, vec!["config-svc", "accounts-svc"]),
        ("notifications-svc", "notifications-svc", 8093, Some(9011), None, vec![]),
        ("mail-svc", "mail-svc", 8094, Some(9012), None, vec![]),
        ("friends-svc", "friends-svc", 8095, Some(9014), None, vec!["accounts-svc"]),
        ("groups-svc", "groups-svc", 8096, Some(9015), None, vec!["accounts-svc"]),
        ("storage-svc", "storage-svc", 8097, Some(9016), None, vec![]),
        ("party-svc", "party-svc", 8098, Some(9017), None, vec!["accounts-svc"]),
        ("gateway-svc", "gateway-svc", 8082, Some(9013), Some(9100), vec!["characters-svc", "inventory-svc", "accounts-svc", "match-svc", "leaderboard-svc", "apikeys-svc", "wallet-svc", "notifications-svc", "friends-svc", "groups-svc", "storage-svc", "party-svc"]),
        ("admin-svc", "admin-svc", 8085, None, None, vec!["characters-svc", "inventory-svc", "config-svc", "accounts-svc", "audit-svc", "scheduler-svc", "apikeys-svc", "wallet-svc", "notifications-svc", "mail-svc", "friends-svc", "groups-svc", "storage-svc"]),
    ];

    // Per-service diff, the shape `FleetSpec::validate_names` reports drift in: a reader
    // gets the service that moved, not two 16-element vectors to align by eye.
    let mut drift: Vec<String> = Vec::new();
    for want in &expected {
        match actual.iter().find(|got| got.0 == want.0) {
            None => drift.push(format!(
                "{}: in this snapshot, ABSENT from the fleet -- removed from \
                 game_backend_fleet without updating the snapshot?",
                want.0
            )),
            Some(got) if got != want => {
                drift.push(format!("{}: snapshot says {want:?}, fleet says {got:?}", want.0))
            }
            Some(_) => {}
        }
    }
    for got in &actual {
        if !expected.iter().any(|want| want.0 == got.0) {
            drift.push(format!(
                "{}: in the fleet, ABSENT from this snapshot -- a new service must be \
                 written into this list (and into every other hand-maintained one)",
                got.0
            ));
        }
    }
    if drift.is_empty() {
        // Order is part of the claim: it is the start order, and `FleetSpec::new` rejects
        // a dependency that is not earlier.
        let got: Vec<&str> = actual.iter().map(|row| row.0).collect();
        let want: Vec<&str> = expected.iter().map(|row| row.0).collect();
        if got != want {
            drift.push(format!("start order drifted: snapshot {want:?}, fleet {got:?}"));
        }
    }
    assert!(
        drift.is_empty(),
        "the proof fleet drifted from the canonical snapshot:\n  {}",
        drift.join("\n  ")
    );
}

/// The split gateway's origin is one macro; this pins what it expands to by hand, and that
/// gateway-svc really listens on that port, so a wrong macro value or a gateway moved off it
/// goes red here rather than in a split proof's redirect.
#[test]
fn split_gateway_origin_is_the_gateway_port() {
    assert_eq!(crate::split_gateway_origin!(), "http://127.0.0.1:8082");
    assert_eq!(PROOF_ACCOUNTS_ISSUER_SPLIT, "http://127.0.0.1:8082/accounts/oidc");
    assert_eq!(DEV_ACCOUNTS_ISSUER_SPLIT, "http://localhost:8082/accounts/oidc");
    for flavor in [FleetFlavor::Development, FleetFlavor::Proof] {
        let fleet = game_backend_fleet(&inputs(), flavor);
        let gateway = fleet.service("gateway-svc").unwrap();
        let port: u16 = crate::split_gateway_port!().parse().unwrap();
        assert_eq!(gateway.http_port, port, "{flavor:?}: gateway-svc's http_port");
        assert_eq!(
            gateway.env.get("PORT").map(String::as_str),
            Some(format!("127.0.0.1:{}", crate::split_gateway_port!()).as_str()),
            "{flavor:?}: gateway-svc's PORT env"
        );
    }
}

#[test]
fn proof_overlay_is_explicit_and_name_lookup_is_stable() {
    let development = game_backend_fleet(&inputs(), FleetFlavor::Development);
    let proof = game_backend_fleet(&inputs(), FleetFlavor::Proof);
    assert!(!development.service("accounts-svc").unwrap().env.contains_key("EPIC_TOKEN_URL"));
    assert_eq!(
        proof
            .service("accounts-svc")
            .unwrap()
            .env
            .get("EPIC_REDIRECT_URI")
            .map(String::as_str),
        Some("http://127.0.0.1:8082/accounts/epic/callback")
    );
    assert_eq!(proof.service("scheduler-svc").unwrap().env.get("SCHEDULER_ENABLED").map(String::as_str), Some("1"));
    // `PUSH_PRESENCE` is default-off in `gateway::PushLimits` and stays off for the
    // Development fleet; only the Proof overlay turns it on, for `[PH5]`. Pinned both
    // ways so neither the knob nor the default can be quietly widened to make the
    // assertion pass.
    assert_eq!(
        proof.service("gateway-svc").unwrap().env.get("PUSH_PRESENCE").map(String::as_str),
        Some("1")
    );
    assert!(!development.service("gateway-svc").unwrap().env.contains_key("PUSH_PRESENCE"));
    // The front-door rate splitproof's sign-in bursts wait out is the one the Proof
    // gateway-svc actually runs with, not gateway-svc's default restated.
    let proof_gateway = &proof.service("gateway-svc").unwrap().env;
    assert_eq!(
        proof_gateway.get("RATE_LIMIT_RPS").map(String::as_str),
        Some(PROOF_GATEWAY_RATE_LIMIT_RPS.to_string().as_str())
    );
    assert_eq!(
        proof_gateway.get("RATE_LIMIT_BURST").map(String::as_str),
        Some(PROOF_GATEWAY_RATE_LIMIT_BURST.to_string().as_str())
    );
    assert!(matches!(proof.service("missing"), Err(FleetError::UnknownService(_))));
}

/// The Proof gateway-svc keeps the front-door rate splitproof reasons about even when the
/// operator's environment carries its own `RATE_LIMIT_*`.
#[test]
fn proof_gateway_rate_limit_survives_an_ambient_override() {
    let environment = EnvironmentSnapshot::from_values([
        ("RATE_LIMIT_RPS".into(), "1".into()),
        ("RATE_LIMIT_BURST".into(), "1".into()),
    ]);
    let fleet = game_backend_fleet_with_environment(&inputs(), FleetFlavor::Proof, &environment);
    let gateway = &fleet.service("gateway-svc").unwrap().env;
    assert_eq!(
        gateway.get("RATE_LIMIT_RPS").map(String::as_str),
        Some(PROOF_GATEWAY_RATE_LIMIT_RPS.to_string().as_str())
    );
    assert_eq!(
        gateway.get("RATE_LIMIT_BURST").map(String::as_str),
        Some(PROOF_GATEWAY_RATE_LIMIT_BURST.to_string().as_str())
    );
}

/// The accounts provider keys of both allowlists are exactly the keys accounts' provider
/// parse reads, in both topologies, and each sits under the prefix the `Proof` overlays
/// clear: a key accounts starts reading without both allowlists, a stale allowlist key, or
/// a key outside its provider's prefix fails here, by name.
#[test]
fn provider_allowlists_match_the_accounts_provider_keys() {
    let is_provider =
        |key: &&str| key.starts_with(EPIC_ENV_PREFIX) || key.starts_with(GOOGLE_ENV_PREFIX);
    let read: BTreeSet<&str> = EPIC_ENV_KEYS.iter().chain(GOOGLE_ENV_KEYS).copied().collect();
    for (name, allowlist) in [
        ("ACCOUNTS_OVERRIDEABLE_ENV", ACCOUNTS_OVERRIDEABLE_ENV),
        ("MONOLITH_OVERRIDEABLE_ENV", MONOLITH_OVERRIDEABLE_ENV),
    ] {
        let listed: BTreeSet<&str> = allowlist.iter().copied().filter(is_provider).collect();
        let missing: Vec<&&str> = read.difference(&listed).collect();
        let extra: Vec<&&str> = listed.difference(&read).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "{name}: missing {missing:?}, not read by accounts {extra:?}"
        );
    }
    for (prefix, keys) in [(EPIC_ENV_PREFIX, EPIC_ENV_KEYS), (GOOGLE_ENV_PREFIX, GOOGLE_ENV_KEYS)] {
        for key in keys {
            assert!(
                key.starts_with(prefix),
                "{key} is outside {prefix}, so no proof overlay clears it"
            );
        }
    }
}

fn ambient(keys: &[&str]) -> EnvironmentSnapshot {
    EnvironmentSnapshot::from_values(
        keys.iter().map(|key| ((*key).into(), format!("ambient-{key}"))),
    )
}

fn ambient_google() -> EnvironmentSnapshot {
    ambient(GOOGLE_ENV_KEYS)
}

#[test]
fn development_fleet_passes_every_google_key_to_accounts_in_both_topologies() {
    let environment = ambient_google();
    let fleet = game_backend_fleet_with_environment(&inputs(), FleetFlavor::Development, &environment);
    let accounts = &fleet.service("accounts-svc").unwrap().env;
    let monolith = game_backend_monolith(&inputs(), FleetFlavor::Development, &environment);
    for &key in GOOGLE_ENV_KEYS {
        let expected = format!("ambient-{key}");
        assert_eq!(accounts.get(key), Some(&expected), "accounts-svc {key}");
        assert_eq!(monolith.env.get(key), Some(&expected), "monolith {key}");
    }
}

/// The Proof monolith is where google stays known but unconfigured (`[A6m]` 503,
/// `[P8m]` `Unavailable`): no `GOOGLE_*` key survives, whatever the operator's shell holds.
#[test]
fn proof_monolith_drops_every_ambient_google_key() {
    let monolith = game_backend_monolith(&inputs(), FleetFlavor::Proof, &ambient_google());
    let google: Vec<&String> =
        monolith.env.keys().filter(|key| key.starts_with(GOOGLE_ENV_PREFIX)).collect();
    assert!(google.is_empty(), "monolith kept {google:?}");
}

/// The Proof accounts-svc holds exactly the google configuration splitproof's `[A6]`, `[P8]` and
/// `[BG*]` assert against, every ambient `GOOGLE_*` value replaced or dropped: an ambient
/// `GOOGLE_AUTHORIZE_URL` would otherwise survive the pins beside them.
#[test]
fn proof_split_pins_google_to_the_oidc_fixture_over_every_ambient_key() {
    let fleet = game_backend_fleet_with_environment(&inputs(), FleetFlavor::Proof, &ambient_google());
    let accounts = &fleet.service("accounts-svc").unwrap().env;
    let google: BTreeMap<&str, &str> = accounts
        .iter()
        .filter(|(key, _)| key.starts_with(GOOGLE_ENV_PREFIX))
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let pinned = BTreeMap::from([
        ("GOOGLE_CLIENT_IDS", PROOF_GOOGLE_CLIENT_ID),
        ("GOOGLE_OAUTH_CLIENT_ID", PROOF_GOOGLE_CLIENT_ID),
        ("GOOGLE_JWKS_URL", PROOF_OIDC_JWKS_URL),
        ("GOOGLE_CLIENT_SECRET", "test"),
        ("GOOGLE_REDIRECT_URI", "http://127.0.0.1:8082/accounts/oidc/google/callback"),
        ("GOOGLE_TOKEN_URL", "http://127.0.0.1:1/token"),
    ]);
    assert_eq!(google, pinned);
}

fn epic_env(env: &BTreeMap<String, String>) -> BTreeMap<&str, &str> {
    env.iter()
        .filter(|(key, _)| key.starts_with(EPIC_ENV_PREFIX))
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect()
}

/// The Proof monolith pins epic's three verifier keys to the OIDC fixture and nothing else:
/// an ambient epic browser-flow key (`EPIC_CLIENT_SECRET`, `EPIC_AUTHORIZE_URL`, ...) would
/// otherwise survive beside the pins.
#[test]
fn proof_monolith_pins_epic_to_the_oidc_fixture_over_every_ambient_key() {
    let monolith = game_backend_monolith(&inputs(), FleetFlavor::Proof, &ambient(EPIC_ENV_KEYS));
    let pinned = BTreeMap::from([
        ("EPIC_CLIENT_ID", PROOF_OIDC_CLIENT_ID),
        ("EPIC_JWKS_URL", PROOF_OIDC_JWKS_URL),
        ("EPIC_ISSUER_PREFIX", PROOF_OIDC_ISSUER),
    ]);
    assert_eq!(epic_env(&monolith.env), pinned);
}

/// The Proof accounts-svc holds exactly the epic configuration the split proof drives, every
/// ambient epic value replaced or dropped: an ambient `EPIC_AUTHORIZE_URL` would otherwise
/// survive the six pins beside it.
#[test]
fn proof_split_pins_epic_to_the_oidc_fixture_over_every_ambient_key() {
    let fleet =
        game_backend_fleet_with_environment(&inputs(), FleetFlavor::Proof, &ambient(EPIC_ENV_KEYS));
    let pinned = BTreeMap::from([
        ("EPIC_CLIENT_ID", PROOF_OIDC_CLIENT_ID),
        ("EPIC_JWKS_URL", PROOF_OIDC_JWKS_URL),
        ("EPIC_ISSUER_PREFIX", PROOF_OIDC_ISSUER),
        ("EPIC_CLIENT_SECRET", "test"),
        ("EPIC_REDIRECT_URI", "http://127.0.0.1:8082/accounts/epic/callback"),
        ("EPIC_TOKEN_URL", "http://127.0.0.1:1/token"),
    ]);
    assert_eq!(epic_env(&fleet.service("accounts-svc").unwrap().env), pinned);
}

#[test]
fn disk_drift_compares_names_not_order() {
    let fleet = game_backend_fleet(&inputs(), FleetFlavor::Proof);
    let reversed = fleet.services().iter().rev().map(|service| service.name.to_string());
    assert!(fleet.validate_names(reversed).is_ok());
    assert!(matches!(fleet.validate_names(["accounts-svc".to_string()]), Err(FleetError::DiskDrift { .. })));
}

#[test]
fn hard_dependencies_must_appear_earlier_in_startup_order() {
    let service = |name, dependencies| ServiceSpec {
        name,
        executable_package: name,
        http_port: 1,
        edge_port: None,
        player_port: None,
        dependencies,
        env: BTreeMap::new(),
        overrideable_env: &[],
        pool_budget: PoolBudget { pool_max: 0, dedicated: 0 },
    };
    let error = FleetSpec::new(vec![
        service("consumer-svc", vec!["provider-svc"]),
        service("provider-svc", vec![]),
    ])
    .unwrap_err();
    assert!(matches!(error, FleetError::DependencyNotEarlier { .. }));
}

#[test]
fn fleet_session_budget_is_enforced() {
    // An oversized fleet (each process demanding 50 pooled + 50 dedicated) blows the
    // shared-Postgres budget and is rejected at construction — exercises the failing
    // branch, not just its neighbor.
    let hog = |name| ServiceSpec {
        name,
        executable_package: name,
        http_port: 1,
        edge_port: None,
        player_port: None,
        dependencies: vec![],
        env: BTreeMap::new(),
        overrideable_env: &[],
        pool_budget: PoolBudget { pool_max: 50, dedicated: 50 },
    };
    let error = FleetSpec::new(vec![hog("a-svc"), hog("b-svc")]).unwrap_err();
    assert!(matches!(error, FleetError::PoolBudgetExceeded { total, .. } if total == 200));

    // The real proof fleet is comfortably within budget (built via `.expect` in
    // `game_backend_fleet`, which would panic on violation — assert it explicitly too).
    let fleet = game_backend_fleet(&inputs(), FleetFlavor::Proof);
    let total: u32 = fleet
        .services()
        .iter()
        .map(|service| service.pool_budget.pool_max + service.pool_budget.dedicated)
        .sum();
    assert!(
        total <= crate::fleet::PG_SESSION_BUDGET,
        "proof fleet reserves {total} sessions, budget is {}",
        crate::fleet::PG_SESSION_BUDGET
    );

    // The real peak is not the fleet alone: splitproof runs its `[REPLICAS]` second
    // leaderboard-svc and its own sqlx pool WHILE the whole fleet is up. This pins the
    // DERIVATION's internal consistency — the fleet plus every itemized harness term
    // stays inside the provisioning `USABLE_PG_SESSIONS` is derived from — not
    // exhaustion on a live cluster, which no constant can know: that is the
    // `require_pg_session_floor` probe's job, against what the cluster reports.
    assert!(
        total + crate::fleet::HARNESS_RESERVE <= crate::fleet::USABLE_PG_SESSIONS,
        "fleet {total} + harness reserve {} exceeds the {} sessions the recommended \
         provisioning is derived to offer",
        crate::fleet::HARNESS_RESERVE,
        crate::fleet::USABLE_PG_SESSIONS
    );

    // The replica term is the same reservation a DB-backed split service makes — a
    // hand-tuned number here would stop tracking the process it stands for.
    let leaderboard = fleet.service("leaderboard-svc").unwrap();
    assert_eq!(
        crate::fleet::SPLITPROOF_REPLICA_SESSIONS,
        leaderboard.pool_budget.pool_max + leaderboard.pool_budget.dedicated
    );
}

#[test]
fn pool_budget_dedicated_matches_exported_session_constants() {
    // Anti-drift: the fleet's `u32` mirrors must equal the real crates' exported session
    // constants (pattern: seeded_schedule_names_are_contract). If a plane changes its
    // worker/listener count, this fails until the mirror + budget are re-derived.
    assert_eq!(crate::fleet::AE_WORKERS as usize, asyncevents::WORKERS);
    assert_eq!(crate::fleet::AE_WAKEUP_SESSIONS as usize, asyncevents::WAKEUP_SESSIONS);
    assert_eq!(
        crate::fleet::INVALIDATION_LISTEN_SESSIONS as usize,
        invalidation::LISTEN_SESSIONS
    );
    assert_eq!(
        crate::fleet::SCHEDULER_FIRE_SESSIONS as usize,
        scheduler::DEDICATED_FIRE_SESSIONS
    );
    assert_eq!(
        crate::fleet::AE_TRANSIENT_POISON_SESSIONS as usize,
        asyncevents::TRANSIENT_POISON_SESSIONS
    );

    // And the per-service reservations are composed from those mirrors, asserted for
    // EVERY service in the fleet (a wrong count on any svc must be visible, not just
    // on spot-checked ones): each DB-backed svc reserves both planes, the scheduler
    // adds its fire connection, gateway (DB-less) reserves nothing.
    let fleet = game_backend_fleet(&inputs(), FleetFlavor::Proof);
    let plane = crate::fleet::AE_WORKERS
        + crate::fleet::AE_WAKEUP_SESSIONS
        + crate::fleet::INVALIDATION_LISTEN_SESSIONS;
    for service in fleet.services() {
        let expected_dedicated = match service.name {
            "gateway-svc" => 0,
            "scheduler-svc" => plane + crate::fleet::SCHEDULER_FIRE_SESSIONS,
            _ => plane,
        };
        assert_eq!(
            service.pool_budget.dedicated, expected_dedicated,
            "{}: dedicated sessions drifted from the composition formula",
            service.name
        );
        // One field feeds BOTH runtime and invariant: a DB-backed svc's injected env
        // cap equals its reserved pool_max; the DB-less gateway gets no injection and
        // reserves no pool.
        let injected = service.env.get("DATABASE_POOL_MAX_CONNECTIONS");
        if service.name == "gateway-svc" {
            assert_eq!(service.pool_budget.pool_max, 0, "gateway reserves no pool");
            assert!(injected.is_none(), "gateway must not get a pool cap injected");
        } else {
            assert_eq!(
                injected.map(String::as_str),
                Some(service.pool_budget.pool_max.to_string().as_str()),
                "{}: injected pool cap != reserved pool_max",
                service.name
            );
        }
    }
    // The monolith's single-process reservation follows the same composition.
    let monolith = game_backend_monolith(
        &inputs(),
        FleetFlavor::Proof,
        &EnvironmentSnapshot::from_values([]),
    );
    assert_eq!(
        monolith.pool_budget.dedicated,
        plane + crate::fleet::SCHEDULER_FIRE_SESSIONS
    );
    assert_eq!(
        monolith.env.get("DATABASE_POOL_MAX_CONNECTIONS").map(String::as_str),
        Some(monolith.pool_budget.pool_max.to_string().as_str())
    );
}

#[cfg(windows)]
#[test]
fn build_environment_preserves_appdata_case_insensitively() {
    let environment = EnvironmentSnapshot::from_values([
        ("appdata".into(), r"C:\Users\test\AppData\Roaming".into()),
        ("localappdata".into(), r"C:\Users\test\AppData\Local".into()),
    ]);

    let env = environment.build_environment();

    assert_eq!(
        env.get("APPDATA").map(String::as_str),
        Some(r"C:\Users\test\AppData\Roaming")
    );
    assert!(!env.contains_key("LOCALAPPDATA"));
}

#[cfg(windows)]
#[test]
fn sanitized_build_path_contains_the_discovered_msvc_linker() {
    let env = build_environment();
    assert_eq!(
        env.get("ProgramFiles(x86)").map(String::as_str),
        std::env::var("ProgramFiles(x86)").ok().as_deref()
    );
    let path = env.get("PATH").expect("build environment has PATH");
    let lower_components = |directory: &std::path::Path| -> Vec<String> {
        directory
            .components()
            .map(|part| part.as_os_str().to_string_lossy().to_ascii_lowercase())
            .collect()
    };
    let first = std::env::split_paths(path).next().expect("PATH is not empty");
    let parts = lower_components(&first);
    assert!(first.join("link.exe").is_file(), "first PATH entry {first:?} has no link.exe");
    assert!(
        parts.windows(3).any(|window| window == ["vc", "tools", "msvc"]),
        "first PATH entry {first:?} is not under VC\\Tools\\MSVC"
    );
    assert!(
        parts.ends_with(&["bin".into(), "hostx64".into(), "x64".into()]),
        "first PATH entry {first:?} is not the Hostx64\\x64 linker dir"
    );
    let libraries: Vec<_> = env.get("LIB").into_iter().flat_map(std::env::split_paths).collect();
    assert!(
        libraries.iter().any(|directory| {
            let parts = lower_components(directory);
            parts.windows(3).any(|window| window == ["vc", "tools", "msvc"])
                && parts.ends_with(&["lib".into(), "x64".into()])
                && directory.join("msvcrt.lib").is_file()
        }),
        "sanitized build LIB must contain the MSVC lib\\x64 dir with msvcrt.lib"
    );
    assert!(
        libraries.iter().any(|directory| directory.join("kernel32.lib").is_file()),
        "sanitized build LIB must contain the Windows SDK libraries"
    );
    assert!(env.contains_key("INCLUDE"));
}

// ============================================================================
// The Postgres session preflight's PURE verdict. It is `pub` so it can be driven without a
// cluster; the probe that feeds it is the only part that needs one.
// ============================================================================

fn capacity(max_connections: u32, reserved: u32) -> PgSessionCapacity {
    PgSessionCapacity { max_connections, reserved }
}

#[test]
fn a_cluster_at_or_above_the_rollout_reservation_admits_it() {
    // Exactly at the floor: the comparison is `>=`, so a rollout that fits precisely runs.
    check_pg_session_floor(capacity(100, 3), 97).expect("97 usable sessions admit 97 reserved");
    check_pg_session_floor(capacity(150, 3), 98).expect("headroom admits");
    check_pg_session_floor(capacity(1, 1), 0).expect("a reservation of zero always fits");
}

/// The refusal happens BEFORE anything is spawned, so its message is the operator's only
/// instrument. `pg_reload_conf()` silently does NOT apply `max_connections`, which is
/// postmaster-context — a remedy that omits the restart sends the operator round twice.
#[test]
fn a_cluster_below_the_rollout_reservation_is_refused_with_both_halves_of_the_remedy() {
    let error = check_pg_session_floor(capacity(100, 3), 101)
        .expect_err("97 usable sessions cannot carry a 101-session rollout");
    let message = error.to_string();
    assert!(message.contains("97"), "the observed usable count: {message}");
    assert!(message.contains("100"), "the observed max_connections: {message}");
    assert!(message.contains("3 reserved"), "the observed reservation: {message}");
    assert!(message.contains("101"), "what this rollout reserves: {message}");
    assert!(
        message.contains(&format!(
            "ALTER SYSTEM SET max_connections = {REQUIRED_MAX_CONNECTIONS};"
        )),
        "the remedy must be a copyable statement: {message}"
    );
    assert!(
        message.to_lowercase().contains("restart"),
        "a reload does NOT apply max_connections — the restart must be named: {message}"
    );
    assert!(
        matches!(error, FleetError::PgSessionFloor { required: 101, .. }),
        "the verdict carries what was asked for"
    );
}

/// Where the operator's OWN reservations put the recommended provisioning out of reach, the
/// remedy must name a bigger number than the recommendation — otherwise following it
/// verbatim leaves the next rollout refused for the same reason.
#[test]
fn the_remedy_outgrows_the_recommendation_when_the_reservation_demands_it() {
    let required = REQUIRED_MAX_CONNECTIONS + 10;
    let message = check_pg_session_floor(capacity(100, 20), required)
        .unwrap_err()
        .to_string();
    assert!(
        message.contains(&format!("max_connections = {};", required + 20)),
        "the suggestion must cover the reservation PLUS what the cluster withholds: {message}"
    );
}

#[test]
fn usable_sessions_never_underflow_a_larger_reservation() {
    assert_eq!(capacity(10, 25).usable(), 0);
    check_pg_session_floor(capacity(10, 25), 1).expect_err("no sessions carries no rollout");
}

/// The verdict is taken against the CALLER's own reservation, not against the whole split's:
/// a monolith rollout must not be refused for a fleet it is not spawning. The split fleet
/// plus the harness is the largest reservation any entry point takes, and the recommended
/// provisioning is what it is derived within.
#[test]
fn the_recommended_provisioning_admits_the_largest_reservation_any_entry_point_takes() {
    let split = game_backend_fleet(&inputs(), FleetFlavor::Development).pg_session_reservation();
    let monolith_total = game_backend_monolith(
        &inputs(),
        FleetFlavor::Development,
        &EnvironmentSnapshot::from_values([]),
    )
    .pool_budget
    .sessions();
    let stock = capacity(REQUIRED_MAX_CONNECTIONS, 3);

    check_pg_session_floor(stock, split + HARNESS_RESERVE)
        .expect("the recommended provisioning must admit the split fleet beside the harness");
    check_pg_session_floor(stock, monolith_total)
        .expect("and the monolith, which reserves far less");
    assert!(
        monolith_total < split,
        "a monolith rollout must be charged less than the split it is not spawning"
    );
    assert!(
        split <= PG_SESSION_BUDGET,
        "the split fleet reserves {split}, above the {PG_SESSION_BUDGET} budget it is \
         derived within"
    );
}
