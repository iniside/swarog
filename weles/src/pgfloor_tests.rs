//! The three PURE halves of the Postgres session preflight. They are `pub` precisely so a
//! verdict can be driven without a cluster, and the env lookup is an injected closure so a
//! passthrough key is resolved as DATA rather than from this process's environment.

use std::collections::BTreeMap;
use std::ffi::OsString;

use crate::fleet_toml::{Fleet, PrepareCmd};
use crate::manifest::{Addrs, Port, ServiceDef};
use crate::pgfloor::{
    check_pg_session_floor, fleet_dsn, fleet_session_reservation, PgSessionCapacity,
    DEFAULT_DATABASE_URL, PLANE_DEDICATED_SESSIONS, REQUIRED_MAX_CONNECTIONS,
};

fn no_env(_key: &str) -> Option<OsString> {
    None
}

fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
    let map: BTreeMap<String, OsString> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), OsString::from(*v)))
        .collect();
    move |key: &str| map.get(key).cloned()
}

fn service(name: &str, env: &[(&str, &str)]) -> ServiceDef {
    ServiceDef {
        name: name.to_string(),
        pkg: name.to_string(),
        provider: None,
        placement: None,
        http_port: Port::Literal(8080),
        edge_port: None,
        player_port: None,
        addrs: Addrs::Told(Vec::new()),
        env: env
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
    }
}

fn fleet(services: Vec<ServiceDef>, passthrough: &[&str]) -> Fleet {
    Fleet {
        prepare: Vec::new(),
        passthrough: passthrough.iter().map(|k| (*k).to_string()).collect(),
        services,
    }
}

fn capacity(max_connections: u32, reserved: u32) -> PgSessionCapacity {
    PgSessionCapacity {
        max_connections,
        reserved,
    }
}

// ============================================================================
// The verdict.
// ============================================================================

#[test]
fn a_cluster_at_or_above_the_reservation_admits_the_fleet() {
    // Exactly at the floor: the comparison is `>=`, so a fleet that fits precisely boots.
    check_pg_session_floor(capacity(100, 3), 97).expect("97 usable sessions admit 97 reserved");
    check_pg_session_floor(capacity(150, 3), 98).expect("headroom admits");
    // A fleet that reserves nothing is admitted whatever the cluster offers.
    check_pg_session_floor(capacity(1, 1), 0).expect("a reservation of zero always fits");
}

/// The message is the whole value of refusing BEFORE the spawn: an operator who is told only
/// "too few connections" cannot act, and `pg_reload_conf()` silently does NOT apply
/// `max_connections`, which is postmaster-context.
#[test]
fn a_cluster_below_the_reservation_is_refused_with_both_halves_of_the_remedy() {
    let e = check_pg_session_floor(capacity(100, 3), 101)
        .expect_err("97 usable sessions cannot carry a 101-session fleet");
    let msg = format!("{e:#}");
    assert!(msg.contains("97"), "the observed usable count: {msg}");
    assert!(msg.contains("100"), "the observed max_connections: {msg}");
    assert!(msg.contains("3 reserved"), "the observed reservation: {msg}");
    assert!(msg.contains("101"), "what this fleet reserves: {msg}");
    assert!(
        msg.contains(&format!("ALTER SYSTEM SET max_connections = {REQUIRED_MAX_CONNECTIONS};")),
        "the remedy must be a copyable statement: {msg}"
    );
    assert!(
        msg.to_lowercase().contains("restart"),
        "a reload does NOT apply max_connections — the restart must be named: {msg}"
    );
}

/// Where the operator's OWN reservations put the recommended provisioning out of reach, the
/// remedy has to name a bigger number than the recommendation, or following it verbatim
/// leaves the rollout refused a second time.
#[test]
fn the_remedy_outgrows_the_recommendation_when_the_reservation_demands_it() {
    let required = REQUIRED_MAX_CONNECTIONS + 10;
    let e = check_pg_session_floor(capacity(100, 20), required).unwrap_err();
    let msg = format!("{e:#}");
    assert!(
        msg.contains(&format!("max_connections = {};", required + 20)),
        "the suggestion must cover the reservation PLUS what the cluster withholds: {msg}"
    );
}

#[test]
fn usable_sessions_never_underflow_a_larger_reservation() {
    assert_eq!(capacity(10, 25).usable(), 0);
    check_pg_session_floor(capacity(10, 25), 1).expect_err("no sessions carries no fleet");
}

// ============================================================================
// What the fleet reserves.
// ============================================================================

/// weles is domain-BLIND: `DATABASE_POOL_MAX_CONNECTIONS` is the only per-service DB fact an
/// operator writes into a fleet file, so it is the signal that a service opens a pool. A
/// service without it is not charged — `DATABASE_URL` cannot serve, because it is fleet-level
/// passthrough that reaches the DB-less gateway too.
#[test]
fn only_a_service_declaring_a_pool_is_charged_and_it_carries_the_plane_sessions() {
    let f = fleet(
        vec![
            service("characters-svc", &[("DATABASE_POOL_MAX_CONNECTIONS", "2")]),
            service("gateway-svc", &[]),
        ],
        &[],
    );
    assert_eq!(
        fleet_session_reservation(&f, &no_env).unwrap(),
        2 + PLANE_DEDICATED_SESSIONS
    );
    // A fleet with no pooled service reserves nothing, and `require_pg_session_floor` then
    // never probes a cluster at all.
    assert_eq!(
        fleet_session_reservation(&fleet(vec![service("gateway-svc", &[])], &[]), &no_env).unwrap(),
        0
    );
}

#[test]
fn every_pooled_service_is_summed() {
    let f = fleet(
        vec![
            service("a-svc", &[("DATABASE_POOL_MAX_CONNECTIONS", "2")]),
            service("b-svc", &[("DATABASE_POOL_MAX_CONNECTIONS", "5")]),
        ],
        &[],
    );
    assert_eq!(
        fleet_session_reservation(&f, &no_env).unwrap(),
        2 + 5 + 2 * PLANE_DEDICATED_SESSIONS
    );
}

/// `core/app` falls back to a REAL pool for an unparseable value, so skipping it would
/// under-charge a service that goes on to open ten connections.
#[test]
fn a_malformed_pool_declaration_is_refused_rather_than_skipped() {
    for bad in ["", "two", "-1", "2.5"] {
        let f = fleet(
            vec![service("a-svc", &[("DATABASE_POOL_MAX_CONNECTIONS", bad)])],
            &[],
        );
        let got = fleet_session_reservation(&f, &no_env);
        assert!(got.is_err(), "{bad:?} was accepted as a session count: {got:?}");
    }
}

/// A passthrough key is resolved through the SAME lookup the spawn composes with, so the
/// preflight and the spawn can never charge a service differently from what it opens.
#[test]
fn a_pool_declared_only_by_passthrough_is_still_charged() {
    let f = fleet(vec![service("a-svc", &[])], &["DATABASE_POOL_MAX_CONNECTIONS"]);
    let env = env_of(&[("DATABASE_POOL_MAX_CONNECTIONS", "7")]);
    assert_eq!(
        fleet_session_reservation(&f, &env).unwrap(),
        7 + PLANE_DEDICATED_SESSIONS
    );
    // Declared as passthrough but absent from the environment: nothing is forwarded, so
    // nothing is charged.
    assert_eq!(fleet_session_reservation(&f, &no_env).unwrap(), 0);
    // The literal env table is composed LAST and therefore wins over the forwarded value.
    let f = fleet(
        vec![service("a-svc", &[("DATABASE_POOL_MAX_CONNECTIONS", "2")])],
        &["DATABASE_POOL_MAX_CONNECTIONS"],
    );
    assert_eq!(
        fleet_session_reservation(&f, &env).unwrap(),
        2 + PLANE_DEDICATED_SESSIONS
    );
}

/// The committed 15-process split fixture, charged through the real path — the number the
/// preflight refuses a rollout against.
#[test]
fn the_committed_split_fixture_reserves_a_stated_number_of_sessions() {
    let f = crate::test_fixtures::load_split_fixture();
    let reserved = fleet_session_reservation(&f, &no_env).unwrap();
    assert!(
        reserved > 0,
        "the split fixture declares pooled services, so it must be charged"
    );
    check_pg_session_floor(capacity(REQUIRED_MAX_CONNECTIONS, 3), reserved)
        .expect("the recommended provisioning must admit the committed split fixture");
}

// ============================================================================
// Which cluster the fleet uses.
// ============================================================================

/// A fleet that names no DSN still opens every session it reserved — against `core/app`'s
/// own default. Answering "no cluster" here would skip the probe in exactly the case the
/// probe is for.
#[test]
fn a_fleet_naming_no_dsn_falls_back_to_the_process_default() {
    let f = fleet(vec![service("a-svc", &[])], &[]);
    assert_eq!(fleet_dsn(&f, &no_env).unwrap(), DEFAULT_DATABASE_URL);
}

#[test]
fn one_dsn_repeated_across_services_and_hooks_is_one_cluster() {
    let mut f = fleet(
        vec![
            service("a-svc", &[("DATABASE_URL", "postgres://one")]),
            service("b-svc", &[("DATABASE_URL", "postgres://one")]),
        ],
        &[],
    );
    f.prepare.push(PrepareCmd {
        name: "seed".into(),
        run: "adminctl".into(),
        args: Vec::new(),
        env: [("DATABASE_URL".to_string(), "postgres://one".to_string())]
            .into_iter()
            .collect(),
        passthrough: Vec::new(),
        timeout_secs: 0,
    });
    assert_eq!(fleet_dsn(&f, &no_env).unwrap(), "postgres://one");
}

/// A reservation summed over the whole fleet says nothing about a cluster only part of it
/// connects to, so that fleet shape is REFUSED rather than checked against one of them.
#[test]
fn a_fleet_pointing_at_two_clusters_is_refused() {
    let f = fleet(
        vec![
            service("a-svc", &[("DATABASE_URL", "postgres://one")]),
            service("b-svc", &[("DATABASE_URL", "postgres://two")]),
        ],
        &[],
    );
    let msg = fleet_dsn(&f, &no_env)
        .expect_err("two clusters must be refused")
        .to_string();
    assert!(msg.contains('2'), "the count must be named: {msg}");
}

#[test]
fn a_dsn_forwarded_by_passthrough_is_resolved_through_the_injected_lookup() {
    let f = fleet(vec![service("a-svc", &[])], &["DATABASE_URL"]);
    let env = env_of(&[("DATABASE_URL", "postgres://forwarded")]);
    assert_eq!(fleet_dsn(&f, &env).unwrap(), "postgres://forwarded");
    // The same fleet with the key absent from the environment takes the fallback, never a
    // silent empty DSN.
    assert_eq!(fleet_dsn(&f, &no_env).unwrap(), DEFAULT_DATABASE_URL);
}

/// The copied constants are what the `weles-wire-contract` stage pins against processctl;
/// these assertions are the local statement of what this file believes them to be.
#[test]
fn the_copied_budget_constants_hold_their_documented_values() {
    assert_eq!(REQUIRED_MAX_CONNECTIONS, 150);
    assert_eq!(PLANE_DEDICATED_SESSIONS, 4);
    assert_eq!(
        DEFAULT_DATABASE_URL,
        "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable"
    );
}
