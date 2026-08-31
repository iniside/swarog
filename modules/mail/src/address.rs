use std::str::FromStr;

use lettre::Address;

use mailevents::MAX_ADDRESS_BYTES;

/// The ONE address rule, shared by the operator's envelope sender (`MAIL_FROM`) and by the
/// producer's recipient — and it is THE SAME PARSER the transport builds the envelope
/// with, so ingress refuses exactly what the relay would. A shape check that merely looked
/// for an `@` accepted `a@`, which every relay rejects: the row is enqueued, claimed, and
/// permanently parked on its first attempt, where a refusal at ingress costs one counter
/// and no row. `MAIL_FROM` gets the same treatment for a harder reason — a `From` no relay
/// accepts parks EVERY row, so it must fail the boot.
///
/// The parsed [`Address`] is returned rather than discarded: a caller that needs the value
/// (the transport's envelope) must not re-parse it under a second rule.
///
/// `Err` carries a predicate phrase; each caller words the subject and its own error type.
pub(crate) fn parse_address(value: &str) -> Result<Address, String> {
    if value.trim().is_empty() {
        return Err("is required".to_string());
    }
    if value.len() > MAX_ADDRESS_BYTES {
        return Err(format!("exceeds {MAX_ADDRESS_BYTES} bytes"));
    }
    // CR/LF in an address is header injection: the value goes into the message header,
    // where a newline starts a header nobody wrote. Checked before the parse so the
    // operator reads the injection verdict, not a generic parse failure.
    if value.chars().any(char::is_control) {
        return Err("must not contain control characters".to_string());
    }
    Address::from_str(value).map_err(|e| format!("is not a routable address: {e}"))
}
