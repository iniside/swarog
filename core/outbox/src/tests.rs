use super::*;

#[test]
fn parse_subscribers_shape() {
    let m = parse_subscribers(
        "character.created=http://b/events,http://c/events; character.deleted = http://b/events ;;bad;=nourl;t2=",
    );
    assert_eq!(
        m.get("character.created").unwrap(),
        &vec!["http://b/events".to_string(), "http://c/events".to_string()]
    );
    assert_eq!(m.get("character.deleted").unwrap(), &vec!["http://b/events".to_string()]);
    // "bad" has no '=', "=nourl" has empty topic, "t2=" has empty url -> none recorded.
    assert!(!m.contains_key("bad"));
    assert!(!m.contains_key("t2"));
    assert_eq!(m.len(), 2);
}

#[test]
fn parse_subscribers_empty_is_empty_map() {
    assert!(parse_subscribers("").is_empty());
    assert!(parse_subscribers("   ").is_empty());
}

#[test]
fn valid_ident_accepts_and_rejects() {
    assert!(valid_ident("messaging"));
    assert!(valid_ident("_x9"));
    assert!(!valid_ident("9x"));
    assert!(!valid_ident("a.b"));
    assert!(!valid_ident(""));
    assert!(!valid_ident("drop table"));
}

#[tokio::test]
#[should_panic(expected = "invalid schema name")]
async fn new_panics_on_bad_schema() {
    let pool = PgPool::connect_lazy("postgres://x/y").unwrap();
    let _ = Relay::new(pool, "bad schema", "o", HashMap::new(), Vec::new());
}
