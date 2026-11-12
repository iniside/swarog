//! Unit tests for archcheck's path classification and the single-front-door allow-list.
//! The dependency-edge scan itself runs against real `cargo metadata` (exercised by the
//! `fortress` verify stage); here we pin the pure helpers that decide WHAT a package is
//! and WHICH cmd binaries may host the `gateway` crate.

use super::{
    classify, cmd_is_a_main, contains_boundary_checked, core_bus_sqlx_violations,
    cross_schema_fk_violations, forbidden_api_deps, has_non_dev_dep, http_op_domains,
    is_inline_test_mod, locking_ddl_excluded, locking_ddl_violations, missing_exclusion_violations,
    missing_svc_violations, mod_test_ident_end, module_import_violations,
    Kind, DEMO_HOST, FORBIDDEN_API_DEPS, FRONT_DOOR_HOSTS, GATEWAY_CRATE, LOCKING_DDL_EXCLUDE,
    comment_spans, grep_locking_ddl, missing_scope_violations, skip_literal_or_comment,
    DDL_CREATE_PHRASES, LOCKING_DDL_SCOPE, SVC_EXEMPT_MODULES, WORKSPACE_SRC_DIRS,
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
    // A tool crate (not under modules/cmd/demos/api/core) falls through to Kind::Other.
    assert!(matches!(
        classify("/repo/tools/rpc-macro/Cargo.toml"),
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
    assert!(FORBIDDEN_API_DEPS.contains(&"push"));
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
//
// These call the REAL gate (`core_bus_sqlx_violations`) against a fixture package
// list, not a hand-built deps array — the previous version of these tests asserted
// on serde_json filtering directly and would pass whether or not the gate's own
// `Kind` filter was broken (it was: classify() returns `Kind::Core("bus")` for
// core/bus/Cargo.toml, but the gate required `Kind::Other`, so the loop never ran
// against the real package — dead since 70674db).

#[test]
fn bus_depending_on_sqlx_is_flagged_by_the_real_scan() {
    let packages = serde_json::json!([
        {
            "name": "bus",
            "manifest_path": "/repo/core/bus/Cargo.toml",
            "dependencies": [
                { "name": "sqlx", "kind": null },
            ],
        },
    ]);
    let packages = packages.as_array().unwrap();
    let violations = core_bus_sqlx_violations(packages);
    assert_eq!(violations.len(), 1, "violations: {violations:?}");
}

#[test]
fn bus_depending_on_sqlx_via_windows_manifest_path_is_still_flagged() {
    // Confirms classify()'s backslash normalization ('\\' -> '/') holds inside the
    // real gate, not just in isolated classify() unit tests.
    let packages = serde_json::json!([
        {
            "name": "bus",
            "manifest_path": r"C:\repo\core\bus\Cargo.toml",
            "dependencies": [
                { "name": "sqlx", "kind": "dev" },
            ],
        },
    ]);
    let packages = packages.as_array().unwrap();
    let violations = core_bus_sqlx_violations(packages);
    assert_eq!(violations.len(), 1, "violations: {violations:?}");
}

#[test]
fn bus_without_sqlx_is_clean_via_the_real_scan() {
    let packages = serde_json::json!([
        {
            "name": "bus",
            "manifest_path": "/repo/core/bus/Cargo.toml",
            "dependencies": [
                { "name": "tokio", "kind": null },
            ],
        },
    ]);
    let packages = packages.as_array().unwrap();
    assert!(core_bus_sqlx_violations(packages).is_empty());
}

#[test]
fn non_bus_core_crate_with_sqlx_is_not_flagged() {
    // The rule is scoped to core/bus specifically, not every core/* crate. A clean
    // bus package is included so the missing-target tripwire stays quiet here.
    let packages = serde_json::json!([
        {
            "name": "asyncevents",
            "manifest_path": "/repo/core/asyncevents/Cargo.toml",
            "dependencies": [
                { "name": "sqlx", "kind": null },
            ],
        },
        {
            "name": "bus",
            "manifest_path": "/repo/core/bus/Cargo.toml",
            "dependencies": [
                { "name": "tokio", "kind": null },
            ],
        },
    ]);
    let packages = packages.as_array().unwrap();
    assert!(core_bus_sqlx_violations(packages).is_empty());
}

#[test]
fn missing_core_bus_target_is_itself_a_violation() {
    // Rename/move tripwire: if NO package matches (name `bus` classified
    // Kind::Core("bus")), the rule must fail loudly instead of silently scanning
    // nothing — the same silent-skip class as the original Kind::Other regression.
    let packages = serde_json::json!([
        {
            "name": "core-bus", // renamed — no longer matches the rule's target
            "manifest_path": "/repo/core/bus/Cargo.toml",
            "dependencies": [
                { "name": "sqlx", "kind": null },
            ],
        },
    ]);
    let packages = packages.as_array().unwrap();
    let violations = core_bus_sqlx_violations(packages);
    assert_eq!(violations.len(), 1, "violations: {violations:?}");
    assert!(violations[0].contains("found no core/bus package"));
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

// --- Rule 14a: retired push-plane tokens banned in workspace source ----------

#[test]
fn retired_tokens_cover_the_env_graph_and_route() {
    // The three tokens the cutover retired: two env knobs + the exact quoted route.
    assert!(super::RETIRED_EVENT_TOKENS.contains(&"EVENTS_SUBSCRIBERS"));
    assert!(super::RETIRED_EVENT_TOKENS.contains(&"EVENTS_ORIGIN"));
    assert!(super::RETIRED_EVENT_TOKENS.contains(&"\"/events\""));
}

#[test]
fn grep_retired_tokens_flags_a_comment_line() {
    // The ban scans comments too (unlike grep_events_env): a doc comment naming
    // EVENTS_SUBSCRIBERS documents delivery machinery that no longer exists.
    let root = unique_temp_dir();
    std::fs::create_dir_all(root.join("core/outbox/src")).unwrap();
    std::fs::write(
        root.join("core/outbox/src/lib.rs"),
        "/// Parses the `EVENTS_SUBSCRIBERS` env value.\n",
    )
    .unwrap();
    let hits = super::grep_retired_event_tokens(&root);
    assert_eq!(hits.len(), 1, "{hits:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn grep_retired_tokens_quoted_route_only_no_false_positive_on_url() {
    // `http://b/events` must NOT trip: only the exact quoted `"/events"` form is banned.
    let root = unique_temp_dir();
    std::fs::create_dir_all(root.join("core/x/src")).unwrap();
    std::fs::write(
        root.join("core/x/src/lib.rs"),
        "let url = \"http://b/events\"; // a peer URL, not the route\n",
    )
    .unwrap();
    assert!(super::grep_retired_event_tokens(&root).is_empty());
    // …but the exact quoted route IS flagged.
    std::fs::write(root.join("core/x/src/lib.rs"), "router.route(\"/events\", h);\n").unwrap();
    assert_eq!(super::grep_retired_event_tokens(&root).len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn grep_retired_tokens_excludes_the_archcheck_crate() {
    // archcheck's own source names the tokens it bans — it must never flag itself.
    let root = unique_temp_dir();
    std::fs::create_dir_all(root.join("tools/archcheck/src")).unwrap();
    std::fs::write(
        root.join("tools/archcheck/src/main.rs"),
        "const T: &[&str] = &[\"EVENTS_SUBSCRIBERS\", \"EVENTS_ORIGIN\"];\n",
    )
    .unwrap();
    assert!(super::grep_retired_event_tokens(&root).is_empty(), "self-exclusion");
    let _ = std::fs::remove_dir_all(&root);
}

// --- Rule 14b: schema-qualified asyncevents.<table> access banned ------------

#[test]
fn allowlisted_plane_function_calls_are_clean() {
    assert!(super::forbidden_asyncevents_refs(
        "PERFORM asyncevents.append_event('config.changed', 1, _payload);"
    )
    .is_empty());
    assert!(super::forbidden_asyncevents_refs(
        "SELECT asyncevents.ensure_history_contract($1, $2, $3, $4)"
    )
    .is_empty());
}

#[test]
fn direct_plane_table_access_is_flagged() {
    let refs = super::forbidden_asyncevents_refs(
        "INSERT INTO asyncevents.history_contracts (topic) VALUES ($1)",
    );
    assert_eq!(refs, vec!["history_contracts".to_string()]);
    let refs = super::forbidden_asyncevents_refs("SELECT * FROM asyncevents.events");
    assert_eq!(refs, vec!["events".to_string()]);
}

#[test]
fn asyncevents_rust_path_is_not_a_sql_ref() {
    // A Rust path uses `asyncevents::`, never `asyncevents.` — the dot is what marks SQL,
    // so a `use asyncevents::store;` or `crate::asyncevents::…` never matches.
    assert!(super::forbidden_asyncevents_refs("use asyncevents::store::append;").is_empty());
    // A longer identifier ending in `asyncevents` is boundary-rejected on the left.
    assert!(super::forbidden_asyncevents_refs("myasyncevents.foo").is_empty());
}

#[test]
fn is_test_source_recognizes_the_sanctioned_homes() {
    assert!(super::is_test_source("modules/config/src/tests.rs"));
    assert!(super::is_test_source("core/asyncevents/src/store_tests.rs"));
    assert!(super::is_test_source("modules/x/tests/integration.rs"));
    assert!(!super::is_test_source("modules/config/src/lib.rs"));
}

#[test]
fn grep_asyncevents_sql_skips_comments_and_flags_code() {
    let root = unique_temp_dir();
    std::fs::create_dir_all(root.join("modules/x/src")).unwrap();
    // A comment mentioning the table is fine; a code line SELECTing it is not.
    std::fs::write(
        root.join("modules/x/src/lib.rs"),
        "// seeds asyncevents.history_contracts via the plane function\n\
         let q = \"SELECT * FROM asyncevents.subscriptions\";\n",
    )
    .unwrap();
    let hits = super::grep_asyncevents_sql(&root);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].contains("subscriptions"), "{hits:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn grep_asyncevents_sql_excludes_plane_crate_and_tests() {
    let root = unique_temp_dir();
    // core/asyncevents (the plane) and a test file both SELECT plane tables — both clean.
    std::fs::create_dir_all(root.join("core/asyncevents/src")).unwrap();
    std::fs::create_dir_all(root.join("modules/x/src")).unwrap();
    std::fs::write(
        root.join("core/asyncevents/src/store.rs"),
        "sqlx::query(\"SELECT * FROM asyncevents.events\");\n",
    )
    .unwrap();
    std::fs::write(
        root.join("modules/x/src/tests.rs"),
        "sqlx::query(\"DELETE FROM asyncevents.events\");\n",
    )
    .unwrap();
    assert!(super::grep_asyncevents_sql(&root).is_empty(), "plane + tests exempt");
    let _ = std::fs::remove_dir_all(&root);
}

// --- Rule 15: a module may not query a FOREIGN module's schema in SQL --------

/// The 10 persisting-module schema names, as `grep_foreign_schema_sql` derives them from
/// the `modules/` dir scan (gateway owns none but still appears as a dir name).
fn schema_set() -> Vec<String> {
    strings(&[
        "accounts",
        "characters",
        "inventory",
        "config",
        "audit",
        "scheduler",
        "match",
        "rating",
        "leaderboard",
        "apikeys",
    ])
}

#[test]
fn foreign_schema_after_from_join_into_update_is_flagged() {
    let s = schema_set();
    // FROM inventory.items inside a `characters` module file.
    assert_eq!(
        super::foreign_schema_sql_refs("SELECT * FROM inventory.items", "characters", &s),
        vec!["inventory".to_string()]
    );
    // INSERT INTO rating.ratings (own = match).
    assert_eq!(
        super::foreign_schema_sql_refs(
            "INSERT INTO rating.ratings (player, mmr) VALUES ($1, $2)",
            "match",
            &s
        ),
        vec!["rating".to_string()]
    );
    // UPDATE config.settings.
    assert_eq!(
        super::foreign_schema_sql_refs("UPDATE config.settings SET v = $1", "audit", &s),
        vec!["config".to_string()]
    );
    // EXISTS (SELECT 1 FROM apikeys.keys …) — caught by the inner FROM.
    assert_eq!(
        super::foreign_schema_sql_refs(
            "WHERE EXISTS (SELECT 1 FROM apikeys.keys WHERE key = $1)",
            "characters",
            &s
        ),
        vec!["apikeys".to_string()]
    );
}

#[test]
fn own_schema_reference_is_clean() {
    let s = schema_set();
    // A module querying its OWN schema (`characters.characters`) is fine.
    assert!(super::foreign_schema_sql_refs(
        "SELECT id FROM characters.characters WHERE owner = $1",
        "characters",
        &s
    )
    .is_empty());
}

#[test]
fn topic_literal_and_method_id_are_not_sql_refs() {
    let s = schema_set();
    // A topic literal passed to append_event has no preceding SQL keyword.
    assert!(super::foreign_schema_sql_refs(
        "PERFORM asyncevents.append_event('config.changed', 1, _payload);",
        "audit",
        &s
    )
    .is_empty());
    // A method-id policy string ("accounts.login,characters.create") likewise.
    assert!(super::foreign_schema_sql_refs(
        "let policy = \"accounts.login,characters.create\";",
        "apikeys",
        &s
    )
    .is_empty());
}

#[test]
fn foreign_schema_split_across_lines_escapes_the_line_scoped_rule() {
    // DECLARED LIMITATION: the scan is line-scoped. With the keyword on one line and the
    // schema token on the next, NEITHER line trips — documenting that a multi-line split
    // escapes this tripwire (drift coverage, not exhaustive).
    let s = schema_set();
    assert!(super::foreign_schema_sql_refs("SELECT * FROM", "characters", &s).is_empty());
    assert!(super::foreign_schema_sql_refs("  inventory.items", "characters", &s).is_empty());
}

// --- Rule 16: core purity — foundations never dep a module or api/ crate -----

#[test]
fn classifies_core_manifest() {
    assert!(matches!(
        classify("/repo/core/app/Cargo.toml"),
        Kind::Core(n) if n == "app"
    ));
    // Windows backslashes normalize the same way.
    assert!(matches!(
        classify(r"C:\repo\core\bus\Cargo.toml"),
        Kind::Core(n) if n == "bus"
    ));
}

#[test]
fn non_core_paths_do_not_classify_as_core() {
    // A module path must still win over the later /core/ check even if it contained one.
    assert!(matches!(
        classify("/repo/modules/characters/Cargo.toml"),
        Kind::Module(_)
    ));
}

// --- Rule 12 (G2 leg): svc lib.rs must construct its module ------------------

#[test]
fn svc_lib_referencing_its_module_is_clean() {
    let dir = unique_temp_dir();
    let lib = dir.join("lib.rs");
    std::fs::write(
        &lib,
        "pub fn modules() -> Vec<Box<dyn Module>> {\n\
         \tvec![Box::new(characters::Characters::new())]\n}\n",
    )
    .unwrap();
    assert!(super::svc_lib_references_module(&lib, "characters"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn svc_lib_without_its_module_token_is_flagged() {
    let dir = unique_temp_dir();
    let lib = dir.join("lib.rs");
    // Constructs metrics but never `characters::` — the tripwire fires.
    std::fs::write(
        &lib,
        "pub fn modules() -> Vec<Box<dyn Module>> {\n\
         \tvec![Box::new(metrics::Metrics::new())]\n}\n",
    )
    .unwrap();
    assert!(!super::svc_lib_references_module(&lib, "characters"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn svc_lib_module_token_only_in_comment_is_flagged() {
    let dir = unique_temp_dir();
    let lib = dir.join("lib.rs");
    // A comment naming `characters::` is not construction — comment lines are skipped.
    std::fs::write(&lib, "// dials characters::Characters over the edge\n").unwrap();
    assert!(!super::svc_lib_references_module(&lib, "characters"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn svc_lib_module_token_is_left_boundary_checked() {
    let dir = unique_temp_dir();
    let lib = dir.join("lib.rs");
    // `mycharacters::` must NOT satisfy the `characters::` token (left-boundary check).
    std::fs::write(&lib, "vec![Box::new(mycharacters::Thing::new())]\n").unwrap();
    assert!(!super::svc_lib_references_module(&lib, "characters"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn svc_lib_missing_file_is_flagged() {
    let dir = unique_temp_dir();
    let lib = dir.join("does-not-exist.rs");
    assert!(!super::svc_lib_references_module(&lib, "characters"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_foreign_schema_sql_flags_code_and_skips_comments_and_tests() {
    let root = unique_temp_dir();
    // Two module dirs so the schema scan sees both `characters` and `inventory`.
    std::fs::create_dir_all(root.join("modules/characters/src")).unwrap();
    std::fs::create_dir_all(root.join("modules/inventory/src")).unwrap();
    std::fs::write(root.join("modules/characters/Cargo.toml"), "[package]\n").unwrap();
    std::fs::write(root.join("modules/inventory/Cargo.toml"), "[package]\n").unwrap();
    // A code line querying inventory's schema is a violation; a comment naming it (even
    // with a topic-shaped token) is not.
    std::fs::write(
        root.join("modules/characters/src/lib.rs"),
        "// match.finished then FROM inventory.items — prose, not code\n\
         let q = \"SELECT qty FROM inventory.items WHERE owner = $1\";\n",
    )
    .unwrap();
    // A test source may build cross-schema fixtures — excluded.
    std::fs::write(
        root.join("modules/characters/src/tests.rs"),
        "let q = \"DELETE FROM inventory.items\";\n",
    )
    .unwrap();
    let hits = super::grep_foreign_schema_sql(&root);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].contains("inventory"), "{hits:?}");
    assert!(hits[0].contains(":2:"), "flags the code line, not the comment: {hits:?}");
    let _ = std::fs::remove_dir_all(&root);
}

// --- Rule 17: gateway stub coverage — every #[http( domain stubbed in gateway-svc ---

#[test]
fn gateway_stubs_domain_matches_multiline_new() {
    // rustfmt puts the stub name on the line AFTER `Stub::new(` — the check must still
    // see it across the newline.
    let text = "Box::new(remote::Stub::new(\n    \"characters\",\n    &peer,\n));";
    assert!(super::gateway_stubs_domain(text, "characters"));
    // A different domain is not stubbed, and a prefix isn't a match.
    assert!(!super::gateway_stubs_domain(text, "inventory"));
    assert!(!super::gateway_stubs_domain(text, "char"));
}

#[test]
fn gateway_stubs_domain_matches_single_line() {
    let text = "remote::Stub::new(\"match\", &peer, f)";
    assert!(super::gateway_stubs_domain(text, "match"));
}

#[test]
fn gateway_stubs_domain_matches_describe_peer_constructor() {
    // The D2 peer-only stub (`Stub::describe_peer`, no factories) is equally valid PEER_SLOT
    // coverage — it contributes exactly the peer entry the describe fetch iterates. Both
    // single-line and rustfmt's multiline (name on the next line) forms must match.
    assert!(super::gateway_stubs_domain(
        "remote::Stub::describe_peer(\"characters\", &peer)",
        "characters"
    ));
    let multiline = "Box::new(remote::Stub::describe_peer(\n    \"match\",\n    &peer,\n));";
    assert!(super::gateway_stubs_domain(multiline, "match"));
    // A domain stubbed ONLY via describe_peer is clean (no violation).
    assert!(super::gateway_stub_coverage_violations(&strings(&["match"]), multiline).is_empty());
}

#[test]
fn missing_gateway_stub_is_a_violation() {
    // `match` exposes HTTP ops but is not stubbed — one violation naming it + the fix path.
    let v = super::gateway_stub_coverage_violations(
        &strings(&["characters", "match"]),
        "remote::Stub::new(\"characters\", &p, f);",
    );
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("`match`"), "{v:?}");
    assert!(v[0].contains("cmd/gateway-svc/src/lib.rs"), "{v:?}");
}

#[test]
fn all_http_domains_stubbed_is_clean() {
    // Extra stubs (apikeys) are fine — only a MISSING http domain is a gap.
    let text = "Stub::new(\"characters\", ..); Stub::new(\"match\", ..); Stub::new(\"apikeys\", ..);";
    assert!(
        super::gateway_stub_coverage_violations(&strings(&["characters", "match"]), text)
            .is_empty()
    );
}

#[test]
fn http_op_domains_scans_api_dirs_and_skips_comments() {
    let root = unique_temp_dir();
    for domain in ["characters", "rating"] {
        std::fs::create_dir_all(root.join("api").join(domain).join("api/src")).unwrap();
        std::fs::create_dir_all(root.join("modules").join(domain)).unwrap();
        std::fs::write(root.join("modules").join(domain).join("Cargo.toml"), "").unwrap();
    }
    // characters exposes an #[http( op; rating is wire-only — the ONLY `#[http(` there is
    // in a comment, which must be skipped.
    std::fs::write(
        root.join("api/characters/api/src/lib.rs"),
        "#[rpc]\ntrait Characters {\n    #[http(post, \"/create\")]\n    fn create();\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("api/rating/api/src/lib.rs"),
        "// wire-only; no #[http( ops here\n#[rpc]\ntrait Mmr { fn get(); }\n",
    )
    .unwrap();
    let (domains, errors) = super::http_op_domains(&root);
    assert_eq!(domains, vec!["characters".to_string()], "{domains:?}");
    assert!(errors.is_empty(), "{errors:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// The previously-wrong branch: a `#[http(` method that lives in a contract source OTHER
/// than `lib.rs`. A lib.rs-only scan reported zero HTTP domains for it, so rule 17 matched
/// nothing and passed green over a domain unreachable in the split. Test sources
/// (`tests.rs`, `*_tests.rs`) stay excluded — `contract_sources` cannot tell a fixture
/// `#[http(` from a real one.
#[test]
fn http_op_domains_sees_a_non_lib_contract_source_but_not_a_test_file() {
    let root = unique_temp_dir();
    for domain in ["foo", "bar"] {
        std::fs::create_dir_all(root.join("api").join(domain).join("api/src")).unwrap();
        std::fs::create_dir_all(root.join("modules").join(domain)).unwrap();
        std::fs::write(root.join("modules").join(domain).join("Cargo.toml"), "").unwrap();
    }
    std::fs::write(root.join("api/foo/api/src/lib.rs"), "pub mod ops;\n").unwrap();
    std::fs::write(
        root.join("api/foo/api/src/ops.rs"),
        "#[rpc]\ntrait Foo {\n    #[http(post, \"/foo\")]\n    fn go();\n}\n",
    )
    .unwrap();
    std::fs::write(root.join("api/bar/api/src/lib.rs"), "#[rpc]\ntrait Bar { fn go(); }\n")
        .unwrap();
    std::fs::write(
        root.join("api/bar/api/src/tests.rs"),
        "#[http(post, \"/fixture\")]\nfn fixture() {}\n",
    )
    .unwrap();
    let (domains, errors) = super::http_op_domains(&root);
    assert_eq!(domains, vec!["foo".to_string()], "{domains:?}");
    assert!(errors.is_empty(), "{errors:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// An unreadable api root is an ERROR line, not a vacuously clean scan.
#[test]
fn http_op_domains_reports_an_unreadable_api_root() {
    let root = unique_temp_dir();
    std::fs::create_dir_all(root.join("modules/served")).unwrap();
    std::fs::write(root.join("modules/served/Cargo.toml"), "").unwrap();
    let (domains, errors) = super::http_op_domains(&root);
    assert!(domains.is_empty(), "{domains:?}");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("cannot read"), "{errors:?}");
    let _ = std::fs::remove_dir_all(&root);
}

// --- Rule 18: conformancecheck stays out of shipping dependency graphs --------

#[test]
fn shipping_roots_reject_normal_and_build_paths_to_conformancecheck_only() {
    let packages = serde_json::json!([
        {
            "name": "server",
            "manifest_path": "G:/repo/cmd/server/Cargo.toml",
            "dependencies": [{ "name": "accounts", "kind": null }]
        },
        {
            "name": "accounts-svc",
            "manifest_path": "G:/repo/cmd/accounts-svc/Cargo.toml",
            "dependencies": [{ "name": "conformancecheck", "kind": "dev" }]
        },
        {
            "name": "gateway-svc",
            "manifest_path": "G:/repo/cmd/gateway-svc/Cargo.toml",
            "dependencies": [{ "name": "gateway-build", "kind": "build" }]
        },
        {
            "name": "accounts",
            "manifest_path": "G:/repo/modules/accounts/Cargo.toml",
            "dependencies": [{ "name": "conformancecheck", "kind": null }]
        },
        {
            "name": "gateway-build",
            "manifest_path": "G:/repo/tools/gateway-build/Cargo.toml",
            "dependencies": [{ "name": "conformancecheck", "kind": null }]
        },
        {
            "name": "conformancecheck",
            "manifest_path": "G:/repo/tools/conformance/Cargo.toml",
            "dependencies": [{ "name": "gateway", "kind": null }]
        },
        {
            "name": "gateway",
            "manifest_path": "G:/repo/modules/gateway/Cargo.toml",
            "dependencies": []
        }
    ]);
    let findings = super::shipping_conformance_violations(packages.as_array().unwrap());
    assert_eq!(findings.len(), 2, "{findings:?}");
    assert!(
        findings
            .iter()
            .any(|finding| finding.contains("server -> accounts -> conformancecheck")),
        "{findings:?}"
    );
    assert!(
        findings.iter().any(|finding| finding
            .contains("gateway-svc -> gateway-build -> conformancecheck")),
        "{findings:?}"
    );
    assert!(findings.iter().all(|finding| !finding.contains("accounts-svc")));
}

// --- Rule 19: only owner files construct canonical typed slots ----------------

#[test]
fn module_slot_constructor_is_a_violation() {
    let findings = super::slot_constructor_violations(
        "modules/inventory/src/lib.rs",
        "const BAD: contrib::Slot<u32> = contrib::Slot::new(\"bad\");",
    );
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].contains("modules/inventory/src/lib.rs:1"));
}

#[test]
fn qualified_slot_constructor_remains_a_workspace_wide_violation() {
    let findings = super::slot_constructor_violations(
        "core/bus/src/tests.rs",
        "const BAD: contrib::Slot<u32> = contrib::Slot::new(\"bad\");",
    );
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].contains("core/bus/src/tests.rs:1"));
}

#[test]
fn imported_slot_constructor_is_a_violation_in_modules_and_cmd() {
    for rel in ["modules/inventory/src/lib.rs", "cmd/server/src/lib.rs"] {
        let findings = super::slot_constructor_violations(
            rel,
            "use contrib::Slot;\nconst BAD: Slot<u32> = Slot::new(\"bad\");",
        );
        assert_eq!(findings.len(), 1, "{rel}: {findings:?}");
        assert!(findings[0].contains(&format!("{rel}:2")), "{findings:?}");
    }
}

#[test]
fn contribs_own_bare_constructor_test_seam_is_not_flagged() {
    let findings = super::slot_constructor_violations(
        "core/contrib/src/tests.rs",
        "const TEST_SLOT: Slot<u32> = Slot::new(\"test\");",
    );
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn every_canonical_slot_owner_may_construct_slots() {
    assert_eq!(super::SLOT_OWNER_FILES.len(), 7);
    for owner in super::SLOT_OWNER_FILES {
        let findings = super::slot_constructor_violations(
            owner,
            "use contrib::Slot;\npub const SLOT: Slot<u32> = Slot::new(\"owned\");",
        );
        assert!(findings.is_empty(), "owner {owner} was rejected: {findings:?}");
    }
}

#[test]
fn slot_constructor_in_comments_and_strings_is_ignored() {
    let text = r###"
// contrib::Slot::new("line-comment")
// Slot::new("imported-line-comment")
"contrib::Slot::new(normal-string)";
"Slot::new(imported-normal-string)";
"###;
    assert!(
        super::slot_constructor_violations("modules/demo/src/lib.rs", text).is_empty()
    );
}

#[test]
fn other_slot_constructor_is_not_the_canonical_token() {
    let text = "const X: OtherSlot<u32> = OtherSlot::new(\"x\");";
    assert!(
        super::slot_constructor_violations("modules/demo/src/lib.rs", text).is_empty()
    );
}

// --- Rules 1+2: the fortress rule (no exceptions) covers dev-dependencies -----
// The authority is `module_import_violations`. Before the no-exceptions tightening it
// `continue`d on any `kind == "dev"` dep, so a test-only reach into a sibling module or
// a foreign `<name>rpc` silently passed. These four tests pin BOTH the newly-illegal
// branches (module→module dev, module→foreign-rpc dev) AND the regression boundary that
// must stay legal at dev kind (module→core, module→events).

/// Builds a package-graph fixture: `consumer` is a `modules/<name>` impl crate with a
/// single dependency on `dep_name` of the given cargo dep `kind` (`"dev"`, `"build"`, or
/// `null` for normal), and `dep_manifest` places the dependency crate so `classify`
/// resolves its `Kind`. Shaped exactly like real `cargo metadata` output.
fn module_dep_fixture(
    consumer: &str,
    dep_name: &str,
    dep_manifest: &str,
    kind: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!([
        {
            "name": consumer,
            "manifest_path": format!("/repo/modules/{consumer}/Cargo.toml"),
            "dependencies": [
                { "name": dep_name, "kind": kind },
            ],
        },
        {
            "name": dep_name,
            "manifest_path": dep_manifest,
            "dependencies": [],
        },
    ])
}

#[test]
fn module_to_module_dev_dependency_is_a_violation() {
    // THE branch that silently passed before the no-exceptions tightening: a test-only
    // (`kind == "dev"`) edge from one fortress impl crate into another's.
    let pkgs = module_dep_fixture(
        "match",
        "rating",
        "/repo/modules/rating/Cargo.toml",
        serde_json::json!("dev"),
    );
    let v = module_import_violations(pkgs.as_array().unwrap());
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("modules/match depends on modules/rating"), "{v:?}");
}

#[test]
fn module_to_foreign_rpc_dev_dependency_is_a_violation() {
    // A test-only edge into a FOREIGN domain's generated `<name>rpc` glue is a fortress
    // breach exactly like the runtime form, so it must be flagged at `kind == "dev"`.
    let pkgs = module_dep_fixture(
        "apikeys",
        "walletrpc",
        "/repo/api/wallet/rpc/Cargo.toml",
        serde_json::json!("dev"),
    );
    let v = module_import_violations(pkgs.as_array().unwrap());
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("modules/apikeys depends on walletrpc"), "{v:?}");
    assert!(v[0].contains("api/wallet/rpc"), "{v:?}");
}

#[test]
fn module_to_core_dev_dependency_is_clean() {
    // Regression boundary: the sanctioned test-wiring pattern — a module dev-deps a core
    // crate (asyncevents/invalidation/app) to spin the plane in its own test suite. This
    // classifies `Kind::Core` and MUST stay legal at dev kind.
    let pkgs = module_dep_fixture(
        "config",
        "asyncevents",
        "/repo/core/asyncevents/Cargo.toml",
        serde_json::json!("dev"),
    );
    assert!(
        module_import_violations(pkgs.as_array().unwrap()).is_empty(),
        "module→core dev-dep must stay legal"
    );
}

#[test]
fn module_to_events_dev_dependency_is_clean() {
    // Regression boundary: audit dev-deps five FOREIGN `<name>events` crates to build
    // typed deliveries in its tests. `<name>events` classifies `Kind::Events` and MUST
    // stay legal at dev kind — the no-exceptions change must not start flagging it.
    let pkgs = module_dep_fixture(
        "audit",
        "matchevents",
        "/repo/api/match/events/Cargo.toml",
        serde_json::json!("dev"),
    );
    assert!(
        module_import_violations(pkgs.as_array().unwrap()).is_empty(),
        "module→events dev-dep must stay legal"
    );
}

// --- Rule 20: only core/edge + tools/rpc-macro may NAME edge::InvalidRequestBody -----
//
// The marker's whole correctness argument is "constructed at EXACTLY one site": it is
// what turns a peer's handler failure into a client-facing 400 (and, on a `#[retry_safe]`
// op, an instruction to the client to DROP the request instead of retrying). `edge` is a
// dependency of every module and both the type and its field are `pub`, so a second
// construction site is reachable from anywhere — hence a gate, not a convention.

#[test]
fn module_naming_the_marker_is_a_violation() {
    let findings = super::invalid_request_body_violations(
        "modules/inventory/src/lib.rs",
        "Err(Box::new(edge::InvalidRequestBody(e)))",
    );
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].contains("modules/inventory/src/lib.rs:1"), "{findings:?}");
    assert!(findings[0].contains("InvalidRequestBody"), "{findings:?}");
}

#[test]
fn cmd_root_and_other_tools_naming_the_marker_are_violations() {
    for rel in [
        "cmd/gateway-svc/src/lib.rs",
        "tools/splitproof/src/main.rs",
        "core/app/src/lib.rs",
        "api/match/rpc/src/lib.rs",
    ] {
        let findings = super::invalid_request_body_violations(
            rel,
            "use edge::InvalidRequestBody as Forge;\nlet e = Forge(err);",
        );
        assert_eq!(
            findings.len(),
            1,
            "{rel} must be flagged (the import-alias dodge included): {findings:?}"
        );
        assert!(findings[0].contains(&format!("{rel}:1")), "{findings:?}");
    }
}

#[test]
fn the_two_owner_crates_may_name_the_marker() {
    assert_eq!(super::INVALID_REQUEST_BODY_OWNERS.len(), 2);
    for (rel, text) in [
        // core/edge defines it, matches on it, and (in its own tests) constructs it.
        ("core/edge/src/server.rs", "pub struct InvalidRequestBody(pub serde_json::Error);"),
        ("core/edge/src/invalid_request_tests.rs", "Err(Box::new(InvalidRequestBody(e)))"),
        // tools/rpc-macro emits the ONE construction site into the generated adapter.
        ("tools/rpc-macro/src/lib.rs", ".map_err(::edge::InvalidRequestBody)?"),
    ] {
        assert!(
            super::invalid_request_body_violations(rel, text).is_empty(),
            "owner {rel} was rejected"
        );
    }
}

#[test]
fn a_comment_only_mention_of_the_marker_is_allowed_anywhere() {
    let text = "\
// The adapter wraps that failure in `edge::InvalidRequestBody` so it answers 400.
    /// Never constructed here: see `edge::InvalidRequestBody`.
let ok = 1;";
    assert!(
        super::invalid_request_body_violations("modules/match/src/lib.rs", text).is_empty(),
        "prose must stay free to DOCUMENT the mechanism"
    );
    // ...but a real statement on the SAME file is still caught (the exemption is per
    // line, not per file — a comment above the violation cannot launder it).
    let findings = super::invalid_request_body_violations(
        "modules/match/src/lib.rs",
        "// see edge::InvalidRequestBody\nlet e = edge::InvalidRequestBody(err);",
    );
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].contains("modules/match/src/lib.rs:2"), "{findings:?}");
}

#[test]
fn push_install_is_flagged_outside_core_app() {
    let call = "        ctx.push().install(my_sink);\n";
    assert!(super::push_install_violations("core/app/src/lib.rs", call).is_empty());
    let flagged = super::push_install_violations("modules/gateway/src/lib.rs", call);
    assert_eq!(flagged.len(), 1, "a module installing its own sink must be flagged");
    assert!(flagged[0].contains("bypasses"));
}

#[test]
fn push_install_rule_ignores_documentation_and_unrelated_installs() {
    // The mechanism must stay documentable, and an unrelated `.install(` (a metrics
    // recorder, a crypto provider) must not be swept up by the token match.
    assert!(
        super::push_install_violations(
            "modules/gateway/src/lib.rs",
            "        // never call push().install( here — contribute to SINK_SLOT\n",
        )
        .is_empty()
    );
    assert!(
        super::push_install_violations(
            "cmd/gateway-svc/src/main.rs",
            "    provider.install(Default::default());\n",
        )
        .is_empty()
    );
}

#[test]
fn push_install_rule_exempts_test_files() {
    // Steps 8/9 install a sink to exercise the seam; a test file cannot reach the
    // production wiring path, so flagging it would forbid proving the rule's subject.
    let call = "        push.install(sink);\n";
    assert!(super::push_install_violations("core/push/src/tests.rs", call).is_empty());
    assert!(super::push_install_violations("modules/notifications/src/tests.rs", call).is_empty());
    assert!(super::push_install_violations("modules/gateway/src/push_ws_tests.rs", call).is_empty());
    // A non-test file in the same module is still flagged.
    assert_eq!(
        super::push_install_violations("modules/notifications/src/service.rs", call).len(),
        1
    );
}

// --- Rule 17: which domains the stub-coverage requirement applies to ------------

/// Writes `api/<domain>/api/src/lib.rs` with one `#[http(` method, and `modules/<domain>/
/// Cargo.toml` only when `served`.
fn write_domain(root: &std::path::Path, domain: &str, served: bool) {
    let src = root.join("api").join(domain).join("api/src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("lib.rs"),
        "#[http(verb = \"POST\", path = \"/x\", auth = \"none\", success = 200)]\n",
    )
    .unwrap();
    if served {
        let module = root.join("modules").join(domain);
        std::fs::create_dir_all(&module).unwrap();
        std::fs::write(module.join("Cargo.toml"), "").unwrap();
    }
}

/// The branch the retarget introduces AND the one it must not weaken: a domain with a
/// module still demands a gateway stub; a domain whose contracts landed ahead of its
/// module does not.
#[test]
fn only_a_domain_with_a_module_needs_a_gateway_stub() {
    let root = unique_temp_dir();
    std::fs::create_dir_all(root.join("modules")).unwrap();
    write_domain(&root, "served", true);
    write_domain(&root, "contractonly", false);

    let (domains, errors) = http_op_domains(&root);
    assert_eq!(domains, vec!["served".to_string()]);
    assert!(errors.is_empty(), "errors were: {errors:?}");
    let _ = std::fs::remove_dir_all(root);
}

/// A root whose `modules/` cannot be scanned must produce an ERROR line, never a silent
/// empty domain list — a vacuous rule 17 would pass green over an unreachable domain.
#[test]
fn an_unscannable_modules_dir_is_an_error_not_an_empty_domain_list() {
    let root = unique_temp_dir();
    write_domain(&root, "served", false);

    let (domains, errors) = http_op_domains(&root);
    assert!(domains.is_empty());
    assert_eq!(errors.len(), 1, "errors were: {errors:?}");
    assert!(errors[0].contains("rule 17"), "errors were: {errors:?}");
    let _ = std::fs::remove_dir_all(root);
}

// --- Rule 21: rule 17's skip is not a silent hole ------------------------------

/// The exact shape the retarget opened and rule 21 closes: `api/social` (with `#[http(`
/// ops) implemented as `modules/socialgraph`. The dir names diverge, so rule 17 SKIPS the
/// domain and demands no gateway stub — correct in isolation, a silent hole on its own.
/// Rule 21 (`rpc_contract_model::contract_only_violations`, run from `main` beside rule
/// 17) is what turns that skip back into a FAIL. Both halves are asserted on ONE synthetic
/// root, so neither can be removed without this test dying.
#[test]
fn a_divergently_named_module_is_skipped_by_rule_17_and_caught_by_rule_21() {
    let root = unique_temp_dir();
    write_domain(&root, "served", true);
    write_domain(&root, "social", false);
    let socialgraph = root.join("modules").join("socialgraph");
    std::fs::create_dir_all(&socialgraph).unwrap();
    std::fs::write(socialgraph.join("Cargo.toml"), "").unwrap();
    for entry in rpc_contract_model::CONTRACT_ONLY {
        write_domain(&root, entry, false);
    }

    // Rule 17: `social` is not in the checked set, so a gateway lib that stubs only
    // `served` raises no violation.
    let (domains, errors) = http_op_domains(&root);
    assert!(errors.is_empty(), "errors were: {errors:?}");
    assert!(!domains.contains(&"social".to_string()), "{domains:?}");
    assert!(
        super::gateway_stub_coverage_violations(&domains, "remote::Stub::new(\"served\",")
            .is_empty(),
        "rule 17 must have nothing to say about a divergently-named domain"
    );

    // Rule 21: the same tree is a FAIL, naming the domain and the rename remedy.
    let violations = rpc_contract_model::contract_only_violations(&root);
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(violations[0].contains("`social`"), "{violations:?}");
    assert!(
        violations[0].contains("Name the module directory `modules/social`"),
        "{violations:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

// ---- Rule 22: boot-time locking DDL ----

const NON_TEST: &str = "modules/x/src/lib.rs";

/// Asserts `text` (at `rel`) yields exactly one violation, reported on source line `line`.
fn assert_one_violation_at(rel: &str, text: &str, line: usize, needle: &str) {
    let v = locking_ddl_violations(rel, text);
    assert_eq!(v.len(), 1, "expected one violation for {text:?}, got {v:?}");
    assert!(v[0].starts_with(&format!("{rel}:{line}: ")), "wrong line: {v:?}");
    assert!(v[0].contains(needle), "{v:?} should mention {needle:?}");
}

/// Asserts `text` (at `rel`) yields exactly `expected.len()` violations, each expected
/// `(line, needle)` matched by one of them.
fn assert_violations_at(rel: &str, text: &str, expected: &[(usize, &str)]) {
    let v = locking_ddl_violations(rel, text);
    assert_eq!(v.len(), expected.len(), "for {text:?}, got {v:?}");
    for (line, needle) in expected {
        let prefix = format!("{rel}:{line}: ");
        assert!(
            v.iter().any(|x| x.starts_with(&prefix) && x.contains(needle)),
            "no violation at {prefix:?} naming {needle:?} in {v:?}"
        );
    }
}

#[test]
fn locking_ddl_flags_alter_table_subcommands() {
    let text = "\nconst S_DDL: &str = \"ALTER TABLE t ALTER COLUMN c SET DEFAULT 1\";\n";
    assert_one_violation_at(NON_TEST, text, 2, "ALTER TABLE");
}

#[test]
fn locking_ddl_flags_drop_trigger_and_drop_index() {
    assert_one_violation_at(
        NON_TEST,
        "\n\nconst S_DDL: &str = \"DROP TRIGGER IF EXISTS tg ON s.t\";\n",
        3,
        "DROP TRIGGER",
    );
    assert_one_violation_at(
        NON_TEST,
        "\nconst S_DDL: &str = \"DROP INDEX IF EXISTS s.i\";\n",
        2,
        "DROP INDEX",
    );
}

#[test]
fn locking_ddl_flags_raw_sql_imports_in_module_code() {
    assert_one_violation_at(NON_TEST, "\nuse sqlx::raw_sql;\n", 2, "`raw_sql`");
    assert_one_violation_at(NON_TEST, "\n\nuse sqlx::raw_sql as rs;\n", 3, "`raw_sql`");
}

#[test]
fn locking_ddl_flags_any_raw_sql_call_in_module_code() {
    assert_one_violation_at(NON_TEST, "\nsqlx::raw_sql(other).execute(p);\n", 2, "`raw_sql`");
}

#[test]
fn locking_ddl_passes_prose_and_test_files_for_the_alter_drop_class() {
    // Whole-word on both sides: plurals and prose do not trip the forms.
    let prose = "// we alter tables lazily and drop indexes by hand\n";
    assert!(locking_ddl_violations(NON_TEST, prose).is_empty());
    // The DDL forms are exempt in test files, and a bare raw_sql there is fine too.
    let ddl = "const S_DDL: &str = \"ALTER TABLE t ADD c int; DROP TRIGGER tg ON t; DROP INDEX i\";\n\
               use sqlx::raw_sql;\n";
    assert!(locking_ddl_violations("modules/x/src/tests.rs", ddl).is_empty());
    // raw_sql outside the module-DDL owners is not this rule's business.
    assert!(locking_ddl_violations("core/foo/src/lib.rs", "use sqlx::raw_sql;\n").is_empty());
}

#[test]
fn locking_ddl_flags_create_index_if_not_exists() {
    let text = "fn a() {}\nfn b() {}\nconst S_DDL: &str = \"CREATE INDEX IF NOT EXISTS y ON s.t (a)\";\n";
    assert_one_violation_at(NON_TEST, text, 3, "INDEX IF NOT EXISTS");
}

#[test]
fn locking_ddl_flags_unique_index_if_not_exists() {
    let text = "\n\nconst S_DDL: &str = \"CREATE UNIQUE INDEX IF NOT EXISTS y ON s.t (a)\";\n";
    assert_one_violation_at(NON_TEST, text, 3, "INDEX IF NOT EXISTS");
}

#[test]
fn locking_ddl_flags_lowercase_form() {
    let text = "\nconst S_DDL: &str = \"create index if not exists y on s.t (a)\";\n";
    assert_one_violation_at(NON_TEST, text, 2, "INDEX IF NOT EXISTS");
}

#[test]
fn locking_ddl_flags_multiline_literal_on_the_line_of_the_phrase() {
    // The form straddles lines 3-4; the report names the line where it starts.
    let text = "fn a() {}\n\nconst S_DDL: &str = \"CREATE INDEX\n  IF NOT EXISTS y ON s.t (a)\";\n";
    assert_one_violation_at(NON_TEST, text, 3, "INDEX IF NOT EXISTS");
}

#[test]
fn locking_ddl_flags_backslash_continuation() {
    let text = "fn a() {}\nconst S_DDL: &str = \"CREATE INDEX \\
    IF NOT EXISTS y ON s.t (a)\";\n";
    assert_one_violation_at(NON_TEST, text, 2, "INDEX IF NOT EXISTS");
}

#[test]
fn locking_ddl_line_numbers_survive_earlier_multiline_literals() {
    // Two continuation/newline runs before the phrase: the mapping must keep counting.
    let text = "const A: &str = \"x \\
   y\";\nconst B: &str = \"p\n\nq\";\nconst S_DDL: &str = \"CREATE INDEX IF NOT EXISTS y ON s.t (a)\";\n";
    assert_one_violation_at(NON_TEST, text, 6, "INDEX IF NOT EXISTS");
}

#[test]
fn locking_ddl_flags_create_or_replace_trigger() {
    let text = "\n\n\nconst S_DDL: &str = \"CREATE OR REPLACE TRIGGER t BEFORE INSERT ON s.t\";\n";
    assert_one_violation_at(NON_TEST, text, 4, "OR REPLACE TRIGGER");
}

#[test]
fn locking_ddl_flags_add_column_if_not_exists() {
    let text = "\nconst S_DDL: &str = \"ALTER TABLE x ADD COLUMN IF NOT EXISTS y int\";\n";
    assert_violations_at(NON_TEST, text, &[(2, "ADD COLUMN"), (2, "ALTER TABLE")]);
}

#[test]
fn locking_ddl_flags_raw_sql_of_a_ddl_const() {
    assert_one_violation_at(
        NON_TEST,
        "fn a() {}\nsqlx::raw_sql(SCHEMA_DDL).execute(p);\n",
        2,
        "raw_sql(SCHEMA_DDL)",
    );
}

#[test]
fn locking_ddl_flags_raw_sql_of_a_borrowed_ddl_const() {
    assert_one_violation_at(
        NON_TEST,
        "\n\nsqlx::raw_sql(&SCHEMA_DDL).execute(p);\n",
        3,
        "raw_sql(&SCHEMA_DDL)",
    );
}

#[test]
fn locking_ddl_flags_add_if_not_exists_without_column_keyword() {
    let text = "\nconst S_DDL: &str = \"ALTER TABLE t ADD IF NOT EXISTS c int\";\n";
    assert_violations_at(NON_TEST, text, &[(2, "ADD IF NOT EXISTS"), (2, "ALTER TABLE")]);
}

#[test]
fn locking_ddl_flags_raw_sql_of_a_deref_borrowed_ddl_const() {
    assert_one_violation_at(
        NON_TEST,
        "\n\nsqlx::raw_sql(&*SCHEMA_DDL).execute(p);\n",
        3,
        "raw_sql(&*SCHEMA_DDL)",
    );
}

#[test]
fn locking_ddl_flags_raw_sql_of_a_path_qualified_ddl_const() {
    assert_one_violation_at(
        "core/admin/src/store.rs",
        "\nsqlx::raw_sql(admin::USERS_DDL).execute(p);\n",
        2,
        "raw_sql(admin::USERS_DDL)",
    );
}

#[test]
fn locking_ddl_flags_raw_sql_ddl_even_in_a_test_file() {
    assert_one_violation_at(
        "modules/x/src/tests.rs",
        "\nsqlx::raw_sql(SCHEMA_DDL).execute(p);\n",
        2,
        "raw_sql(SCHEMA_DDL)",
    );
}

#[test]
fn locking_ddl_passes_converted_ddl_runner_calls() {
    let text = "Ddl::new(p)\n    .index(\"s.y\", \"CREATE INDEX y ON s.t (a)\")\n    .trigger(\"s.t\", \"t\", \"CREATE TRIGGER t BEFORE INSERT ON s.t\");\n";
    assert!(locking_ddl_violations(NON_TEST, text).is_empty());
}

#[test]
fn locking_ddl_exempts_banned_forms_in_test_files() {
    let text = "const S_DDL: &str = \"CREATE INDEX IF NOT EXISTS y ON s.t (a)\";\n";
    for rel in [
        "modules/x/src/tests.rs",
        "modules/x/tests/foo.rs",
        "modules/x/src/store_tests.rs",
    ] {
        assert!(locking_ddl_violations(rel, text).is_empty(), "{rel}");
    }
    // The exemption is for the banned forms only; the same file is still red for raw_sql.
    assert!(!locking_ddl_violations(
        "modules/x/tests/foo.rs",
        "raw_sql(SCHEMA_DDL);"
    )
    .is_empty());
}

#[test]
fn locking_ddl_ignores_out_of_scope_paths() {
    let text = "CREATE INDEX IF NOT EXISTS y; raw_sql(SCHEMA_DDL); ADD COLUMN z";
    assert!(locking_ddl_violations("api/x/api/src/lib.rs", text).is_empty());
    assert!(locking_ddl_violations("docs/x.rs", text).is_empty());
}

#[test]
fn locking_ddl_ignores_excluded_paths() {
    let text = "CREATE INDEX IF NOT EXISTS y; CREATE OR REPLACE TRIGGER t; ADD COLUMN z";
    let rel = "tools/archcheck/src/main.rs";
    assert!(locking_ddl_excluded(rel));
    assert!(locking_ddl_violations(rel, text).is_empty());
    assert!(!locking_ddl_excluded("tools/archcheck2/src/main.rs"));
    // The plane template, the runner and the plane's lib.rs are scanned like any source.
    for rel in [
        "core/asyncevents/src/store.rs",
        "core/lifecycle/src/ddl.rs",
        "core/asyncevents/src/lib.rs",
    ] {
        assert!(!locking_ddl_excluded(rel), "{rel}");
        assert!(!locking_ddl_violations(rel, text).is_empty(), "{rel}");
    }
}

#[test]
fn locking_ddl_flags_the_escape_sequence_whitespace_branch() {
    // A literal backslash + `n` inside a Rust string counts as whitespace.
    let text = "const S_DDL: &str = \"CREATE INDEX\\nIF NOT EXISTS y\";";
    assert_one_violation_at(NON_TEST, text, 1, "INDEX IF NOT EXISTS");
}

#[test]
fn locking_ddl_scope_dirs_are_all_walked() {
    for p in LOCKING_DDL_SCOPE {
        let dir = p.trim_end_matches('/');
        assert!(WORKSPACE_SRC_DIRS.contains(&dir), "{dir} is scoped but never walked");
    }
}

#[test]
fn grep_locking_ddl_walks_scope_and_reports_the_expected_files() {
    let root = unique_temp_dir();
    let put = |rel: &str, body: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    std::fs::create_dir_all(root.join("tools/archcheck")).unwrap();
    let banned = "const S_DDL: &str = \"CREATE INDEX IF NOT EXISTS y ON s.t (a)\";\n";
    put("modules/x/src/lib.rs", banned);
    put("demos/x/src/lib.rs", banned);
    put("modules/x/src/tests.rs", "sqlx::raw_sql(SCHEMA_DDL);\n");
    put("api/x/src/lib.rs", banned);
    put("tools/archcheck/src/main.rs", banned);

    let found = grep_locking_ddl(&root);
    assert_eq!(found.len(), 3, "{found:?}");
    for rel in ["modules/x/src/lib.rs", "demos/x/src/lib.rs", "modules/x/src/tests.rs"] {
        assert!(found.iter().any(|f| f.starts_with(&format!("{rel}:1: "))), "{rel}: {found:?}");
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn locking_ddl_raw_sql_matches_only_ddl_consts_at_a_call_boundary() {
    // A non-DDL argument is still red in non-test module code (blanket raw_sql ban) but
    // green in a test file, where only `raw_sql(*_DDL)` is checked.
    assert!(!locking_ddl_violations(NON_TEST, "sqlx::raw_sql(other).execute(p);").is_empty());
    assert!(locking_ddl_violations("modules/x/src/tests.rs", "sqlx::raw_sql(other);").is_empty());
    // `my_raw_sql` is not the `raw_sql` token, but a `*_DDL` reaching anything other than
    // `.sql(` is an escape in non-test module code; a test file may do it.
    assert_one_violation_at(NON_TEST, "\nmy_raw_sql(X_DDL);\n", 2, "`X_DDL`");
    assert!(locking_ddl_violations("modules/x/src/tests.rs", "my_raw_sql(X_DDL);").is_empty());
    // Outside RAW_DDL_SCOPE the call is not this rule's business.
    assert!(locking_ddl_violations("core/foo/src/lib.rs", "raw_sql(X_DDL);").is_empty());
}

#[test]
fn ddl_ident_escapes() {
    for text in [
        "\nlet s = &SCHEMA_DDL;\n",
        "\nsqlx::Executor::execute(&pool, SCHEMA_DDL);\n",
        "\nsqlx::query(X_DDL);\n",
    ] {
        assert_one_violation_at(NON_TEST, text, 2, "is used outside a `.sql(`");
    }
    assert_one_violation_at(
        "core/admin/src/store.rs",
        "\npool.execute(admin::USERS_DDL);\n",
        2,
        "`admin::USERS_DDL`",
    );
    // Test files may hold every red shape.
    for text in [
        "let s = &SCHEMA_DDL;",
        "sqlx::Executor::execute(&pool, SCHEMA_DDL);",
        "pool.execute(admin::USERS_DDL);",
        "sqlx::query(X_DDL);",
    ] {
        assert!(locking_ddl_violations("modules/x/src/tests.rs", text).is_empty(), "{text}");
    }
    // Direct `.sql(` arguments, the definition and a doc mention are not escapes.
    for text in [
        "Ddl::new(p).sql(SCHEMA_DDL);",
        "Ddl::new(p).sql(&SCHEMA_DDL);",
        "Ddl::new(p).sql(&*SCHEMA_DDL);",
        "pub const X_DDL: &str = \"CREATE TABLE s.t (a int)\";",
        "static Y_DDL: LazyLock<String> = LazyLock::new(f);",
        "/// Applied by [`migrate`] from [SCHEMA_DDL].",
    ] {
        assert!(locking_ddl_violations(NON_TEST, text).is_empty(), "{text}");
    }
}

#[test]
fn locking_ddl_flags_each_additional_form() {
    for (text, needle) in [
        ("CREATE INDEX CONCURRENTLY IF NOT EXISTS i ON s.t (a)", "INDEX CONCURRENTLY IF NOT EXISTS"),
        ("ALTER INDEX s.i RENAME TO j", "ALTER INDEX"),
        ("DROP POLICY IF EXISTS p ON s.t", "DROP POLICY"),
        ("DROP RULE IF EXISTS r ON s.t", "DROP RULE"),
        ("CREATE OR REPLACE RULE r AS ON INSERT TO s.t DO NOTHING", "OR REPLACE RULE"),
        ("CREATE OR REPLACE VIEW s.v AS SELECT 1", "OR REPLACE VIEW"),
        ("TRUNCATE TABLE s.t", "`TRUNCATE`"),
        ("truncate only s.u", "`TRUNCATE`"),
        ("LOCK TABLE s.t IN ACCESS EXCLUSIVE MODE", "`LOCK`"),
        ("lock s.v", "`LOCK`"),
    ] {
        // Wrapped in a `*_DDL` const so only the locking form is under test.
        let wrapped = format!("\nconst S_DDL: &str = \"{text}\";\n");
        assert_one_violation_at(NON_TEST, &wrapped, 2, needle);
    }
}

#[test]
fn locking_ddl_qualified_target_forms_pass_code_and_prose() {
    for text in [
        "rows.truncate(3);",
        "use x::{truncate, y};",
        "// we truncate history nightly",
        "// the shared lock table is gone",
        // The rule needs a schema-qualified name right after the keyword (optionally
        // TABLE/ONLY); a backticked name is not one, so this prose is not flagged.
        "// take the lock `a.b` first",
    ] {
        assert!(locking_ddl_violations(NON_TEST, text).is_empty(), "{text}");
    }
}

#[test]
fn locking_ddl_remedy_text_matches_the_form_class() {
    let say_ddl = |text: &str| locking_ddl_violations(NON_TEST, text)[0].contains("use lifecycle::Ddl");
    // Create-if-missing forms have a Ddl equivalent to point at.
    assert!(say_ddl("CREATE INDEX IF NOT EXISTS i ON s.t (a)"));
    assert!(say_ddl("CREATE OR REPLACE TRIGGER t BEFORE INSERT ON s.t"));
    // ALTER/DROP contradict the wipe policy: the remedy is the wipe, not the runner.
    for text in [
        "ALTER TABLE s.t ADD c int",
        "ALTER INDEX s.i RENAME TO j",
        "DROP TRIGGER tg ON s.t",
        "DROP INDEX s.i",
        "DROP POLICY p ON s.t",
        "DROP RULE r ON s.t",
    ] {
        assert!(!say_ddl(text), "{text}");
        assert!(locking_ddl_violations(NON_TEST, text)[0].contains("wiping")
            || locking_ddl_violations(NON_TEST, text)[0].contains("wipe"), "{text}");
    }
}

#[test]
fn stray_create_text_is_flagged_outside_ddl_homes() {
    assert_one_violation_at(
        NON_TEST,
        "\nsqlx::query(\"CREATE TABLE m.t (a int)\");\n",
        2,
        "`CREATE TABLE`",
    );
    // A non-`*_DDL` const holding trigger text is stray even when later passed to
    // `.trigger(`: only the argument position is a home, not the const.
    let text = "const T: &str = \"CREATE TRIGGER t BEFORE INSERT ON s.t\";\n\
                Ddl::new(p).trigger(\"s.t\", \"t\", T);\n";
    assert_one_violation_at(NON_TEST, text, 1, "`CREATE TRIGGER`");
    // `.sql(` takes a `*_DDL` const, not inline text.
    assert_one_violation_at(
        NON_TEST,
        "\nDdl::new(p).sql(\"CREATE OR REPLACE FUNCTION f() RETURNS int AS $$ SELECT 1 $$\");\n",
        2,
        "`CREATE OR REPLACE FUNCTION`",
    );
    assert_one_violation_at(
        NON_TEST,
        "\nlet s = \"x // CREATE TABLE inside a string\";\n",
        2,
        "`CREATE TABLE`",
    );
    assert_one_violation_at(
        NON_TEST,
        "\npool.execute(TRIG_DDL);\n",
        2,
        "is used outside a `.sql(`",
    );
}

#[test]
fn stray_create_text_covers_every_phrase() {
    // An independent list, so renaming or dropping a rule phrase cannot move the test with it.
    let expected = [
        "CREATE TABLE",
        "CREATE TEMP TABLE",
        "CREATE TEMPORARY TABLE",
        "CREATE UNLOGGED TABLE",
        "CREATE INDEX",
        "CREATE UNIQUE INDEX",
        "CREATE TRIGGER",
        "CREATE SCHEMA",
        "CREATE FUNCTION",
        "CREATE OR REPLACE FUNCTION",
        "CREATE VIEW",
        "CREATE MATERIALIZED VIEW",
        "CREATE TYPE",
        "CREATE SEQUENCE",
        "CREATE EXTENSION",
    ];
    assert_eq!(DDL_CREATE_PHRASES.len(), expected.len());
    for phrase in expected {
        let lower = phrase.to_ascii_lowercase();
        let text = format!("\nsqlx::query(\"{lower} m.x\");\n");
        assert_one_violation_at(NON_TEST, &text, 2, &format!("`{phrase}`"));
        // The same text is fine in the homes: a `*_DDL` const and an `.index(` argument.
        let home = format!("const X_DDL: &str = \"{phrase} m.x\";\nb.index(\"m.x\", \"{phrase} m.x\");\n");
        assert!(locking_ddl_violations(NON_TEST, &home).is_empty(), "{phrase}");
        // And fine anywhere in a test file.
        assert!(locking_ddl_violations("modules/x/src/tests.rs", &text).is_empty(), "{phrase}");
    }
}

#[test]
fn ddl_text_homes_are_green() {
    for text in [
        "foo(); // CREATE TABLE",
        "bar(/* CREATE TABLE */)",
        "/// Creates the table: CREATE TABLE s.t.",
        "static A_DDL: LazyLock<String> = LazyLock::new(|| {\n    format!(\"CREATE SCHEMA {s}; CREATE TABLE {s}.t (a int)\")\n});",
        "Ddl::new(p).index(\"s.i\", \"CREATE UNIQUE INDEX i ON s.t (a)\");",
        "const TRIG_DDL: &str = \"CREATE TRIGGER t BEFORE INSERT ON s.t\";\nDdl::new(p).trigger(\"s.t\", \"t\", TRIG_DDL);",
        "static A_DDL: LazyLock<String> = LazyLock::new(f);\nDdl::new(p).sql(&A_DDL);",
        "let q = '\\'';\nconst B_DDL: &str = \"CREATE TABLE s.t (a int)\";",
    ] {
        assert!(locking_ddl_violations(NON_TEST, text).is_empty(), "{text}");
    }
    // Every red shape is allowed in a test file.
    for text in [
        "sqlx::query(\"CREATE TABLE m.t (a int)\");",
        "let s = \"x // CREATE TABLE inside a string\";",
        "pool.execute(TRIG_DDL);",
    ] {
        assert!(locking_ddl_violations("modules/x/src/tests.rs", text).is_empty(), "{text}");
    }
}

#[test]
fn a_char_literal_does_not_swallow_the_following_const() {
    // A `'"'` char literal mis-lexed as a string start would swallow the const below, so
    // its CREATE text would go red (or the stray one go unseen); lexing must resume.
    let text = "let q = '\"';\nconst B_DDL: &str = \"CREATE TABLE s.t (a int)\";\nsqlx::query(\"CREATE TABLE m.t (a int)\");\n";
    assert_one_violation_at(NON_TEST, text, 3, "`CREATE TABLE`");
}

#[test]
fn index_call_inside_a_comment_or_string_is_not_a_home() {
    assert_one_violation_at(
        NON_TEST,
        "// call .index( later\nsqlx::query(\"CREATE TABLE m.t (a int)\");",
        2,
        "`CREATE TABLE`",
    );
    assert_one_violation_at(
        NON_TEST,
        "let s = \"x.index(\"; sqlx::query(\"CREATE TABLE m.t (a int)\");",
        1,
        "`CREATE TABLE`",
    );
    // A `;` inside the const's string does not end the item early.
    let semi = "const A_DDL: &str = \"CREATE SCHEMA s; CREATE TABLE s.t (a int)\";";
    assert!(locking_ddl_violations(NON_TEST, semi).is_empty());
    assert!(locking_ddl_violations(NON_TEST, "b.index(X_DDL);").is_empty());
}

#[test]
fn ddl_const_names_in_strings_and_use_items_are_not_uses() {
    for text in [
        "x.context(\"SCHEMA_DDL failed\");",
        "use crate::store::SCHEMA_DDL;",
        "use crate::{\n    other::USERS_DDL,\n    x\n};",
    ] {
        assert!(locking_ddl_violations(NON_TEST, text).is_empty(), "{text}");
    }
    // A real use next to those mentions is still the one violation.
    let text = "use crate::store::SCHEMA_DDL;\nx.context(\"SCHEMA_DDL failed\");\npool.execute(SCHEMA_DDL);\n";
    assert_one_violation_at(NON_TEST, text, 3, "`SCHEMA_DDL`");
    // CREATE text in a plain string is still stray.
    assert_one_violation_at(
        NON_TEST,
        "sqlx::query(\"CREATE TABLE m.t\");",
        1,
        "`CREATE TABLE`",
    );
}

#[test]
fn comment_spans_skip_raw_and_byte_strings() {
    let spans = |t: &str| -> Vec<String> {
        comment_spans(t).into_iter().map(|r| t[r].to_string()).collect()
    };
    assert_eq!(spans(r##"let a = r#"x // no"#; // yes"##), ["// yes"]);
    assert_eq!(spans(r#"let a = b"/* no */"; /* yes */ x"#), ["/* yes */"]);
    assert_eq!(spans(r###"let a = br#"// no"#; // yes"###), ["// yes"]);
    assert_eq!(spans(r#"let u = "http://x"; // tail"#), ["// tail"]);
    // A lifetime is not a char literal: lexing continues and still finds the comment.
    assert_eq!(spans("fn f<'a>(x: &'a /* c */ str) {}"), ["/* c */"]);
    // A `'/'` char literal is not a comment start.
    assert_eq!(spans("let q = '/'; // yes"), ["// yes"]);
}

#[test]
fn skip_literal_or_comment_returns_the_offset_past_the_token() {
    assert_eq!(skip_literal_or_comment(r##"r#"a // b"# tail"##, 0), Some(11));
    assert_eq!(skip_literal_or_comment(r#"b"x" y"#, 0), Some(4));
    assert_eq!(skip_literal_or_comment("// c\nx", 0), Some(4));
    assert_eq!(skip_literal_or_comment("/* c */x", 0), Some(7));
    assert_eq!(skip_literal_or_comment("'a str", 0), None);
    assert_eq!(skip_literal_or_comment("x", 0), None);
}

#[test]
fn missing_scope_names_the_absent_directory() {
    let root = unique_temp_dir();
    for d in ["core", "modules", "cmd", "tools", "demos", "tools/adminctl"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let v = missing_scope_violations(&root);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].starts_with("core/admin/: "), "{v:?}");
    std::fs::create_dir_all(root.join("core/admin")).unwrap();
    assert!(missing_scope_violations(&root).is_empty());
    // A directory only in LOCKING_DDL_SCOPE (not RAW_DDL_SCOPE) is checked too.
    std::fs::remove_dir(root.join("demos")).unwrap();
    let v = missing_scope_violations(&root);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].starts_with("demos/: "), "{v:?}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn real_locking_ddl_scope_dirs_all_exist() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let v = missing_scope_violations(&root);
    assert!(v.is_empty(), "{v:?}");
}

#[test]
fn missing_exclusion_names_the_absent_entry() {
    let root = unique_temp_dir();
    std::fs::write(root.join("present.rs"), "").unwrap();
    let v = missing_exclusion_violations(&root, &["present.rs", "gone/file.rs"]);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].starts_with("gone/file.rs: "), "{v:?}");
    assert!(missing_exclusion_violations(&root, &["present.rs"]).is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn real_locking_ddl_exclude_entries_all_exist() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let v = missing_exclusion_violations(&root, &LOCKING_DDL_EXCLUDE);
    assert!(v.is_empty(), "{v:?}");
}
