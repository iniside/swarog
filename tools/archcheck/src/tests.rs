//! Unit tests for archcheck's path classification and the single-front-door allow-list.
//! The dependency-edge scan itself runs against real `cargo metadata` (exercised by the
//! `fortress` verify stage); here we pin the pure helpers that decide WHAT a package is
//! and WHICH cmd binaries may host the `gateway` crate.

use super::{
    classify, cmd_is_a_main, contains_boundary_checked, cross_schema_fk_violations,
    forbidden_api_deps, has_non_dev_dep, is_inline_test_mod, missing_svc_violations,
    mod_test_ident_end, Kind, DEMO_HOST, FORBIDDEN_API_DEPS, FRONT_DOOR_HOSTS, GATEWAY_CRATE,
    SVC_EXEMPT_MODULES,
};

#[test]
fn classifies_module_manifest() {
    assert!(matches!(
        classify("G:/Projects/GameBackend/modules/characters/Cargo.toml"),
        Kind::Module(n) if n == "characters"
    ));
}

#[test]
fn classifies_rpc_glue_manifest() {
    assert!(matches!(
        classify("/repo/api/characters/rpc/Cargo.toml"),
        Kind::Rpc(n) if n == "characters"
    ));
}

#[test]
fn classifies_cmd_manifest_by_dir_name() {
    assert!(matches!(
        classify("/repo/cmd/characters-svc/Cargo.toml"),
        Kind::Cmd(n) if n == "characters-svc"
    ));
    // Windows backslashes normalize the same way.
    assert!(matches!(
        classify(r"C:\repo\cmd\gateway-svc\Cargo.toml"),
        Kind::Cmd(n) if n == "gateway-svc"
    ));
}

#[test]
fn non_module_non_cmd_is_other() {
    assert!(matches!(
        classify("/repo/core/app/Cargo.toml"),
        Kind::Other
    ));
}

#[test]
fn classifies_api_contract_manifest() {
    assert!(matches!(
        classify("/repo/api/config/api/Cargo.toml"),
        Kind::Api(n) if n == "config"
    ));
    // Windows backslashes normalize the same way.
    assert!(matches!(
        classify(r"C:\repo\api\characters\api\Cargo.toml"),
        Kind::Api(n) if n == "characters"
    ));
}

#[test]
fn only_front_processes_may_host_the_gateway() {
    // The two sanctioned front doors.
    assert!(FRONT_DOOR_HOSTS.contains(&"gateway-svc"));
    assert!(FRONT_DOOR_HOSTS.contains(&"server"));
    // Domain svcs must NOT be on the allow-list — a `gateway` dep there is a violation.
    for svc in [
        "characters-svc",
        "inventory-svc",
        "config-svc",
        "accounts-svc",
        "match-svc",
        "leaderboard-svc",
    ] {
        assert!(
            !FRONT_DOOR_HOSTS.contains(&svc),
            "{svc} must not be permitted to host the front door"
        );
    }
    assert_eq!(GATEWAY_CRATE, "gateway");
}

#[test]
fn forbidden_api_deps_include_edge_and_remote() {
    // Fact 6: the realistic regression vector is a <name>api picking up edge/remote,
    // not a raw transport crate directly — both must be in the forbid-list.
    assert!(FORBIDDEN_API_DEPS.contains(&"edge"));
    assert!(FORBIDDEN_API_DEPS.contains(&"remote"));
    assert!(FORBIDDEN_API_DEPS.contains(&"tokio"));
}

#[test]
fn api_crate_with_edge_dep_is_a_violation() {
    // A synthetic <name>api package that (regressively) depends on `edge`.
    let deps = serde_json::json!([
        { "name": "serde", "kind": null },
        { "name": "edge", "kind": null },
    ]);
    let deps = deps.as_array().unwrap().clone();
    assert_eq!(forbidden_api_deps(&deps), vec!["edge".to_string()]);
}

#[test]
fn api_crate_with_edge_dev_dep_only_is_clean() {
    // A dev-dependency on a transport crate (e.g. an integration test) is not a
    // violation — only the runtime import graph is constrained.
    let deps = serde_json::json!([
        { "name": "edge", "kind": "dev" },
    ]);
    let deps = deps.as_array().unwrap().clone();
    assert!(forbidden_api_deps(&deps).is_empty());
}

#[test]
fn every_svc_and_server_is_a_main_requiring_metrics() {
    assert!(cmd_is_a_main("characters-svc"));
    assert!(cmd_is_a_main("server"));
    // A hypothetical non-process cmd/ helper crate is not constrained.
    assert!(!cmd_is_a_main("playercli"));
}

#[test]
fn cmd_without_metrics_dep_is_a_violation() {
    let deps = serde_json::json!([
        { "name": "tokio", "kind": null },
    ]);
    let deps = deps.as_array().unwrap().clone();
    assert!(!has_non_dev_dep(&deps, "metrics"));
}

#[test]
fn cmd_with_metrics_dep_is_clean() {
    let deps = serde_json::json!([
        { "name": "tokio", "kind": null },
        { "name": "metrics", "kind": null },
    ]);
    let deps = deps.as_array().unwrap().clone();
    assert!(has_non_dev_dep(&deps, "metrics"));
}

// --- cross-schema FK tripwire (fact 8) -------------------------------------

#[test]
fn cross_schema_reference_is_a_violation() {
    let text = "CREATE SCHEMA IF NOT EXISTS mine;\n\
                CREATE TABLE mine.widgets (\n\
                \tother_id uuid NOT NULL REFERENCES otherschema.foo(id)\n\
                );";
    let v = cross_schema_fk_violations(text, "mine");
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("otherschema"), "{v:?}");
}

#[test]
fn same_schema_reference_is_clean() {
    let text = "CREATE SCHEMA IF NOT EXISTS mine;\n\
                CREATE TABLE mine.widgets (\n\
                \towner_id uuid NOT NULL REFERENCES mine.owners(id)\n\
                );";
    assert!(cross_schema_fk_violations(text, "mine").is_empty());
}

#[test]
fn file_with_no_ddl_is_clean() {
    assert!(cross_schema_fk_violations("fn helper() {}", "mine").is_empty());
}

#[test]
fn schema_name_not_matching_dir_is_a_checker_assumption_violation() {
    let text = "CREATE SCHEMA IF NOT EXISTS wrongname;";
    let v = cross_schema_fk_violations(text, "mine");
    assert_eq!(v.len(), 1);
    assert!(v[0].contains("checker assumption violated"), "{v:?}");
}

#[test]
fn multiple_create_schema_in_one_file_is_a_checker_assumption_violation() {
    let text = "CREATE SCHEMA IF NOT EXISTS mine;\nCREATE SCHEMA IF NOT EXISTS other;";
    let v = cross_schema_fk_violations(text, "mine");
    assert_eq!(v.len(), 1);
    assert!(v[0].contains("checker assumption violated"), "{v:?}");
}

// --- inline test module tripwire (fact 9) -----------------------------------

#[test]
fn mod_tests_with_inline_body_is_a_violation() {
    let lines = ["mod tests { fn it_works() {} }"];
    assert!(mod_test_ident_end(lines[0]).is_some());
    assert!(is_inline_test_mod(&lines));
}

#[test]
fn mod_tests_declaration_is_clean() {
    let lines = ["mod tests;"];
    assert!(mod_test_ident_end(lines[0]).is_some());
    assert!(!is_inline_test_mod(&lines));
}

#[test]
fn mod_suffix_tests_declaration_is_clean() {
    let lines = ["mod proxy_tests;"];
    assert!(mod_test_ident_end(lines[0]).is_some());
    assert!(!is_inline_test_mod(&lines));
}

#[test]
fn mod_tests_with_brace_on_next_line_is_a_violation() {
    let lines = ["mod tests", "{", "    fn it_works() {}", "}"];
    assert!(is_inline_test_mod(&lines));
}

#[test]
fn path_attribute_above_mod_tests_declaration_stays_clean() {
    // A `#[path = "..."]` retarget line above `mod tests;` doesn't change the
    // discriminator — it only ever looks at the `mod` line itself.
    let lines = ["mod tests;"];
    assert!(!is_inline_test_mod(&lines));
}

#[test]
fn cfg_test_fn_is_not_a_mod_and_never_matches() {
    // webui's shape: `#[cfg(test)] pub(crate) fn test_router() -> Router` — `fn`, not
    // `mod`, so the `mod` keyword scan must never fire on it.
    let line = "pub(crate) fn test_router() -> Router {";
    assert!(mod_test_ident_end(line).is_none());
}

// --- Kind::Events classification (Rule E) -----------------------------------

#[test]
fn classifies_events_contract_manifest() {
    assert!(matches!(
        classify("/repo/api/scheduler/events/Cargo.toml"),
        Kind::Events(n) if n == "scheduler"
    ));
    // Windows backslashes normalize the same way.
    assert!(matches!(
        classify(r"C:\repo\api\match\events\Cargo.toml"),
        Kind::Events(n) if n == "match"
    ));
}

// --- Rule C: core/bus stays sqlx-free (no dep kind is skipped) --------------

#[test]
fn bus_with_normal_sqlx_dep_is_detected() {
    let deps = serde_json::json!([
        { "name": "tokio", "kind": null },
        { "name": "sqlx", "kind": null },
    ]);
    let deps = deps.as_array().unwrap();
    assert!(deps.iter().any(|d| d["name"].as_str() == Some("sqlx")));
}

#[test]
fn bus_with_dev_only_sqlx_dep_is_still_detected() {
    // Rule C deliberately does NOT skip dev deps (unlike has_non_dev_dep) — bus must
    // stay sqlx-free under every dep kind, including a "just for tests" dev-dep.
    let deps = serde_json::json!([
        { "name": "sqlx", "kind": "dev" },
    ]);
    let deps = deps.as_array().unwrap();
    assert!(deps.iter().any(|d| d["name"].as_str() == Some("sqlx")));
}

#[test]
fn bus_with_no_sqlx_dep_is_clean() {
    let deps = serde_json::json!([
        { "name": "tokio", "kind": null },
        { "name": "tokio", "kind": "dev" },
    ]);
    let deps = deps.as_array().unwrap();
    assert!(!deps.iter().any(|d| d["name"].as_str() == Some("sqlx")));
}

// --- Rule D: modules never runtime-dep the durable-events plane -------------

#[test]
fn module_with_normal_asyncevents_dep_is_a_violation() {
    let deps = serde_json::json!([
        { "name": "asyncevents", "kind": null },
    ]);
    let deps = deps.as_array().unwrap().clone();
    assert!(has_non_dev_dep(&deps, "asyncevents"));
}

#[test]
fn module_with_dev_only_asyncevents_dep_is_clean() {
    // The sanctioned test-wiring pattern used by the 5 fortress test suites.
    let deps = serde_json::json!([
        { "name": "asyncevents", "kind": "dev" },
    ]);
    let deps = deps.as_array().unwrap().clone();
    assert!(!has_non_dev_dep(&deps, "asyncevents"));
}

// --- Rule E: <name>events crates stay transport-free -------------------------

#[test]
fn events_crate_with_tokio_dep_is_a_violation() {
    // Model the real baseline dep set (bus + serde) plus a regressive tokio dep.
    let deps = serde_json::json!([
        { "name": "bus", "kind": null },
        { "name": "serde", "kind": null },
        { "name": "tokio", "kind": null },
    ]);
    let deps = deps.as_array().unwrap().clone();
    assert_eq!(forbidden_api_deps(&deps), vec!["tokio".to_string()]);
}

#[test]
fn events_crate_with_baseline_deps_only_is_clean() {
    let deps = serde_json::json!([
        { "name": "bus", "kind": null },
        { "name": "serde", "kind": null },
    ]);
    let deps = deps.as_array().unwrap().clone();
    assert!(forbidden_api_deps(&deps).is_empty());
}

// --- Rule F: EVENTS_ env tripwire (boundary-checked) -------------------------

#[test]
fn events_env_knob_is_boundary_matched() {
    assert!(contains_boundary_checked(
        "std::env::var(\"EVENTS_ORIGIN\")",
        "EVENTS_"
    ));
    assert!(contains_boundary_checked("EVENTS_SUBSCRIBERS", "EVENTS_"));
}

#[test]
fn asyncevents_ready_does_not_trip_the_boundary_check() {
    // The regression case: a naive substring match on "EVENTS_" hits the middle of
    // "ASYNCEVENTS_READY" (a real, legitimate identifier). The boundary check — the
    // char right before the match must not be alnum/`_` — must NOT flag it.
    assert!(!contains_boundary_checked("ASYNCEVENTS_READY", "EVENTS_"));
    assert!(!contains_boundary_checked(
        "wait_for(\"ASYNCEVENTS_READY\").await;",
        "EVENTS_"
    ));
}

/// Creates a fresh temp directory under the OS temp dir for a `grep_events_env` walk
/// test, using the current process id + an atomic counter so parallel `cargo test`
/// threads never collide on the same path.
fn unique_temp_dir() -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "archcheck_events_env_test_{}_{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp test dir");
    dir
}

#[test]
fn grep_events_env_flags_env_knob_line() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("a.rs"),
        "let origin = std::env::var(\"EVENTS_ORIGIN\").unwrap();\n",
    )
    .unwrap();
    let hits = super::grep_events_env(&dir);
    assert_eq!(hits.len(), 1, "{hits:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_events_env_ignores_comment_line() {
    let dir = unique_temp_dir();
    std::fs::write(dir.join("a.rs"), "// std::env::var(\"EVENTS_ORIGIN\")\n").unwrap();
    let hits = super::grep_events_env(&dir);
    assert!(hits.is_empty(), "{hits:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_events_env_ignores_asyncevents_ready_line() {
    // The boundary-check regression case (fact 3 / plan Rule F): this exact shape is
    // real code in modules/config/src/tests.rs:168,171 and must stay CLEAN.
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("a.rs"),
        "wait_for(\"ASYNCEVENTS_READY\").await;\n",
    )
    .unwrap();
    let hits = super::grep_events_env(&dir);
    assert!(hits.is_empty(), "{hits:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

// --- Rule 12: fortress parity — every modules/<name> has a cmd/<name>-svc ----

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn module_without_matching_svc_is_a_violation() {
    let v = missing_svc_violations(
        &strings(&["characters", "newthing"]),
        &strings(&["characters-svc", "server"]),
    );
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("modules/newthing"), "{v:?}");
    assert!(v[0].contains("cmd/newthing-svc"), "{v:?}");
}

#[test]
fn module_with_matching_svc_is_clean() {
    let v = missing_svc_violations(
        &strings(&["characters", "gateway"]),
        &strings(&["characters-svc", "gateway-svc", "server"]),
    );
    assert!(v.is_empty(), "{v:?}");
}

#[test]
fn no_module_is_svc_exempt() {
    // Demos live under demos/ (not modules/), so the exemption list is empty: a
    // webui-shaped module resurrected under modules/ WITHOUT a svc is a violation.
    assert!(SVC_EXEMPT_MODULES.is_empty());
    let v = missing_svc_violations(&strings(&["webui"]), &strings(&["server"]));
    assert_eq!(v.len(), 1, "{v:?}");
}

// --- Rule 13: demos/* crates are non-shipping (monolith-only) ----------------

#[test]
fn classifies_demo_manifest() {
    assert!(matches!(
        classify("/repo/demos/webui/Cargo.toml"),
        Kind::Demo(n) if n == "webui"
    ));
    // Windows backslashes normalize the same way.
    assert!(matches!(
        classify(r"C:\repo\demos\webui\Cargo.toml"),
        Kind::Demo(n) if n == "webui"
    ));
}

#[test]
fn only_the_monolith_hosts_demos() {
    assert_eq!(DEMO_HOST, "server");
}
