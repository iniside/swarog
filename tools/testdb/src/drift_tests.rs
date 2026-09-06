use crate::drift::{file_findings, workspace_findings, workspace_root};

const NAIVE_COPY: &str = r#"
async fn test_pool() -> Option<PgPool> {
    match PgPool::connect(&dsn).await {
        Ok(p) => Some(p),
        Err(_) => {
            eprintln!("SKIP: postgres unreachable at {dsn} — widget DB tests skipped");
            None
        }
    }
}
"#;

#[test]
fn the_gate_fires_on_a_re_copied_silent_skip_helper() {
    let findings = file_findings("modules/widget/src/tests.rs", NAIVE_COPY);
    assert_eq!(
        findings.len(),
        2,
        "the skip line and the local definition must both be named: {findings:?}"
    );
}

#[test]
fn the_gate_fires_on_a_second_spelling_of_the_opt_out() {
    let findings = file_findings(
        "modules/widget/src/tests.rs",
        "let allowed = std::env::var(\"TESTDB_ALLOW_SKIP\").is_ok();\n",
    );
    assert_eq!(findings.len(), 1, "{findings:?}");
}

#[test]
fn prose_may_name_the_markers() {
    let findings = file_findings(
        "modules/widget/src/tests.rs",
        "// The pool comes from testdb: no postgres unreachable skip line, and\n// TESTDB_ALLOW_SKIP is the only opt-out.\n",
    );
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn a_consumer_that_imports_the_authority_is_clean() {
    let findings = file_findings(
        "modules/widget/src/tests.rs",
        "use testdb::test_pool;\n\nasync fn migrated_pool() -> Option<PgPool> {\n    let pool = test_pool().await?;\n    Some(pool)\n}\n",
    );
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn the_authority_itself_is_exempt() {
    assert!(file_findings("tools/testdb/src/lib.rs", NAIVE_COPY).is_empty());
    assert!(file_findings("tools\\testdb\\src\\lib.rs", NAIVE_COPY).is_empty());
}

#[test]
fn the_workspace_routes_every_database_test_through_the_one_authority() {
    let findings = workspace_findings(&workspace_root());
    assert!(
        findings.is_empty(),
        "{} drift finding(s):\n{}",
        findings.len(),
        findings
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}
