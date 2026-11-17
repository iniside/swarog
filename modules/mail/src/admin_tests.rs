//! `parse_cursor`: a hand-edited queue cursor answers `Invalid` here, never a database error
//! from `store::list_tx`, whose float8 µs product is exact only up to 2^53.

use opsapi::Status;

use super::{parse_cursor, MAX_CURSOR_MICROS};

const ID: &str = "0190b5a2-7c3e-7d4a-9f1e-123456789abc";

fn status_of(raw: &str) -> Status {
    parse_cursor(raw).expect_err(raw).status
}

#[test]
fn parse_cursor_accepts_the_bound_itself() {
    assert_eq!(MAX_CURSOR_MICROS, 1 << 53);
    let raw = format!("{}.{ID}", 1_i64 << 53);
    assert_eq!(parse_cursor(&raw).unwrap(), (1 << 53, ID));
}

#[test]
fn parse_cursor_refuses_micros_past_the_bound() {
    assert_eq!(status_of(&format!("{}.{ID}", (1_i64 << 53) + 1)), Status::Invalid);
    assert_eq!(status_of(&format!("{}.{ID}", i64::MAX)), Status::Invalid);
}

#[test]
fn parse_cursor_refuses_a_malformed_cursor() {
    for raw in [
        "",
        ID,
        "123",
        &format!(".{ID}"),
        &format!("-1.{ID}"),
        &format!("12a.{ID}"),
        "123.not-a-uuid",
        // Past i64::MAX: digits only, so the i64 parse is what refuses it.
        &format!("99999999999999999999.{ID}"),
    ] {
        assert_eq!(status_of(raw), Status::Invalid, "cursor {raw:?}");
    }
}
