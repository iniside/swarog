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
    assert_eq!(config.http_port, Port::Literal(8083));
    assert_eq!(config.edge_port, Some(Port::Literal(9002)));
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
// Placement — a manifest ANNOTATION (weles-design.md:245), NOT an address.
// Single-machine: absent or the reserved "local" sentinel is legal and changes
// NO address; any real node name fails closed (no node registry yet).
// ---------------------------------------------------------------------------

#[test]
fn placement_local_validates_and_leaves_the_address_untouched() {
    let text = r#"
[[service]]
name = "config-svc"
pkg = "config-svc"
provider = "config"
placement = "local"
http_port = 8083
edge_port = 9002
"#;
    let fleet = parsed(text);
    let config = &fleet.services[0];
    assert_eq!(config.placement.as_deref(), Some("local"), "the sentinel is carried verbatim");
    validate(&fleet).expect("placement = \"local\" is a legal single-machine value");
    // The whole point: a placement annotation does NOT touch the address — the
    // address stays agent-resolved (loopback on one node).
    assert_eq!(
        manifest::service_addr(config, AddrKind::Http).as_deref(),
        Some("127.0.0.1:8083"),
        "placement is an annotation, not an address — service_addr is unchanged"
    );
}

#[test]
fn omitted_placement_defaults_to_none_and_validates() {
    // GOOD_FLEET declares no placement on either service.
    let fleet = parsed(GOOD_FLEET);
    assert!(
        fleet.services.iter().all(|svc| svc.placement.is_none()),
        "an omitted placement key is None, never a defaulted sentinel"
    );
    validate(&fleet).expect("a fleet with no placement must validate");
}

#[test]
fn a_real_node_name_placement_is_rejected() {
    // Any value other than "local" names a node with nowhere to resolve — there
    // is no node registry yet, so it fails closed rather than silently no-oping.
    let text = r#"
[[service]]
name = "config-svc"
pkg = "config-svc"
provider = "config"
placement = "node-b"
http_port = 8083
"#;
    let err = validate(&parsed(text)).expect_err("a real node name must fail closed");
    let msg = chain(&err);
    assert!(msg.contains("config-svc"), "names the offending service: {msg}");
    assert!(msg.contains("node-b"), "echoes the rejected placement value: {msg}");
    assert!(
        msg.contains("multi-node placement is not supported yet"),
        "names why it is rejected: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Replicated-provider Told-peer guard — a Told peer carries exactly ONE address
// in one env var, and manifest::peer_addr resolves it FIRST-match, so a Told
// reference to a provider two services provide would silently see only the
// first replica. That misconfiguration must fail HERE (a replicated provider is
// consumed with resolve = "asks", whose PeerAddrs returns all instances).
// ---------------------------------------------------------------------------

#[test]
fn a_told_peer_to_a_replicated_provider_is_rejected() {
    // Two services provide "characters"; a THIRD Told-consumes it over HTTP
    // (Http has no boot-order rule, so this reaches the multiplicity branch
    // after validate_peers passes on the first-match provider).
    let text = r#"
[[service]]
name = "characters-a"
pkg = "characters-svc"
provider = "characters"
http_port = 8080

[[service]]
name = "characters-b"
pkg = "characters-svc"
provider = "characters"
http_port = 8081

[[service]]
name = "consumer-svc"
pkg = "consumer-svc"
provider = "consumer"
http_port = 8082

[[service.peer]]
env_key = "CHARACTERS_HTTP_ADDR"
provider = "characters"
kind = "http"
"#;
    let err = validate(&parsed(text)).expect_err("a Told peer to a 2-instance provider must fail");
    let msg = chain(&err);
    assert!(msg.contains("consumer-svc"), "names the consumer: {msg}");
    assert!(msg.contains("CHARACTERS_HTTP_ADDR"), "names the peer env_key: {msg}");
    assert!(msg.contains("characters"), "names the replicated provider: {msg}");
    assert!(msg.contains('2'), "names the instance count: {msg}");
    assert!(
        msg.contains("resolve=\"asks\""),
        "points at the fix (use asks for a replicated provider): {msg}"
    );
}

#[test]
fn asks_only_replicas_stay_legal() {
    // Same two "characters" replicas, but the consumer ASKS (its PeerAddrs
    // returns all instances) rather than being Told — so nothing silently
    // resolves to the first replica, and the guard must NOT fire.
    let text = r#"
[[service]]
name = "characters-a"
pkg = "characters-svc"
provider = "characters"
http_port = 8080

[[service]]
name = "characters-b"
pkg = "characters-svc"
provider = "characters"
http_port = 8081

[[service]]
name = "gateway-svc"
pkg = "gateway-svc"
provider = "gateway"
http_port = 8082
resolve = "asks"
"#;
    let fleet = parsed(text);
    validate(&fleet).expect("a replicated provider consumed only via Asks is legal");
}

#[test]
fn two_none_providers_are_not_a_replicated_provider() {
    // provider = None on both (the monolith shape). Two Nones must NOT be
    // counted as one shared provider — that would be a false positive. Distinct
    // names/ports keep the other passes clean so only the None-skip is exercised.
    let text = r#"
[[service]]
name = "server-a"
pkg = "server"
http_port = 8080

[[service]]
name = "server-b"
pkg = "server"
http_port = 8081
"#;
    let fleet = parsed(text);
    assert!(
        fleet.services.iter().all(|svc| svc.provider.is_none()),
        "both are provider = None (monolith shape)"
    );
    validate(&fleet).expect("two None-provider services must not trip the replicated-provider guard");
}

// ---------------------------------------------------------------------------
// Minting (A4): a `"mint"` port parses to Port::Mint, a bogus string is a loud
// error, and a Told consumer of a mintable provider fails closed (only an Asks
// consumer can learn a not-yet-bound minted address).
// ---------------------------------------------------------------------------

#[test]
fn a_mint_port_parses_to_the_mint_variant() {
    // Both port fields authored as the explicit marker `"mint"` — anti-magic:
    // no "0 means mint", the string is the ONLY request. A literal integer still
    // parses to Port::Literal (the happy-path test above pins that).
    let text = r#"
[[service]]
name = "gateway-svc"
pkg = "gateway-svc"
provider = "gateway"
http_port = "mint"
edge_port = "mint"
"#;
    let fleet = parsed(text);
    assert_eq!(fleet.services[0].http_port, Port::Mint, "http \"mint\" ⇒ Port::Mint");
    assert_eq!(
        fleet.services[0].edge_port,
        Some(Port::Mint),
        "edge \"mint\" ⇒ Some(Port::Mint)"
    );
}

#[test]
fn a_bogus_port_string_is_rejected() {
    // Any string other than "mint" is a loud parse error — a typo must never
    // silently fall through to a literal or to mint.
    let text = r#"
[[service]]
name = "gateway-svc"
pkg = "gateway-svc"
provider = "gateway"
http_port = "ephemeral"
"#;
    let err = parse(text).expect_err("a non-\"mint\" port string must fail");
    let msg = chain(&err);
    assert!(msg.contains("mint"), "names the only accepted marker: {msg}");
    assert!(msg.contains("ephemeral"), "echoes the offending value: {msg}");
}

#[test]
fn a_told_peer_to_a_mintable_provider_is_rejected() {
    // config's EDGE port is minted (not known until the agent binds it), and
    // characters is TOLD config's edge address — a literal env value that cannot
    // carry a not-yet-bound port. Must fail closed; the fix is resolve = "asks".
    // config (position 0) is earlier than characters, so the boot-order rule
    // passes and the mintable-provider rule is the one that fires.
    let text = r#"
[[service]]
name = "config-svc"
pkg = "config-svc"
provider = "config"
http_port = 8083
edge_port = "mint"

[[service]]
name = "characters-svc"
pkg = "characters-svc"
provider = "characters"
http_port = 8080

[[service.peer]]
env_key = "CONFIG_EDGE_ADDR"
provider = "config"
kind = "edge"
"#;
    let err = validate(&parsed(text)).expect_err("a Told peer to a mintable provider must fail");
    let msg = chain(&err);
    assert!(msg.contains("CONFIG_EDGE_ADDR"), "names the offending Told peer key: {msg}");
    assert!(msg.contains("config"), "names the mintable provider: {msg}");
    assert!(msg.contains("mint"), "names the minted kind as the cause: {msg}");
    assert!(
        msg.contains("resolve = \"asks\""),
        "points at the fix (consume a mintable provider via asks): {msg}"
    );
}

#[test]
fn asks_consuming_a_mintable_provider_stays_legal() {
    // Same mintable config edge, but the consumer ASKS — its address is resolved
    // at boot over the agent (from PeerAddrs derived AFTER the mint pass), so the
    // not-yet-bound port is representable and the guard must NOT fire.
    let text = r#"
[[service]]
name = "config-svc"
pkg = "config-svc"
provider = "config"
http_port = 8083
edge_port = "mint"

[[service]]
name = "gateway-svc"
pkg = "gateway-svc"
provider = "gateway"
http_port = 8082
resolve = "asks"
"#;
    validate(&parsed(text)).expect("a mintable provider consumed via asks is legal");
}

#[test]
fn a_told_peer_to_a_providers_literal_kind_stays_legal_while_another_kind_mints() {
    // config mints its HTTP port but its EDGE port is a literal. A Told peer on
    // the LITERAL edge is fine — only the minted KIND is unrepresentable in a
    // Told env value, so the guard is per-(provider, kind), not per-provider.
    let text = r#"
[[service]]
name = "config-svc"
pkg = "config-svc"
provider = "config"
http_port = "mint"
edge_port = 9002

[[service]]
name = "characters-svc"
pkg = "characters-svc"
provider = "characters"
http_port = 8080

[[service.peer]]
env_key = "CONFIG_EDGE_ADDR"
provider = "config"
kind = "edge"
"#;
    validate(&parsed(text))
        .expect("a Told peer on a provider's LITERAL kind is legal even if another kind mints");
}

// ---------------------------------------------------------------------------
// `replicas` sugar + the fail-closed `replica_safe` future-guard (B2). weles is
// DOMAIN-BLIND: it carries no per-module "replica-safe" list; it enforces that
// the operator's `replica_safe = true` assertion EXISTS before fanning a service
// out to N instances. Distinct ports come from minting (`"mint"` per port
// field), never a guessed per-replica literal offset.
// ---------------------------------------------------------------------------

#[test]
fn replicas_without_replica_safe_fails_closed() {
    // The FUTURE-GUARD's branch: a service asks for two instances but does NOT
    // assert replica_safe. A new module with request-spanning in-memory state
    // must not silently inherit replicas — this fails closed at expansion.
    let text = r#"
[[service]]
name = "characters-svc"
pkg = "characters-svc"
provider = "characters"
http_port = "mint"
replicas = 2
"#;
    let err = parse(text).expect_err("replicas > 1 without replica_safe must fail closed");
    let msg = chain(&err);
    assert!(msg.contains("characters-svc"), "names the offending service: {msg}");
    assert!(msg.contains("replica_safe"), "names the missing assertion: {msg}");
    assert!(
        msg.contains("domain-blind"),
        "explains weles cannot audit the module itself: {msg}"
    );
}

#[test]
fn replicas_with_replica_safe_expands_to_distinct_minted_instances() {
    // The permitted branch: replica_safe asserted AND every port field "mint".
    // One [[service]] unfolds into TWO owned ServiceDefs with distinct #-suffixed
    // names, each carrying Port::Mint (the agent binds a distinct free port per
    // instance), and the whole fleet validates.
    let text = r#"
[[service]]
name = "characters-svc"
pkg = "characters-svc"
provider = "characters"
http_port = "mint"
edge_port = "mint"
replicas = 2
replica_safe = true
"#;
    let fleet = parsed(text);
    let chars: Vec<_> = fleet
        .services
        .iter()
        .filter(|s| s.provider.as_deref() == Some("characters"))
        .collect();
    assert_eq!(chars.len(), 2, "replicas = 2 expands to two ServiceDefs");
    let names: Vec<&str> = chars.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"characters-svc#1"), "distinct #1 name: {names:?}");
    assert!(names.contains(&"characters-svc#2"), "distinct #2 name: {names:?}");
    assert!(
        chars.iter().all(|s| s.http_port == Port::Mint && s.edge_port == Some(Port::Mint)),
        "each instance keeps its Mint ports so the agent binds distinct free ports"
    );
    validate(&fleet).expect("two minted replica instances must validate");
}

#[test]
fn a_told_peer_to_a_replicas_expanded_provider_is_rejected() {
    // The existing replicated-provider guard fires on the EXPANSION: replicas = 2
    // produces two same-provider defs, so a THIRD service Told that provider over
    // HTTP resolves to only the first replica and must fail. (HTTP so there is no
    // boot-order rule and the replicated-provider branch is the one that fires,
    // ahead of the mintable-provider check in validate's order.)
    let text = r#"
[[service]]
name = "characters-svc"
pkg = "characters-svc"
provider = "characters"
http_port = "mint"
replicas = 2
replica_safe = true

[[service]]
name = "consumer-svc"
pkg = "consumer-svc"
provider = "consumer"
http_port = 8082

[[service.peer]]
env_key = "CHARACTERS_HTTP_ADDR"
provider = "characters"
kind = "http"
"#;
    let err =
        validate(&parsed(text)).expect_err("a Told peer to a replicas-expanded provider must fail");
    let msg = chain(&err);
    assert!(msg.contains("consumer-svc"), "names the consumer: {msg}");
    assert!(msg.contains("CHARACTERS_HTTP_ADDR"), "names the peer env_key: {msg}");
    assert!(msg.contains('2'), "names the instance count from the expansion: {msg}");
    assert!(
        msg.contains("resolve=\"asks\""),
        "points at the fix for a replicated provider: {msg}"
    );
}

#[test]
fn replicas_unset_or_one_stays_a_single_def_with_no_flag() {
    // Absent replicas ⇒ one def, name untouched, no replica_safe needed.
    let unset = r#"
[[service]]
name = "characters-svc"
pkg = "characters-svc"
provider = "characters"
http_port = 8080
"#;
    let fleet = parsed(unset);
    assert_eq!(fleet.services.len(), 1, "absent replicas ⇒ one instance");
    assert_eq!(fleet.services[0].name, "characters-svc", "single instance keeps its bare name");
    validate(&fleet).expect("a single-instance service needs no replica_safe");

    // replicas = 1 is the same: single def, literal port fine (no minting forced),
    // no replica_safe assertion required.
    let one = r#"
[[service]]
name = "characters-svc"
pkg = "characters-svc"
provider = "characters"
http_port = 8080
replicas = 1
"#;
    let fleet = parsed(one);
    assert_eq!(fleet.services.len(), 1, "replicas = 1 ⇒ one instance");
    assert_eq!(fleet.services[0].name, "characters-svc", "no #-suffix for a single instance");
    assert_eq!(fleet.services[0].http_port, Port::Literal(8080), "literal port kept as authored");
    validate(&fleet).expect("replicas = 1 needs no replica_safe");
}

#[test]
fn replicas_with_a_literal_port_fails_closed() {
    // Sibling guard: replica_safe is asserted, but http_port is a LITERAL. weles
    // will NOT reuse or guess per-replica ports (anti-magic) — replicas need
    // minted ports, so this fails closed rather than colliding two instances on
    // one port.
    let text = r#"
[[service]]
name = "characters-svc"
pkg = "characters-svc"
provider = "characters"
http_port = 8080
replicas = 2
replica_safe = true
"#;
    let err = parse(text).expect_err("replicas on a literal port must fail closed");
    let msg = chain(&err);
    assert!(msg.contains("characters-svc"), "names the offending service: {msg}");
    assert!(msg.contains("http_port = \"mint\""), "points at minting as the fix: {msg}");
}

#[test]
fn replicas_with_a_player_port_fails_closed() {
    // A player-QUIC front is a single fixed public port (not mintable), so a
    // service serving one cannot be replicated — fail closed rather than collide.
    let text = r#"
[[service]]
name = "gateway-svc"
pkg = "gateway-svc"
provider = "gateway"
http_port = "mint"
player_port = 9100
replicas = 2
replica_safe = true
"#;
    let err = parse(text).expect_err("replicas with a player_port must fail closed");
    let msg = chain(&err);
    assert!(msg.contains("gateway-svc"), "names the offending service: {msg}");
    assert!(msg.contains("player_port"), "names the un-mintable fixed front port: {msg}");
}

#[test]
fn replicas_zero_is_rejected() {
    // replicas = 0 would run nothing — a loud error, never a silent no-op.
    let text = r#"
[[service]]
name = "characters-svc"
pkg = "characters-svc"
provider = "characters"
http_port = "mint"
replicas = 0
"#;
    let err = parse(text).expect_err("replicas = 0 must fail");
    let msg = chain(&err);
    assert!(msg.contains("characters-svc"), "names the offending service: {msg}");
    assert!(msg.contains("runs nothing"), "explains why zero is rejected: {msg}");
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
