use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use serde::Deserialize;
use serde_json::json;

use super::*;

/// Serves the canned (status, body) answers in order, one per connection, and sends back each
/// request line it read ("GET /characters HTTP/1.1").
pub(crate) fn canned(answers: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let (lines, received) = mpsc::channel();
    thread::spawn(move || {
        for (status, body) in answers {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream);
            let mut request_line = String::new();
            reader.read_line(&mut request_line).expect("request line");
            let mut length = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).expect("header");
                if header == "\r\n" || header.is_empty() {
                    break;
                }
                if let Some((name, value)) = header.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().expect("content-length");
                    }
                }
            }
            let mut request_body = vec![0; length];
            reader.read_exact(&mut request_body).expect("body");
            let response = format!(
                "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            reader.get_mut().write_all(response.as_bytes()).expect("write");
            lines.send(request_line.trim_end().to_string()).expect("send");
        }
    });
    (base, received)
}

fn gateway(base: &str) -> Gateway {
    Gateway::new(base, 4).expect("client").scenario("test scenario")
}

#[test]
fn route_gives_verb_and_fills_an_id_template() {
    let row = route("characters.delete");
    assert_eq!((row.verb, row.path), ("DELETE", "/characters/{id}"));
    assert_eq!(fill_path(row.method, row.path, &[("id", "c-1")]), "/characters/c-1");
}

#[test]
fn route_fills_a_two_param_template() {
    let row = route("storage.get");
    assert_eq!(row.verb, "POST");
    let path = fill_path(row.method, row.path, &[("key", "slot1"), ("collection", "saves")]);
    assert_eq!(path, "/storage/saves/slot1/get");
}

#[test]
fn route_without_params_is_the_template() {
    let row = route("characters.list");
    assert_eq!(fill_path(row.method, row.path, &[]), "/characters");
}

#[test]
#[should_panic(expected = "missing path param \"id\"")]
fn missing_path_param_panics() {
    fill_path("friends.accept", route("friends.accept").path, &[]);
}

#[test]
#[should_panic(expected = "extra path param \"id\"")]
fn extra_path_param_panics() {
    fill_path("characters.list", route("characters.list").path, &[("id", "x")]);
}

#[test]
#[should_panic(expected = "not in opscatalog::OPERATIONS")]
fn unknown_op_panics() {
    route("characters.rename");
}

#[test]
fn decide_accepts_only_listed_statuses() {
    assert_eq!(decide(201, &[201, 409], 0), Decision::Accept);
    assert_eq!(decide(409, &[201, 409], 0), Decision::Accept);
    assert_eq!(decide(409, &[201], 0), Decision::Reject);
    assert_eq!(decide(200, &[201], 0), Decision::Reject);
    assert_eq!(decide(500, &[200], 0), Decision::Reject);
    assert_eq!(decide(204, &[], 0), Decision::Reject);
}

#[test]
fn decide_retries_429_with_growing_backoff_until_the_last_attempt() {
    assert_eq!(decide(429, &[200], 0), Decision::Retry(Duration::from_millis(250)));
    assert_eq!(decide(429, &[200], 2), Decision::Retry(Duration::from_millis(750)));
    assert_eq!(decide(429, &[200], 15), Decision::Retry(Duration::from_millis(2000)));
    assert_eq!(decide(429, &[200], MAX_ATTEMPTS - 2), Decision::Retry(Duration::from_millis(2000)));
    assert_eq!(decide(429, &[200], MAX_ATTEMPTS - 1), Decision::Reject);
    assert_eq!(decide(429, &[429], 0), Decision::Accept);
}

#[test]
fn every_op_constant_is_in_the_catalog() {
    let source = include_str!("ops.rs");
    let consts: Vec<&str> = source.lines().filter(|line| line.starts_with("pub const ")).collect();
    assert!(!consts.is_empty());
    for line in consts {
        let name = line
            .strip_suffix("\";")
            .and_then(|rest| rest.split_once(": &str = \""))
            .map(|(_, name)| name)
            .unwrap_or_else(|| panic!("ops.rs line is not `pub const X: &str = \"op\";`: {line}"));
        assert!(OPERATIONS.iter().any(|row| row.method == name), "{name} is not in opscatalog::OPERATIONS");
    }
}

#[tokio::test]
async fn call_returns_the_body_and_counts_the_op_covered() {
    let (base, lines) = canned(vec![(200, r#"[{"name":"Kestrel"}]"#)]);
    let gateway = gateway(&base);
    let result = gateway.call("characters.list", &[], Caller::Player("t"), None, &[200]).await;
    assert_eq!(result, CallResult::Ok { status: 200, body: json!([{"name": "Kestrel"}]) });
    assert_eq!(lines.recv().unwrap(), "GET /characters HTTP/1.1");
    let report = gateway.take_report();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.calls["characters.list"], crate::report::OpCalls { succeeded: 1, expected: 0, failed: 0 });
}

#[tokio::test]
async fn call_records_an_unexpected_status_and_does_not_abort() {
    let (base, lines) = canned(vec![(500, "boom"), (204, "")]);
    let gateway = gateway(&base);
    let failed = gateway.call("characters.delete", &[("id", "c-9")], Caller::Player("t"), None, &[204]).await;
    assert_eq!(failed, CallResult::Failed);
    assert_eq!(lines.recv().unwrap(), "DELETE /characters/c-9 HTTP/1.1");
    let next = gateway.call("characters.delete", &[("id", "c-9")], Caller::Player("t"), None, &[204]).await;
    assert_eq!(next, CallResult::Ok { status: 204, body: Value::Null });
    let report = gateway.take_report();
    assert_eq!(
        report.errors,
        vec![CallError {
            scenario: "test scenario".to_string(),
            op: "characters.delete",
            method: "DELETE",
            path: "/characters/c-9".to_string(),
            expected: vec![204],
            got: Got::Status(500),
            excerpt: "boom".to_string(),
        }]
    );
    assert_eq!(report.calls["characters.delete"], crate::report::OpCalls { succeeded: 1, expected: 0, failed: 1 });
}

#[tokio::test]
async fn call_retries_a_429_then_accepts() {
    let (base, _lines) = canned(vec![(429, "slow down"), (202, "")]);
    let gateway = gateway(&base);
    let result = gateway.call("match.report", &[], Caller::Server, Some(&json!({})), &[202]).await;
    assert_eq!(result, CallResult::Ok { status: 202, body: Value::Null });
    let report = gateway.take_report();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.calls["match.report"], crate::report::OpCalls { succeeded: 1, expected: 0, failed: 0 });
}

#[tokio::test]
async fn call_accepts_a_listed_409_with_its_text_body() {
    let (base, _lines) = canned(vec![(409, "already registered")]);
    let gateway = gateway(&base);
    let result = gateway.call("accounts.register", &[], Caller::Anonymous, Some(&json!({})), &[201, 409]).await;
    assert_eq!(result, CallResult::Ok { status: 409, body: Value::String("already registered".to_string()) });
    let report = gateway.take_report();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.calls["accounts.register"], crate::report::OpCalls { succeeded: 0, expected: 1, failed: 0 });
    assert_eq!(report.coverage(&[*route("accounts.register")]).uncovered, vec!["accounts.register"]);
}

#[tokio::test]
async fn call_counts_the_declared_success_as_coverage() {
    let (base, _lines) = canned(vec![(201, r#"{"player_id":"p"}"#)]);
    let gateway = gateway(&base);
    let result = gateway.call("accounts.register", &[], Caller::Anonymous, Some(&json!({})), &[201, 409]).await;
    assert_eq!(result, CallResult::Ok { status: 201, body: json!({"player_id": "p"}) });
    let report = gateway.take_report();
    assert_eq!(report.calls["accounts.register"], crate::report::OpCalls { succeeded: 1, expected: 0, failed: 0 });
    assert_eq!(report.coverage(&[*route("accounts.register")]), crate::report::Coverage::default());
}

#[test]
fn every_catalog_op_declares_a_2xx_success() {
    for row in OPERATIONS {
        assert!((200..300).contains(&row.success), "{} has success {}", row.method, row.success);
    }
}

#[test]
fn check_expect_accepts_the_declared_success_and_a_refusal_list() {
    check_expect("accounts.register", route("accounts.register"), &[201, 409]);
    check_expect("accounts.me", route("accounts.me"), &[401]);
    check_expect("characters.create", route("characters.create"), crate::expect::CHARACTERS_CREATE);
    check_expect("inventory.grant", route("inventory.grant"), crate::expect::INVENTORY_GRANT);
}

#[test]
#[should_panic(expected = "accounts.register expects 200, but its declared success is 201")]
fn check_expect_rejects_a_2xx_that_is_not_the_declared_success() {
    check_expect("accounts.register", route("accounts.register"), &[200, 409]);
}

#[tokio::test]
async fn call_records_a_refused_connection_as_a_transport_error() {
    let base = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        format!("http://{}", listener.local_addr().expect("addr"))
    };
    let gateway = gateway(&base);
    assert_eq!(gateway.call("accounts.me", &[], Caller::Player("t"), None, &[200]).await, CallResult::Failed);
    let report = gateway.take_report();
    assert_eq!(report.errors.len(), 1);
    assert!(matches!(report.errors[0].got, Got::Transport(_)), "{:?}", report.errors[0].got);
    assert_eq!(report.calls["accounts.me"], crate::report::OpCalls { succeeded: 0, expected: 0, failed: 1 });
}

#[tokio::test]
async fn probe_fails_when_nothing_listens() {
    let base = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        format!("http://{}", listener.local_addr().expect("addr"))
    };
    let err = Gateway::new(&base, 1).unwrap().probe().await.unwrap_err().to_string();
    assert!(err.contains("unreachable"), "{err}");
}

#[derive(Debug, Deserialize)]
struct Named {
    #[allow(dead_code)]
    name: String,
}

#[test]
fn typed_records_a_shape_error_for_the_op() {
    let gateway = Gateway::new("http://127.0.0.1:1", 1).unwrap().scenario("shape");
    assert!(gateway.typed::<Named>("characters.create", json!({"name": "Kestrel"})).is_some());
    assert!(gateway.typed::<Named>("characters.create", json!({"title": "Kestrel"})).is_none());
    let report = gateway.take_report();
    assert_eq!(report.errors.len(), 1);
    let error = &report.errors[0];
    assert_eq!((error.scenario.as_str(), error.op, error.method), ("shape", "characters.create", "POST"));
    assert!(matches!(&error.got, Got::Shape(cause) if cause.contains("name")), "{:?}", error.got);
    assert_eq!(error.excerpt, r#"{"title":"Kestrel"}"#);
}
