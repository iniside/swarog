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

/// The exclusion table only ever excludes ambient allowlist passthrough keys —
/// never a topology/wiring key. A regression that quietly excluded, say, a
/// peer `*_EDGE_ADDR` would blind the stage; pin that no excluded key is a
/// manifest-synthesized or peer-wiring key.
#[test]
fn exclusions_are_only_ambient_allowlist_keys() {
    for exclusion in ENV_EXCLUSIONS {
        assert!(
            weles::manifest::SERVICE_ENV_ALLOWLIST
                .iter()
                .any(|a| a.eq_ignore_ascii_case(exclusion.key)),
            "{} is excluded but is not a SERVICE_ENV_ALLOWLIST key — an exclusion must be an \
             ambient passthrough, never a topology decision",
            exclusion.key
        );
        assert!(
            !exclusion.reason.is_empty(),
            "{} excluded without a reason",
            exclusion.key
        );
    }
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
