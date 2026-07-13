//! Pins `admission_budget_from_value` — the `CREDENTIAL_ADMISSION_TIMEOUT_MS` parser.
//! The parser takes the raw value (not the env var) so these tests never mutate
//! process env. `cmd/gateway-svc` carries an identical copy of the parser and these
//! pins (env-in-main convention: each front main owns its own env parsing).

use std::time::Duration;

use super::admission_budget_from_value;

#[test]
fn unset_blank_and_garbage_fall_back_to_module_default() {
    assert_eq!(admission_budget_from_value(None).unwrap(), None);
    assert_eq!(admission_budget_from_value(Some("")).unwrap(), None);
    assert_eq!(admission_budget_from_value(Some("   ")).unwrap(), None);
    assert_eq!(admission_budget_from_value(Some("abc")).unwrap(), None);
    assert_eq!(admission_budget_from_value(Some("-5")).unwrap(), None);
}

#[test]
fn positive_value_parses_with_trim() {
    assert_eq!(
        admission_budget_from_value(Some(" 250 ")).unwrap(),
        Some(Duration::from_millis(250))
    );
    assert_eq!(
        admission_budget_from_value(Some("5000")).unwrap(),
        Some(Duration::from_millis(5000))
    );
}

/// The MAJOR review pin: an explicit `0` must FAIL STARTUP LOUDLY — a zero budget
/// would 503 every credentialed request instantly, and "disable the bound" would
/// reintroduce the unbounded hang; neither is inferred silently.
#[test]
fn explicit_zero_fails_startup_loudly() {
    for raw in ["0", " 0 "] {
        let err = admission_budget_from_value(Some(raw)).unwrap_err().to_string();
        assert!(err.contains("CREDENTIAL_ADMISSION_TIMEOUT_MS"), "{err}");
        assert!(err.contains("instantly"), "{err}");
        assert!(err.contains("unset the var"), "{err}");
    }
}
