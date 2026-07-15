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
    ];
    for svc in &services {
        for key in synthesized.iter().copied().chain(svc.env_extra.iter().map(|(k, _)| *k)) {
            assert!(
                !SERVICE_ENV_ALLOWLIST.iter().any(|a| a.eq_ignore_ascii_case(key)),
                "{}: manifest key {key} collides with SERVICE_ENV_ALLOWLIST — \
                 strip_allowlist would hide it from the goldens",
                svc.name
            );
        }
    }
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

    // admin-svc dials 7 peers — all must boot earlier — and boots last.
    assert_eq!(fleet.last().unwrap().name, "admin-svc");
    let admin = index("admin-svc");
    for peer in [
        "characters-svc",
        "inventory-svc",
        "config-svc",
        "accounts-svc",
        "audit-svc",
        "scheduler-svc",
        "apikeys-svc",
    ] {
        assert!(index(peer) < admin, "{peer} must boot before admin-svc");
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

fn synthetic_db_svc(name: &'static str, pool_max: u32) -> ServiceDef {
    ServiceDef {
        name,
        pkg: "synthetic-svc",
        http_port: 1,
        edge_port: None,
        player_port: None,
        has_db: true,
        pool_max,
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
        http_port: 1,
        edge_port: None,
        player_port: None,
        has_db: false,
        pool_max: 0,
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
