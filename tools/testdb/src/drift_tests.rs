use crate::drift::{
    crate_findings, dev_deps_testdb, file_findings, is_test_file, workspace_findings,
    workspace_root, Finding,
};

const NAIVE_COPY: &str = r#"
async fn db() -> Option<PgPool> {
    let Ok(pool) = PgPool::connect(&dsn).await else {
        eprintln!("SKIP: no database here, moving on");
        return None;
    };
    Some(pool)
}
"#;

fn describe(findings: &[Finding]) -> String {
    findings
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_gate_fires_on_a_re_copied_skip_however_it_is_worded() {
    let findings = file_findings("modules/widget/src/tests.rs", NAIVE_COPY);
    assert_eq!(findings.len(), 2, "{}", describe(&findings));
}

/// The message needs no shared phrase, and the helper needs no shared name or signature.
#[test]
fn a_reworded_multi_line_skip_is_still_caught() {
    let reworded = "async fn grab() -> Option<sqlx::PgPool> {\n    let Ok(p) =\n        \
                    PgPool::connect(&url)\n            .await\n    else {\n        return None;\n    \
                    };\n    Some(p)\n}\n";
    let findings = file_findings("modules/widget/src/tests.rs", reworded);
    assert_eq!(findings.len(), 1, "{}", describe(&findings));
}

/// The hole the first version had: naming the authority somewhere in the file must NOT
/// exempt a second, hand-rolled skip in the same file.
#[test]
fn naming_the_authority_does_not_exempt_a_second_skip_in_the_same_file() {
    let mixed = format!("use testdb::test_pool;\n{NAIVE_COPY}");
    let findings = file_findings("modules/widget/src/tests.rs", &mixed);
    assert_eq!(findings.len(), 2, "{}", describe(&findings));
}

#[test]
fn a_connect_a_test_needs_is_expected_not_skipped() {
    let clean = "let pool2 = PgPool::connect(&dsn()).await.expect(\"second replica\");\n";
    assert!(file_findings("modules/widget/src/tests.rs", clean).is_empty());
    let sanctioned = "let Some(pool) = test_pool().await else { return };\n";
    assert!(file_findings("modules/widget/src/tests.rs", sanctioned).is_empty());
}

/// A printed skip about something other than the database is not this gate's business.
#[test]
fn a_non_database_skip_message_is_left_alone() {
    let other = "eprintln!(\"WARN: fixture cleanup SKIPPED — no tokio runtime in scope\");\n";
    assert!(
        file_findings("modules/widget/src/tests.rs", other).is_empty(),
        "{}",
        describe(&file_findings("modules/widget/src/tests.rs", other))
    );
}

#[test]
fn the_gate_fires_on_a_second_spelling_of_the_opt_out() {
    let findings = file_findings(
        "modules/widget/src/lib.rs",
        "let allowed = std::env::var(\"TESTDB_ALLOW_SKIP\").is_ok();\n",
    );
    assert_eq!(findings.len(), 1, "{}", describe(&findings));
}

#[test]
fn prose_may_name_the_markers() {
    let prose = "// TESTDB_ALLOW_SKIP is the only opt-out; nothing prints a postgres skip.\n";
    assert!(file_findings("modules/widget/src/tests.rs", prose).is_empty());
}

#[test]
fn the_authority_itself_is_exempt() {
    assert!(file_findings("tools/testdb/src/lib.rs", NAIVE_COPY).is_empty());
    assert!(file_findings("tools\\testdb\\src\\lib.rs", NAIVE_COPY).is_empty());
}

#[test]
fn test_files_are_recognised_by_shape() {
    assert!(is_test_file("modules/widget/src/tests.rs"));
    assert!(is_test_file("modules/widget/src/store_tests.rs"));
    assert!(is_test_file("modules/accounts/src/tests/guest.rs"));
    assert!(is_test_file("core/remote/tests/abrupt_kill_redial.rs"));
    assert!(!is_test_file("modules/widget/src/lib.rs"));
}

/// One crate reverting must fail on its own — never masked by the other twenty.
#[test]
fn a_single_crate_that_stops_routing_through_the_authority_fails_by_itself() {
    let reverted = vec![(
        "modules/widget/src/tests.rs".to_string(),
        "let pool = PgPool::connect(&dsn).await.unwrap();\n".to_string(),
    )];
    let findings = crate_findings("modules/widget", false, &reverted);
    assert_eq!(findings.len(), 2, "{}", describe(&findings));

    let routed = vec![(
        "modules/widget/src/tests.rs".to_string(),
        "use testdb::test_pool;\nlet pool = PgPool::connect(&dsn).await.unwrap();\n".to_string(),
    )];
    assert!(crate_findings("modules/widget", true, &routed).is_empty());
}

#[test]
fn a_stale_dev_dependency_is_a_finding() {
    let no_db_tests = vec![("modules/widget/src/tests.rs".to_string(), "fn pure() {}\n".to_string())];
    let findings = crate_findings("modules/widget", true, &no_db_tests);
    assert_eq!(findings.len(), 1, "{}", describe(&findings));
}

#[test]
fn only_a_dev_dependency_counts_as_a_db_test_claim() {
    assert!(dev_deps_testdb("[dev-dependencies]\ntestdb = { workspace = true }\n"));
    assert!(dev_deps_testdb("[dev-dependencies]\ntestdb.workspace = true\n"));
    assert!(!dev_deps_testdb("[dependencies]\ntestdb = { workspace = true }\n"));
    assert!(!dev_deps_testdb(
        "[dependencies]\ntestdb = { workspace = true }\n[dev-dependencies]\ntokio = \"1\"\n"
    ));
}

#[test]
fn the_workspace_routes_every_database_test_through_the_one_authority() {
    let findings = workspace_findings(&workspace_root());
    assert!(
        findings.is_empty(),
        "{} drift finding(s):\n{}",
        findings.len(),
        describe(&findings)
    );
}
