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

/// Removes allowlisted ambient-env keys from a composed env so a golden
/// assertion doesn't depend on the machine running the test (RUST_LOG, PATH,
/// etc. vary by shell).
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

#[test]
fn accounts_svc_golden_env() {
    let svc = split_fleet().into_iter().find(|s| s.name == "accounts-svc").unwrap();
    let env = strip_allowlist(&compose_env(&svc, &fake_inputs()));
    let want = expected(&[
        ("PORT", ":8084"),
        ("EDGE_ADDR", ":9003"),
        ("DATABASE_URL", "postgres://gamebackend:gamebackend@localhost:5432/gamebackend"),
        ("DATABASE_POOL_MAX_CONNECTIONS", "3"),
        ("EDGE_CA_CERT", "/fake/ca-cert.pem"),
        ("EDGE_CA_KEY", "/fake/ca-key.pem"),
        ("ACCOUNTS_DEV_AUTH", "1"),
    ]);
    assert_eq!(env, want);
}

#[test]
fn gateway_svc_golden_env() {
    let svc = split_fleet().into_iter().find(|s| s.name == "gateway-svc").unwrap();
    let env = strip_allowlist(&compose_env(&svc, &fake_inputs()));
    let want = expected(&[
        ("PORT", ":8082"),
        // gateway-svc has no edge_port of its own (it hosts the player-QUIC
        // plane and dials internal edges as a client, never serves one).
        ("EDGE_CA_CERT", "/fake/ca-cert.pem"),
        ("EDGE_CA_KEY", "/fake/ca-key.pem"),
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
    ]);
    // gateway-svc must NOT receive DATABASE_URL/DATABASE_POOL_MAX_CONNECTIONS
    // (pure-transport front door, no pool) despite getting the CA material.
    assert!(!env.contains_key(&OsString::from("DATABASE_URL")));
    assert!(!env.contains_key(&OsString::from("DATABASE_POOL_MAX_CONNECTIONS")));
    assert_eq!(env, want);
}

#[test]
fn monolith_golden_env() {
    let svc = monolith();
    let env = strip_allowlist(&compose_env(&svc, &fake_inputs()));
    let want = expected(&[
        ("PORT", ":8080"),
        ("DATABASE_URL", "postgres://gamebackend:gamebackend@localhost:5432/gamebackend"),
        ("DATABASE_POOL_MAX_CONNECTIONS", "20"),
        ("EDGE_CA_CERT", "/fake/ca-cert.pem"),
        ("EDGE_CA_KEY", "/fake/ca-key.pem"),
        ("PLAYER_EDGE_ADDR", ":9100"),
        ("APIKEYS_DEV_SEED", "1"),
        ("ACCOUNTS_DEV_AUTH", "1"),
        ("INVENTORY_DEV_GRANT", "1"),
        ("TLS_MODE", "off"),
        ("ADMIN_COOKIE_SECURE", "0"),
        ("TRUSTED_PROXY_CIDRS", "127.0.0.1/32"),
    ]);
    assert_eq!(env, want);
}

#[test]
fn boot_order_respects_edge_dependencies() {
    let fleet = split_fleet();
    let index = |name: &str| fleet.iter().position(|svc| svc.name == name).unwrap();

    // config-svc before characters-svc before inventory-svc (each dials the
    // previous over its own EDGE_ADDR).
    assert!(index("config-svc") < index("characters-svc"));
    assert!(index("characters-svc") < index("inventory-svc"));

    // gateway-svc dials 6 peers (characters/inventory/accounts/match/
    // leaderboard/apikeys) — all must boot earlier.
    let gateway = index("gateway-svc");
    for peer in [
        "characters-svc",
        "inventory-svc",
        "accounts-svc",
        "match-svc",
        "leaderboard-svc",
        "apikeys-svc",
    ] {
        assert!(index(peer) < gateway, "{peer} must boot before gateway-svc");
    }

    // admin-svc dials 7 peers and boots last.
    assert_eq!(fleet.last().unwrap().name, "admin-svc");
    for peer in [
        "characters-svc",
        "inventory-svc",
        "config-svc",
        "accounts-svc",
        "audit-svc",
        "scheduler-svc",
        "apikeys-svc",
    ] {
        assert!(index(peer) < gateway || index(peer) < fleet.len() - 1);
    }

    // match-svc dials rating-svc.
    assert!(index("rating-svc") < index("match-svc"));
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

#[test]
fn service_pg_budget_charges_plane_dedicated_for_db_services() {
    let svc = ServiceDef {
        name: "some-svc",
        pkg: "some-svc",
        http_port: 1,
        edge_port: None,
        player_port: None,
        has_db: true,
        pool_max: 3,
        env_extra: &[],
    };
    let (pool, dedicated) = service_pg_budget(&svc);
    assert_eq!(pool, 3);
    assert_eq!(dedicated, PLANE_DEDICATED_SESSIONS);
}

#[test]
fn service_pg_budget_scheduler_charges_one_more_dedicated() {
    let svc = ServiceDef {
        name: "scheduler-svc",
        pkg: "scheduler-svc",
        http_port: 1,
        edge_port: None,
        player_port: None,
        has_db: true,
        pool_max: 3,
        env_extra: &[],
    };
    let (_, dedicated) = service_pg_budget(&svc);
    assert_eq!(dedicated, PLANE_DEDICATED_SESSIONS + SCHEDULER_FIRE_SESSIONS);
}

#[test]
fn service_pg_budget_charges_nothing_for_dbless_service() {
    let svc = ServiceDef {
        name: "gateway-svc",
        pkg: "gateway-svc",
        http_port: 1,
        edge_port: None,
        player_port: None,
        has_db: false,
        pool_max: 0,
        env_extra: &[],
    };
    assert_eq!(service_pg_budget(&svc), (0, 0));
}

/// Proves the dedicated term is load-bearing, not decorative: a synthetic
/// fleet whose POOL-ONLY sum fits the budget but whose pool+dedicated sum
/// does not must be rejected. If `service_pg_budget`'s dedicated component
/// were ever dropped from the sum, this test would start passing where it
/// must fail.
#[test]
fn pool_only_sum_would_pass_but_pool_plus_dedicated_fails() {
    // 25 synthetic DB-backed services, each pool_max = 3 (matches the real
    // SPLIT_SERVICE_POOL_MAX). Pool-only sum = 75, comfortably under the 87
    // budget. But every DB service also charges PLANE_DEDICATED_SESSIONS(4),
    // so the true reservation is 25 * (3 + 4) = 175 — far over budget.
    let synthetic: Vec<ServiceDef> = (0..25)
        .map(|i| ServiceDef {
            name: Box::leak(format!("synthetic-{i}-svc").into_boxed_str()),
            pkg: "synthetic-svc",
            http_port: 1,
            edge_port: None,
            player_port: None,
            has_db: true,
            pool_max: SPLIT_SERVICE_POOL_MAX,
            env_extra: &[],
        })
        .collect();

    let pool_only_sum: u32 = synthetic.iter().map(|svc| svc.pool_max).sum();
    assert!(
        pool_only_sum <= PG_SESSION_BUDGET,
        "test fixture must be pool-only-green to prove the dedicated term matters"
    );

    let full_sum: u32 = synthetic
        .iter()
        .map(|svc| {
            let (pool, dedicated) = service_pg_budget(svc);
            pool + dedicated
        })
        .sum();
    assert!(
        full_sum > PG_SESSION_BUDGET,
        "full pool+dedicated sum must exceed budget for this fixture to be meaningful"
    );
}
