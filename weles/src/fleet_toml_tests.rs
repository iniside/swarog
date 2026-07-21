use super::*;

/// Parses a fleet expected to be well-FORMED (used by the `validate` failing
/// branches, where the defect is semantic, not syntactic).
fn parsed(text: &str) -> Fleet {
    parse(text).expect("fixture should parse")
}

/// The alternate-screen `{:#}` form so an assertion sees the whole anyhow chain
/// (the top context PLUS the `toml`/bail source), not just the outermost line.
fn chain(err: &anyhow::Error) -> String {
    format!("{err:#}")
}

// ---------------------------------------------------------------------------
// Happy path: a small 2-service fleet with a [[prepare]] block round-trips to
// the expected owned types, and validates clean.
// ---------------------------------------------------------------------------

const GOOD_FLEET: &str = r#"
passthrough = ["DATABASE_URL"]

[[prepare]]
name = "edgeca"
run = "edgeca"

[[prepare]]
name = "admin-seed"
run = "adminctl"
args = ["create-user", "--username", "admin"]
passthrough = ["DATABASE_URL"]
env = { ADMINCTL_PASSWORD = "admin" }
timeout_secs = 60

[[service]]
name = "config-svc"
pkg = "config-svc"
provider = "config"
http_port = 8083
edge_port = 9002

[[service]]
name = "characters-svc"
pkg = "characters-svc"
provider = "characters"
http_port = 8080
edge_port = 9000
env = { DATABASE_POOL_MAX_CONNECTIONS = "3" }

[[service.peer]]
env_key = "CONFIG_EDGE_ADDR"
provider = "config"
kind = "edge"
"#;

#[test]
fn good_fleet_parses_to_expected_owned_types() {
    let fleet = parsed(GOOD_FLEET);

    assert_eq!(fleet.passthrough, vec!["DATABASE_URL".to_string()]);

    // Prepare hooks: order preserved, defaults applied where omitted.
    assert_eq!(fleet.prepare.len(), 2);

    let ca = &fleet.prepare[0];
    assert_eq!(ca.name, "edgeca");
    assert_eq!(ca.run, "edgeca");
    assert!(ca.args.is_empty());
    assert!(ca.passthrough.is_empty());
    assert!(ca.env.is_empty());
    // Omission → 0, the sentinel prep::run_one_prepare maps to 30 at runtime.
    // This schema does NOT default to 30 (single "30-when-unset" authority).
    assert_eq!(ca.timeout_secs, 0, "omitted timeout_secs stays the 0 sentinel");

    let seed = &fleet.prepare[1];
    assert_eq!(seed.name, "admin-seed");
    assert_eq!(seed.run, "adminctl");
    assert_eq!(seed.args, vec!["create-user", "--username", "admin"]);
    assert_eq!(seed.passthrough, vec!["DATABASE_URL".to_string()]);
    assert_eq!(seed.env.get("ADMINCTL_PASSWORD").map(String::as_str), Some("admin"));
    assert_eq!(seed.timeout_secs, 60);

    // Services: owned ServiceDef, resolve/peer folded into Addrs.
    assert_eq!(fleet.services.len(), 2);

    let config = &fleet.services[0];
    assert_eq!(config.name, "config-svc");
    assert_eq!(config.provider.as_deref(), Some("config"));
    assert_eq!(config.http_port, 8083);
    assert_eq!(config.edge_port, Some(9002));
    assert_eq!(config.player_port, None);
    assert_eq!(config.addrs, Addrs::Told(vec![]), "no peers, no resolve ⇒ empty Told");

    let characters = &fleet.services[1];
    assert_eq!(characters.name, "characters-svc");
    assert_eq!(
        characters.addrs,
        Addrs::Told(vec![(
            "CONFIG_EDGE_ADDR".to_string(),
            "config".to_string(),
            AddrKind::Edge
        )])
    );
    assert_eq!(
        characters.env.get("DATABASE_POOL_MAX_CONNECTIONS").map(String::as_str),
        Some("3"),
        "an opaque operator env key is carried verbatim, weles domain-blind to it"
    );

    validate(&fleet).expect("the good fleet must validate");
}

#[test]
fn resolve_asks_folds_into_addrs_asks() {
    let text = r#"
[[service]]
name = "gateway-svc"
pkg = "gateway-svc"
provider = "gateway"
http_port = 8082
player_port = 9100
resolve = "asks"
"#;
    let fleet = parsed(text);
    assert_eq!(fleet.services[0].addrs, Addrs::Asks);
}

// ---------------------------------------------------------------------------
// Failing branches — each a DISTINCT Err.
// ---------------------------------------------------------------------------

#[test]
fn duplicate_port_is_rejected() {
    // Two services claiming http_port 8080.
    let text = r#"
[[service]]
name = "a-svc"
pkg = "a-svc"
provider = "a"
http_port = 8080

[[service]]
name = "b-svc"
pkg = "b-svc"
provider = "b"
http_port = 8080
"#;
    let err = validate(&parsed(text)).expect_err("duplicate port must fail");
    let msg = chain(&err);
    assert!(msg.contains("8080"), "names the colliding port: {msg}");
    assert!(msg.contains("claimed by both"), "names the collision class: {msg}");
}

#[test]
fn a_port_equal_to_agent_port_is_rejected() {
    let text = format!(
        r#"
[[service]]
name = "a-svc"
pkg = "a-svc"
provider = "a"
http_port = {AGENT_PORT}
"#
    );
    let err = validate(&parsed(&text)).expect_err("AGENT_PORT squat must fail");
    let msg = chain(&err);
    assert!(msg.contains("agent port"), "names the agent-port collision: {msg}");
    assert!(msg.contains(&AGENT_PORT.to_string()), "names AGENT_PORT: {msg}");
}

#[test]
fn unknown_field_is_rejected_by_deny_unknown_fields() {
    let text = r#"
[[service]]
name = "a-svc"
pkg = "a-svc"
provider = "a"
http_port = 8080
has_db = true
"#;
    let err = parse(text).expect_err("an unknown key must fail deny_unknown_fields");
    let msg = chain(&err);
    assert!(msg.contains("unknown field"), "deny_unknown_fields fired: {msg}");
    assert!(msg.contains("has_db"), "names the offending key: {msg}");
}

#[test]
fn peer_naming_an_absent_provider_is_rejected() {
    let text = r#"
[[service]]
name = "characters-svc"
pkg = "characters-svc"
provider = "characters"
http_port = 8080
edge_port = 9000

[[service.peer]]
env_key = "CONFIG_EDGE_ADDR"
provider = "config"
kind = "edge"
"#;
    let err = validate(&parsed(text)).expect_err("dangling peer must fail");
    let msg = chain(&err);
    assert!(msg.contains("config"), "names the missing provider: {msg}");
    assert!(msg.contains("provides"), "names the class (no service provides it): {msg}");
}

#[test]
fn edge_peer_against_a_service_with_no_edge_is_rejected() {
    // config has NO edge_port; characters asks for its Edge address.
    let text = r#"
[[service]]
name = "config-svc"
pkg = "config-svc"
provider = "config"
http_port = 8083

[[service]]
name = "characters-svc"
pkg = "characters-svc"
provider = "characters"
http_port = 8080
edge_port = 9000

[[service.peer]]
env_key = "CONFIG_EDGE_ADDR"
provider = "config"
kind = "edge"
"#;
    let err = validate(&parsed(text)).expect_err("Edge against edge_port=None must fail");
    let msg = chain(&err);
    assert!(msg.contains("no address of that kind"), "names the kind mismatch: {msg}");
    assert!(msg.contains("Edge"), "names the requested kind: {msg}");
}

#[test]
fn out_of_order_edge_peer_is_rejected() {
    // characters (position 0) dials config's Edge, but config is at position 1.
    let text = r#"
[[service]]
name = "characters-svc"
pkg = "characters-svc"
provider = "characters"
http_port = 8080

[[service.peer]]
env_key = "CONFIG_EDGE_ADDR"
provider = "config"
kind = "edge"

[[service]]
name = "config-svc"
pkg = "config-svc"
provider = "config"
http_port = 8083
edge_port = 9002
"#;
    let err = validate(&parsed(text)).expect_err("out-of-order edge peer must fail");
    let msg = chain(&err);
    assert!(msg.contains("boot order"), "names the boot-order class: {msg}");
    assert!(msg.contains("strictly"), "explains the ordering rule: {msg}");
}

#[test]
fn a_bogus_resolve_value_is_rejected() {
    let text = r#"
[[service]]
name = "gateway-svc"
pkg = "gateway-svc"
provider = "gateway"
http_port = 8082
resolve = "maybe"
"#;
    let err = parse(text).expect_err("resolve other than \"asks\" must fail");
    let msg = chain(&err);
    assert!(msg.contains("unknown resolve"), "names the bad resolve: {msg}");
    assert!(msg.contains("maybe"), "echoes the offending value: {msg}");
}

#[test]
fn a_prepare_name_colliding_with_a_service_is_rejected() {
    let text = r#"
[[prepare]]
name = "config-svc"
run = "edgeca"

[[service]]
name = "config-svc"
pkg = "config-svc"
provider = "config"
http_port = 8083
"#;
    let err = validate(&parsed(text)).expect_err("prepare/service name clash must fail");
    let msg = chain(&err);
    assert!(msg.contains("config-svc"), "names the clashing name: {msg}");
    assert!(msg.contains("clobber"), "names the log-namespace clash: {msg}");
}

#[test]
fn two_services_sharing_a_name_are_rejected() {
    // Distinct ports, same name — clean ports, but the shared log/state-key
    // namespace collides. Caught by the union-uniqueness pass.
    let text = r#"
[[service]]
name = "dup-svc"
pkg = "a-svc"
provider = "a"
http_port = 8080

[[service]]
name = "dup-svc"
pkg = "b-svc"
provider = "b"
http_port = 8081
"#;
    let err = validate(&parsed(text)).expect_err("service/service name clash must fail");
    let msg = chain(&err);
    assert!(msg.contains("dup-svc"), "names the duplicated name: {msg}");
    assert!(msg.contains("clobber"), "names the log-namespace clash: {msg}");
}

#[test]
fn two_prepare_hooks_sharing_a_name_are_rejected() {
    let text = r#"
[[prepare]]
name = "dup-hook"
run = "edgeca"

[[prepare]]
name = "dup-hook"
run = "adminctl"

[[service]]
name = "config-svc"
pkg = "config-svc"
provider = "config"
http_port = 8083
"#;
    let err = validate(&parsed(text)).expect_err("prepare/prepare name clash must fail");
    let msg = chain(&err);
    assert!(msg.contains("dup-hook"), "names the duplicated name: {msg}");
    assert!(msg.contains("clobber"), "names the log-namespace clash: {msg}");
}

#[test]
fn an_omitted_timeout_stays_the_zero_sentinel() {
    // Omission → 0; prep::run_one_prepare maps 0 → DEFAULT_PREPARE_TIMEOUT_SECS
    // (30) at runtime — the SOLE "30-when-unset" authority lives there, not here.
    let text = r#"
[[prepare]]
name = "edgeca"
run = "edgeca"

[[service]]
name = "config-svc"
pkg = "config-svc"
provider = "config"
http_port = 8083
"#;
    let fleet = parsed(text);
    assert_eq!(
        fleet.prepare[0].timeout_secs, 0,
        "omitted timeout_secs stays the 0 sentinel (prep maps 0→30 at runtime)"
    );
}

// ---------------------------------------------------------------------------
// The two SHIPPED fixtures must parse AND validate — they are the exact files
// `weles up` boots and verifyctl's `weles-managed-gateway` loads, so a typo or
// a boot-order/port mistake in either must fail HERE, not at a live rollout.
// ---------------------------------------------------------------------------

#[test]
fn the_split_fixture_parses_and_validates() {
    let fleet = super::load_split_fixture();
    validate(&fleet).expect("weles/fleet.split.toml must validate");
    assert_eq!(fleet.services.len(), 12, "the split fleet is 12 processes");
    // The CA-first ordering the D-PREPARE contract requires, and the argv the
    // hooks were recovered with (44b653c prep.rs).
    assert_eq!(fleet.prepare.len(), 2, "edge-ca then admin-seed");
    assert_eq!(fleet.prepare[0].run, "edgeca");
    assert_eq!(
        fleet.prepare[0].args,
        vec![
            "--cert".to_string(),
            "run/weles/edge-ca.crt".to_string(),
            "--key".to_string(),
            "run/weles/edge-ca.key".to_string()
        ]
    );
    assert_eq!(fleet.prepare[1].run, "adminctl");
    assert_eq!(
        fleet.prepare[1].args,
        vec!["create-user".to_string(), "admin".to_string()]
    );
    assert_eq!(fleet.prepare[1].passthrough, vec!["DATABASE_URL".to_string()]);
    assert_eq!(
        fleet.prepare[1].env.get("ADMINCTL_PASSWORD"),
        Some(&"admin".to_string())
    );
    assert_eq!(fleet.passthrough, vec!["DATABASE_URL".to_string()]);
}

#[test]
fn the_monolith_fixture_parses_and_validates() {
    let fleet = super::load_monolith_fixture();
    validate(&fleet).expect("weles/fleet.monolith.toml must validate");
    assert_eq!(fleet.services.len(), 1, "the monolith is one process");
    assert_eq!(fleet.services[0].name, "server");
    assert!(fleet.services[0].provider.is_none(), "the monolith names no single domain");
    assert_eq!(
        fleet.services[0].env.get("DATABASE_POOL_MAX_CONNECTIONS"),
        Some(&"20".to_string()),
        "the monolith carries the old MONOLITH_POOL_MAX in its env"
    );
}
