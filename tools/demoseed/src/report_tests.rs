use super::*;

const CATALOG: &[OpInfo] = &[
    OpInfo { method: "a.one", verb: "GET", path: "/one", auth: "none", success: 200 },
    OpInfo { method: "a.two", verb: "POST", path: "/two", auth: "player", success: 200 },
    OpInfo { method: "a.three", verb: "GET", path: "/three", auth: "player", success: 200 },
];

fn error(op: &'static str, got: Got) -> CallError {
    CallError {
        scenario: "player demo-01@demo.local".to_string(),
        op,
        method: "POST",
        path: "/two".to_string(),
        expected: vec![201],
        got,
        excerpt: "nope".to_string(),
    }
}

#[test]
fn coverage_is_full_when_every_op_matched_once() {
    let mut report = Report::default();
    for row in CATALOG {
        report.record_call(row.method, Answer::Success);
    }
    report.record_call("a.two", Answer::Failed);
    assert_eq!(report.coverage(CATALOG), Coverage::default());
}

#[test]
fn coverage_names_an_op_never_called() {
    let mut report = Report::default();
    report.record_call("a.one", Answer::Success);
    report.record_call("a.three", Answer::Success);
    assert_eq!(report.coverage(CATALOG), Coverage { failed_only: vec![], uncovered: vec!["a.two"] });
}

#[test]
fn coverage_reports_an_op_called_but_only_failed_as_failed_not_uncovered() {
    let mut report = Report::default();
    report.record_call("a.one", Answer::Success);
    report.record_call("a.two", Answer::Failed);
    report.record_call("a.two", Answer::Failed);
    report.record_call("a.three", Answer::Success);
    assert_eq!(report.coverage(CATALOG), Coverage { failed_only: vec!["a.two"], uncovered: vec![] });
}

#[test]
fn render_passes_with_no_errors_and_full_coverage() {
    let mut report = Report::default();
    for row in CATALOG {
        report.record_call(row.method, Answer::Success);
    }
    let rendered = report.render("http://h:1", CATALOG);
    assert!(!rendered.failed, "{}", rendered.text);
    assert!(rendered.text.contains("all 3 catalog ops covered"), "{}", rendered.text);
}

#[test]
fn render_lists_errors_by_op_and_fails() {
    let mut report = Report::default();
    for row in CATALOG {
        report.record_call(row.method, Answer::Success);
    }
    report.errors.push(error("a.two", Got::Status(500)));
    report.errors.push(error("a.one", Got::Transport("refused".to_string())));
    report.skip("group G".to_string(), "memberships skipped".to_string());
    let rendered = report.render("http://h:1", CATALOG);
    assert!(rendered.failed);
    let text = &rendered.text;
    assert!(text.contains("errors (2):"), "{text}");
    assert!(
        text.contains("  a.two (1)\n    [player demo-01@demo.local] POST /two expected [201], got HTTP 500: nope\n"),
        "{text}"
    );
    assert!(text.contains("got transport error: refused"), "{text}");
    assert!(text.find("  a.one (1)").unwrap() < text.find("  a.two (1)").unwrap(), "grouped in op order: {text}");
    assert!(text.contains("  [group G] memberships skipped\n"), "{text}");
    assert!(text.contains("FAILED (2 errors, 0 ops not covered)"), "{text}");
}

#[test]
fn render_fails_on_an_uncovered_op_without_errors() {
    let mut report = Report::default();
    report.record_call("a.one", Answer::Success);
    report.record_call("a.two", Answer::Success);
    let rendered = report.render("http://h:1", CATALOG);
    assert!(rendered.failed);
    assert!(rendered.text.contains("ops not covered (no call answered the declared success): a.three"), "{}", rendered.text);
}

#[test]
fn render_hints_at_the_dev_flag_behind_a_404() {
    let mut report = Report::default();
    report.errors.push(error("accounts.register", Got::Status(404)));
    assert!(report.render("http://h:1", &[]).text.contains("without ACCOUNTS_DEV_AUTH"));
}

#[test]
fn excerpt_keeps_at_most_400_chars() {
    assert_eq!(excerpt(&"x".repeat(1000)).chars().count(), EXCERPT_CHARS);
    assert_eq!(excerpt("short"), "short");
    assert_eq!(excerpt(&"ż".repeat(500)).chars().count(), 400);
}

#[test]
fn outcome_409_is_already_exists() {
    assert_eq!(Outcome::from_status(409), Outcome::AlreadyExists);
    for status in [200, 201, 202, 204] {
        assert_eq!(Outcome::from_status(status), Outcome::Created);
    }
}

#[test]
fn coverage_counts_only_the_declared_success_not_an_expected_409() {
    let mut report = Report::default();
    report.record_call("a.one", Answer::Success);
    report.record_call("a.two", Answer::Expected);
    report.record_call("a.two", Answer::Expected);
    report.record_call("a.three", Answer::Expected);
    report.record_call("a.three", Answer::Failed);
    assert_eq!(report.coverage(CATALOG), Coverage { failed_only: vec!["a.three"], uncovered: vec!["a.two"] });
    let rendered = report.render("http://gw", CATALOG);
    assert!(rendered.failed, "{}", rendered.text);
    assert!(rendered.text.contains("ops not covered (no call answered the declared success): a.two"), "{}", rendered.text);
}

#[test]
fn a_declared_success_covers_even_beside_an_expected_409() {
    let mut report = Report::default();
    for row in CATALOG {
        report.record_call(row.method, Answer::Expected);
        report.record_call(row.method, Answer::Success);
    }
    assert_eq!(report.coverage(CATALOG), Coverage::default());
    assert!(!report.render("http://gw", CATALOG).failed);
}
