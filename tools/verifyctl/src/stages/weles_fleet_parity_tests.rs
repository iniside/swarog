use super::*;

/// PASS-AT-HEAD: the real weles and processctl Development manifests are in
/// full parity right now. If this ever fails, the printed diff names the exact
/// drifting field — that IS the stage doing its job.
#[test]
fn head_fleets_are_in_parity() {
    let diffs = parity_diffs();
    assert!(
        diffs.is_empty(),
        "weles<->processctl fleet drift at HEAD:\n{}",
        diffs.join("\n")
    );
}

/// The KEY-ONLY exclusion is the allowlist and NOTHING else.
///
/// This predicate is service-blind — whatever it excludes is excluded for every
/// service in both topologies — so it is the one place a widening would be
/// invisible. It is DERIVED from the allowlist, so it can only ever exclude an
/// ambient passthrough key.
///
/// `ORCHESTRATOR_URL` is the pointed case: it IS excluded from the gateway's
/// diff, but NOT here. Exclusion 2 is per-service and keyed on
/// `weles::manifest::Addrs` ([`Delegation`]) — if it ever migrated into this
/// predicate it would silently apply to all twelve services, and this assertion
/// is what fires.
#[test]
fn the_key_only_exclusion_is_the_allowlist_and_nothing_else() {
    assert!(is_excluded("PATH"));
    assert!(is_excluded("windir")); // case-insensitive
    assert!(!is_excluded("CONFIG_EDGE_ADDR"));
    assert!(!is_excluded("DATABASE_POOL_MAX_CONNECTIONS"));
    assert!(
        !is_excluded(weles::manifest::ORCHESTRATOR_URL_ENV),
        "the delegation exclusion must stay per-service (Delegation), never become a \
         service-blind key exclusion"
    );
}

// ---------------------------------------------------------------------------
// Exclusion 2 (a managed process's delegated peer addresses) is NARROW.
//
// The whole risk of M1 Step 7: a widened green gate is worse than the red one it
// replaced. Each test below drifts something the exclusion must NOT cover and
// proves the stage still FAILs.
// ---------------------------------------------------------------------------

/// The exclusion is keyed on `Addrs::Asks` — NOT on a key list, and NOT on the
/// value being address-shaped. Prove it on the strongest counterexample
/// available: drop a peer address from an UNMANAGED service, whose value
/// (`127.0.0.1:9000`) is one the agent really does serve — so the value-keyed
/// arm would swallow it if the `Asks` gate were missing. inventory-svc is TOLD,
/// so this is drift and stays a FAIL.
#[test]
fn peer_env_drift_in_an_unmanaged_service_still_fails() {
    let mut weles = weles_split_views();
    let processctl = processctl_split_views();
    let deps = processctl_split_dependencies();

    let inventory = weles
        .iter_mut()
        .find(|v| v.name == "inventory-svc")
        .expect("inventory-svc present");
    assert_eq!(
        inventory.delegation,
        Delegation::TellAtSpawn,
        "fixture: inventory-svc is told its peers"
    );
    let dropped = inventory.env.remove("CHARACTERS_EDGE_ADDR");
    assert_eq!(dropped.as_deref(), Some("127.0.0.1:9000"), "fixture: a resolvable address");

    let diffs = diff_split(&weles, &processctl, &deps);
    assert!(
        diffs
            .iter()
            .any(|d| d.contains("inventory-svc") && d.contains("CHARACTERS_EDGE_ADDR")),
        "peer drift on an UNMANAGED service must still fail: {diffs:?}"
    );
}

/// A NON-address key on the managed service is not delegated, in EITHER
/// asymmetric direction — the only two arms a widening of exclusion 2 can reach.
///
/// Deliberately not phrased on `TLS_MODE`: both sides compose it, so a drift
/// there lands in the `(Some, Some)` arm, which no delegation widening can reach
/// — it would only discriminate a hypothetical "skip the whole managed service"
/// rewrite, and asserting it here would dress a vacuous check as a narrowness
/// proof. (`TLS_MODE` parity is held by `head_fleets_are_in_parity` and
/// `comparator_detects_env_drift`.)
#[test]
fn a_non_address_key_on_the_managed_service_still_fails() {
    let mut weles = weles_split_views();
    let mut processctl = processctl_split_views();
    let deps = processctl_split_dependencies();

    let gateway = weles.iter_mut().find(|v| v.name == "gateway-svc").expect("gateway-svc present");
    assert!(
        matches!(gateway.delegation, Delegation::AskTheAgent { .. }),
        "fixture: gateway-svc is the managed one"
    );
    // weles-only, and NOT the delegation's own key -> `explains_weles_only`.
    gateway.env.insert("WELES_ONLY_KEY".into(), "x".into());
    // processctl-only, and not a peer address key at all -> `claimed_peer` must
    // refuse to parse it rather than let the delegation swallow it.
    processctl
        .iter_mut()
        .find(|v| v.name == "gateway-svc")
        .expect("gateway-svc present")
        .env
        .insert("PROCESSCTL_ONLY_KEY".into(), "1".into());

    let diffs = diff_split(&weles, &processctl, &deps);
    assert!(
        diffs.iter().any(|d| d.contains("WELES_ONLY_KEY") && d.contains("absent in processctl")),
        "a weles-only key that is not ORCHESTRATOR_URL must still fail: {diffs:?}"
    );
    assert!(
        diffs.iter().any(|d| d.contains("PROCESSCTL_ONLY_KEY") && d.contains("absent in weles")),
        "a processctl-only key that names no (provider, kind) must still fail: {diffs:?}"
    );
}

/// THE COPY-PASTE CLASS: a processctl key pointing at ANOTHER service's real
/// address. Every value here is one the agent genuinely serves, so an
/// "is this any fleet address" arm — this exclusion's first cut — went green on
/// all of it. Keying on the key's own `(provider, kind)` is what closes it.
///
/// Both mispairs are drawn from the fleet's own addresses on purpose: that is
/// what makes them invisible to a value-only check, and it is a likelier drift
/// than the typo class (`…:9999`) that a value-only check does catch.
#[test]
fn a_key_value_mispair_among_the_fleets_own_addresses_still_fails() {
    let weles = weles_split_views();
    let deps = processctl_split_dependencies();

    for (key, value, whose) in [
        // characters' key, inventory's edge address.
        ("CHARACTERS_EDGE_ADDR", "127.0.0.1:9001", "inventory's edge"),
        // admin's passthrough origin, pointed at gateway's own http port.
        ("ADMIN_HTTP_ADDR", "127.0.0.1:8082", "gateway's own http port"),
        // the right provider, the WRONG kind: accounts serves both, so this is
        // the pair a provider-only (kind-blind) check would miss.
        ("ACCOUNTS_EDGE_ADDR", "127.0.0.1:8084", "accounts' http port, not its edge"),
    ] {
        let mut processctl = processctl_split_views();
        let gateway = processctl
            .iter_mut()
            .find(|v| v.name == "gateway-svc")
            .expect("gateway-svc present");
        assert!(gateway.env.contains_key(key), "fixture: processctl composes {key}");
        gateway.env.insert(key.into(), value.into());

        let diffs = diff_split(&weles, &processctl, &deps);
        assert!(
            diffs.iter().any(|d| d.contains("gateway-svc") && d.contains(key)),
            "{key} pointing at {whose} ({value}) is a resolvable address under the WRONG \
             (provider, kind) — it must still fail: {diffs:?}"
        );
    }
}

/// The exclusion is value-keyed against the agent's OWN resolve map, so it is
/// narrower than "skip the eight address keys": a processctl peer address
/// pointing at a port the agent does NOT serve is not a delegation, it is drift.
///
/// This is the branch a key-list exclusion would have gone green on.
#[test]
fn a_drifted_peer_address_on_the_managed_service_still_fails() {
    let weles = weles_split_views();
    let mut processctl = processctl_split_views();
    let deps = processctl_split_dependencies();

    processctl
        .iter_mut()
        .find(|v| v.name == "gateway-svc")
        .expect("gateway-svc present")
        .env
        .insert("CHARACTERS_EDGE_ADDR".into(), "127.0.0.1:9999".into());

    let diffs = diff_split(&weles, &processctl, &deps);
    assert!(
        diffs
            .iter()
            .any(|d| d.contains("gateway-svc") && d.contains("CHARACTERS_EDGE_ADDR")),
        "an address the agent could never resolve must still fail on the managed service: \
         {diffs:?}"
    );
}

/// The set FOLLOWS THE DATA: rebuild gateway-svc's view from a def that is
/// `Addrs::Told` instead of `Addrs::Asks`, and all eight address keys are
/// compared again.
///
/// Driven through `view_from_weles` from a real def rather than by poking the
/// view's field, so what is under test is the derivation FROM `Addrs` — the
/// authority the exclusion is keyed on. A hardcoded `"gateway-svc"` or a
/// hardcoded key list passes every other test in this file and fails this one.
#[test]
fn the_exclusion_evaporates_when_the_service_stops_asking() {
    let fleet = weles::manifest::split_fleet();
    let mut weles = weles_split_views();
    let processctl = processctl_split_views();
    let deps = processctl_split_dependencies();
    assert!(diff_split(&weles, &processctl, &deps).is_empty(), "control: HEAD must be clean");

    let told_gateway = weles::manifest::ServiceDef {
        addrs: weles::manifest::Addrs::Told(&[]),
        ..fleet.iter().find(|d| d.name == "gateway-svc").expect("gateway-svc present").clone()
    };
    let index = weles.iter().position(|v| v.name == "gateway-svc").expect("gateway-svc present");
    weles[index] = view_from_weles(&told_gateway, &fleet);
    assert_eq!(weles[index].delegation, Delegation::TellAtSpawn);

    let diffs = diff_split(&weles, &processctl, &deps);
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
            diffs.iter().any(|d| d.contains("gateway-svc") && d.contains(key)),
            "a gateway that no longer ASKS must have {key} compared again: {diffs:?}"
        );
    }
}

/// `ORCHESTRATOR_URL` is excluded for the process that ASKS — and for no other.
/// A service handed the agent's URL without being managed is exactly the
/// "unread value that drifts until someone believes it" state, and it FAILs.
#[test]
fn an_orchestrator_url_on_an_unmanaged_service_still_fails() {
    let mut weles = weles_split_views();
    let processctl = processctl_split_views();
    let deps = processctl_split_dependencies();

    let config = weles.iter_mut().find(|v| v.name == "config-svc").expect("config-svc present");
    assert_eq!(config.delegation, Delegation::TellAtSpawn, "fixture: config-svc is told");
    config
        .env
        .insert(weles::manifest::ORCHESTRATOR_URL_ENV.into(), weles::manifest::agent_url());

    let diffs = diff_split(&weles, &processctl, &deps);
    assert!(
        diffs.iter().any(|d| d.contains("config-svc") && d.contains("ORCHESTRATOR_URL")),
        "ORCHESTRATOR_URL on a service that does not ask must still fail: {diffs:?}"
    );
}

/// The excluded weles-only pair is key AND value: the delegation explains the
/// agent's real URL, not whatever string sits under that key.
#[test]
fn a_managed_service_with_a_wrong_agent_url_still_fails() {
    let mut weles = weles_split_views();
    let processctl = processctl_split_views();
    let deps = processctl_split_dependencies();

    weles
        .iter_mut()
        .find(|v| v.name == "gateway-svc")
        .expect("gateway-svc present")
        .env
        .insert(weles::manifest::ORCHESTRATOR_URL_ENV.into(), "http://127.0.0.1:1".into());

    let diffs = diff_split(&weles, &processctl, &deps);
    assert!(
        diffs.iter().any(|d| d.contains("gateway-svc") && d.contains("ORCHESTRATOR_URL")),
        "an ORCHESTRATOR_URL that is not the agent's URL must still fail: {diffs:?}"
    );
}

/// FAIL-ON-DRIFT (port): mutate one service's http_port in a copy of the real
/// weles views and prove the comparator reports it. A checker that cannot fail
/// on an injected port change is theater.
#[test]
fn comparator_detects_port_drift() {
    let mut weles = weles_split_views();
    let processctl = processctl_split_views();
    let deps = processctl_split_dependencies();
    assert!(diff_split(&weles, &processctl, &deps).is_empty(), "control: HEAD must be clean");

    weles[0].http_port += 1;
    let diffs = diff_split(&weles, &processctl, &deps);
    assert!(
        diffs.iter().any(|d| d.contains("http_port")),
        "port drift not detected: {diffs:?}"
    );
}

/// FAIL-ON-DRIFT (peer-wiring env): mutate a `*_EDGE_ADDR` value and prove the
/// full-env diff catches it — this is exactly the class the reviewer flagged a
/// 5-key subset would miss.
#[test]
fn comparator_detects_env_drift() {
    let mut weles = weles_split_views();
    let processctl = processctl_split_views();
    let deps = processctl_split_dependencies();

    let inventory = weles
        .iter_mut()
        .find(|v| v.name == "inventory-svc")
        .expect("inventory-svc present");
    inventory
        .env
        .insert("CONFIG_EDGE_ADDR".into(), "127.0.0.1:9999".into());

    let diffs = diff_split(&weles, &processctl, &deps);
    assert!(
        diffs.iter().any(|d| d.contains("CONFIG_EDGE_ADDR")),
        "peer-wiring env drift not detected: {diffs:?}"
    );
}

/// FAIL-ON-DRIFT (dropped env key): removing a dev-seed from weles must be
/// reported as absent, not silently tolerated.
#[test]
fn comparator_detects_dropped_env_key() {
    let mut weles = weles_split_views();
    let processctl = processctl_split_views();
    let deps = processctl_split_dependencies();

    weles
        .iter_mut()
        .find(|v| v.name == "apikeys-svc")
        .expect("apikeys-svc present")
        .env
        .remove("APIKEYS_DEV_SEED");

    let diffs = diff_split(&weles, &processctl, &deps);
    assert!(
        diffs
            .iter()
            .any(|d| d.contains("APIKEYS_DEV_SEED") && d.contains("absent in weles")),
        "dropped env key not detected: {diffs:?}"
    );
}

/// FAIL-ON-DRIFT (missing service): dropping a service from one side is
/// reported by the set diff.
#[test]
fn comparator_detects_missing_service() {
    let mut weles = weles_split_views();
    let processctl = processctl_split_views();
    let deps = processctl_split_dependencies();

    let dropped = weles.remove(0).name;
    let diffs = diff_split(&weles, &processctl, &deps);
    assert!(
        diffs
            .iter()
            .any(|d| d.contains(&dropped) && d.contains("not in weles")),
        "missing service not detected: {diffs:?}"
    );
}

/// FAIL-ON-DRIFT (boot order): reordering weles so a dependency boots AFTER its
/// dependent must be reported against processctl's dependency graph.
#[test]
fn comparator_detects_boot_order_violation() {
    let mut weles = weles_split_views();
    let processctl = processctl_split_views();
    let deps = processctl_split_dependencies();
    assert!(diff_split(&weles, &processctl, &deps).is_empty(), "control: HEAD must be clean");

    // characters-svc depends on config-svc; move config-svc to the very end so
    // it boots after its dependent.
    let config_index = weles
        .iter()
        .position(|v| v.name == "config-svc")
        .expect("config-svc present");
    let config = weles.remove(config_index);
    weles.push(config);

    let diffs = diff_split(&weles, &processctl, &deps);
    assert!(
        diffs
            .iter()
            .any(|d| d.contains("config-svc") && d.contains("must appear before")),
        "boot-order violation not detected: {diffs:?}"
    );
}

/// FAIL-ON-DRIFT (dedicated Postgres sessions): the hand-copied budget
/// arithmetic is compared per service. Bumping one side's `dedicated` (as a
/// processctl `AE_WORKERS`/plane change would) must be reported — this is the
/// twin the first cut left uncovered.
#[test]
fn comparator_detects_dedicated_drift() {
    let mut weles = weles_split_views();
    let processctl = processctl_split_views();
    let deps = processctl_split_dependencies();
    assert!(diff_split(&weles, &processctl, &deps).is_empty(), "control: HEAD must be clean");

    weles
        .iter_mut()
        .find(|v| v.name == "accounts-svc")
        .expect("accounts-svc present")
        .dedicated += 1;

    let diffs = diff_split(&weles, &processctl, &deps);
    assert!(
        diffs.iter().any(|d| d.contains("dedicated")),
        "dedicated-session drift not detected: {diffs:?}"
    );
}

/// FAIL-ON-DRIFT (env present in weles but absent in processctl): the
/// asymmetric direction of the env diff (the mirror of the dropped-key test).
#[test]
fn comparator_detects_weles_only_env_key() {
    let mut weles = weles_split_views();
    let processctl = processctl_split_views();
    let deps = processctl_split_dependencies();

    weles
        .iter_mut()
        .find(|v| v.name == "config-svc")
        .expect("config-svc present")
        .env
        .insert("WELES_ONLY_KEY".into(), "x".into());

    let diffs = diff_split(&weles, &processctl, &deps);
    assert!(
        diffs
            .iter()
            .any(|d| d.contains("WELES_ONLY_KEY") && d.contains("absent in processctl")),
        "weles-only env key not detected: {diffs:?}"
    );
}

/// PASS-AT-HEAD + FAIL-ON-DRIFT for the hand-copied `PG_SESSION_BUDGET`
/// constants. HEAD equal → no diff; a mutated pair → a named diff. Consts can't
/// be mutated in place, so the pure comparator is driven with explicit values.
#[test]
fn budget_constant_parity_and_drift() {
    assert!(budget_diffs(87, 87).is_empty());
    assert_eq!(
        weles::manifest::PG_SESSION_BUDGET,
        processctl::PG_SESSION_BUDGET,
        "hand-copied PG_SESSION_BUDGET drifted at HEAD"
    );
    let diffs = budget_diffs(87, 88);
    assert!(
        diffs.iter().any(|d| d.contains("PG_SESSION_BUDGET")),
        "budget drift not detected: {diffs:?}"
    );
}

/// PASS-AT-HEAD + FAIL-ON-DRIFT for the hand-copied `SERVICE_ENV_ALLOWLIST`
/// slices, in both directions (added / dropped key).
#[test]
fn allowlist_parity_and_drift() {
    assert!(
        allowlist_diffs(
            weles::manifest::SERVICE_ENV_ALLOWLIST,
            processctl::SERVICE_ENV_ALLOWLIST,
        )
        .is_empty(),
        "hand-copied SERVICE_ENV_ALLOWLIST drifted at HEAD"
    );
    let added = allowlist_diffs(&["PATH", "APPDATA"], &["PATH"]);
    assert!(
        added.iter().any(|d| d.contains("APPDATA") && d.contains("not processctl")),
        "allowlist add not detected: {added:?}"
    );
    let dropped = allowlist_diffs(&["PATH"], &["PATH", "WINDIR"]);
    assert!(
        dropped.iter().any(|d| d.contains("WINDIR") && d.contains("not weles")),
        "allowlist drop not detected: {dropped:?}"
    );
}

/// The monolith's package identity is compared even though its display name is
/// deliberately not: prove a pkg mismatch on the monolith IS caught.
#[test]
fn monolith_pkg_mismatch_is_caught_name_is_not() {
    let mut weles = weles_monolith_view();
    let processctl = processctl_monolith_view();

    // Names legitimately differ at HEAD; with compare_name=false that's clean.
    assert!(diff_view("monolith", &weles, &processctl, false).is_empty());

    weles.pkg = "not-server".into();
    let diffs = diff_view("monolith", &weles, &processctl, false);
    assert!(
        diffs.iter().any(|d| d.contains("pkg")),
        "monolith pkg mismatch not detected: {diffs:?}"
    );
}
