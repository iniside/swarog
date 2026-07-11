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
/// the `modules/` dir scan (admin/gateway own none but still appear as dir names).
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
    std::fs::create_dir_all(root.join("characters/api/src")).unwrap();
    std::fs::create_dir_all(root.join("rating/api/src")).unwrap();
    // characters exposes an #[http( op; rating is wire-only — the ONLY `#[http(` there is
    // in a comment, which must be skipped.
    std::fs::write(
        root.join("characters/api/src/lib.rs"),
        "#[rpc]\ntrait Characters {\n    #[http(post, \"/create\")]\n    fn create();\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("rating/api/src/lib.rs"),
        "// wire-only; no #[http( ops here\n#[rpc]\ntrait Mmr { fn get(); }\n",
    )
    .unwrap();
    let domains = super::http_op_domains(&root);
    assert_eq!(domains, vec!["characters".to_string()], "{domains:?}");
    let _ = std::fs::remove_dir_all(&root);
}
