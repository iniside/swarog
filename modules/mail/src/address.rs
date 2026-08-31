use mailevents::MAX_ADDRESS_BYTES;

/// The ONE address-shape rule, shared by the operator's envelope sender (`MAIL_FROM`) and
/// by the producer's recipient. Two rules would leave the weaker one guarding the path to
/// the relay: a recipient no relay can route is accepted at ingress, takes a row, and then
/// burns every attempt behind exponential backoff before parking — where a refusal at
/// ingress costs one counter and no row.
///
/// `Err` carries a predicate phrase; each caller words the subject and its own error type.
pub(crate) fn check_address(value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err("is required".to_string());
    }
    if value.len() > MAX_ADDRESS_BYTES {
        return Err(format!("exceeds {MAX_ADDRESS_BYTES} bytes"));
    }
    // CR/LF in an address is header injection: the value goes into the message header,
    // where a newline starts a header nobody wrote.
    if value.chars().any(char::is_control) {
        return Err("must not contain control characters".to_string());
    }
    if !value.contains('@') {
        return Err("is not an address (no '@')".to_string());
    }
    Ok(())
}
