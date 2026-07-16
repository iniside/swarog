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

/// The exclusion predicate is DERIVED from the allowlist, so it can only ever
/// exclude an ambient passthrough key — never a topology/wiring key. Pin that a
/// peer `*_EDGE_ADDR` is not excluded and an allowlist key is.
#[test]
fn exclusion_predicate_is_the_allowlist() {
    assert!(is_excluded("PATH"));
    assert!(is_excluded("windir")); // case-insensitive
    assert!(!is_excluded("CONFIG_EDGE_ADDR"));
    assert!(!is_excluded("DATABASE_POOL_MAX_CONNECTIONS"));
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
