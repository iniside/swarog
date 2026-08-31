use std::collections::BTreeMap;

use crate::{
    check_pg_session_floor, game_backend_fleet, game_backend_fleet_with_environment,
    game_backend_monolith, EnvironmentSnapshot, FleetError, FleetFlavor, FleetInputs, FleetSpec,
    PgSessionCapacity, PoolBudget, ServiceSpec, HARNESS_RESERVE, PG_SESSION_BUDGET,
    REQUIRED_MAX_CONNECTIONS,
};
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
fn inherited_application_overrides_are_explicit_and_cannot_rewire_topology() {
    let environment = EnvironmentSnapshot::from_values([
        ("ACCOUNTS_DEV_AUTH".into(), "0".into()),
        ("PORT".into(), ":9999".into()),
        ("EDGE_ADDR".into(), ":9998".into()),
        ("EDGE_CA_KEY".into(), "attacker-key".into()),
        ("HTTP_PROXY".into(), "http://proxy".into()),
    ]);
    let fleet = game_backend_fleet_with_environment(
        &inputs(), FleetFlavor::Development, &environment,
    );
    let accounts = fleet.service("accounts-svc").unwrap();
    assert_eq!(accounts.env.get("ACCOUNTS_DEV_AUTH").map(String::as_str), Some("0"));
    assert_eq!(accounts.env.get("PORT").map(String::as_str), Some(":8084"));
    assert_eq!(accounts.env.get("EDGE_ADDR").map(String::as_str), Some(":9003"));
    assert_eq!(accounts.env.get("EDGE_CA_KEY").map(String::as_str), Some("run/edge-ca.key"));
    assert!(!accounts.env.contains_key("HTTP_PROXY"));

    let monolith = game_backend_monolith(&inputs(), FleetFlavor::Development, &environment);
    assert_eq!(monolith.env.get("PORT").map(String::as_str), Some(":8080"));
    assert_eq!(monolith.env.get("EDGE_CA_KEY").map(String::as_str), Some("run/edge-ca.key"));
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
fn proof_fleet_is_the_canonical_fifteen_service_snapshot() {
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
        ("wallet-svc", "wallet-svc", 8092, Some(9010), None, vec!["config-svc"]),
        ("notifications-svc", "notifications-svc", 8093, Some(9011), None, vec![]),
        ("mail-svc", "mail-svc", 8094, Some(9012), None, vec![]),
        ("gateway-svc", "gateway-svc", 8082, None, Some(9100), vec!["characters-svc", "inventory-svc", "accounts-svc", "match-svc", "leaderboard-svc", "apikeys-svc", "wallet-svc", "notifications-svc"]),
        ("admin-svc", "admin-svc", 8085, None, None, vec!["characters-svc", "inventory-svc", "config-svc", "accounts-svc", "audit-svc", "scheduler-svc", "apikeys-svc", "wallet-svc", "notifications-svc", "mail-svc"]),
    ];

    // Per-service diff, the shape `FleetSpec::validate_names` reports drift in: a reader
    // gets the service that moved, not two 15-element vectors to align by eye.
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
    assert!(matches!(proof.service("missing"), Err(FleetError::UnknownService(_))));
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
    assert!(
        std::env::split_paths(path).any(|directory| directory.join("link.exe").is_file()),
        "sanitized build PATH must contain an installed MSVC linker"
    );
    assert!(
        env.get("LIB")
            .into_iter()
            .flat_map(std::env::split_paths)
            .any(|directory| directory.join("kernel32.lib").is_file()),
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
