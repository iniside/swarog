//! The fixture is a measuring instrument: if it answers wrong, the stage it
//! serves proves nothing. These tests drive it over a real socket.

use super::*;

/// Speaks HTTP/1.1 to the fixture without reqwest, so the test does not depend
/// on the same client the stage uses.
///
/// Under heavy parallel `cargo test` load the loopback socket can drop a
/// connection mid-exchange (`BrokenPipe`/`ConnectionReset`/etc) even though the
/// fixture served it correctly — that's a transient race in the test's own
/// client socket, not a fixture defect. Retry the whole exchange on a fresh
/// `TcpStream` a bounded number of times for exactly those transient error
/// kinds; a genuinely broken fixture still fails every attempt and the final
/// attempt still panics loudly via `unwrap`/`expect`.
///
/// The same load can also produce an `Ok` short read: `read_to_end` returns
/// having received an empty or partial response (early EOF before the fixture
/// finished writing) — not an I/O error, so `is_transient` never sees it.
/// Treat an incomplete response the same way: retry the whole exchange on a
/// fresh connection. Only the FINAL attempt returns an incomplete response
/// as-is, so a fixture that is genuinely broken (never completes a response)
/// still fails loudly with the real bytes in the assertion message, instead of
/// retrying forever or being masked into a false pass.
const REQUEST_ATTEMPTS: u32 = 5;
const REQUEST_RETRY_BACKOFF: Duration = Duration::from_millis(50);

fn is_transient(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionRefused
    )
}

/// A complete HTTP/1.1 response: non-empty, starts with the status line, and
/// carries the header terminator (i.e. headers were fully received). This
/// deliberately does not check the body length against `Content-Length` — the
/// stage's own client (reqwest) owns that concern; this is only enough to
/// distinguish "the fixture answered" from "the socket gave up mid-write".
fn is_complete_response(response: &str) -> bool {
    !response.is_empty() && response.starts_with("HTTP/1.1 ") && response.contains("\r\n\r\n")
}

fn try_request(port: u16, method: &str, path: &str, body: &[u8]) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()?;
    let mut buffer = Vec::new();
    stream.read_to_end(&mut buffer)?;
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

fn request(port: u16, method: &str, path: &str, body: &[u8]) -> String {
    let mut last_error = None;
    let mut last_incomplete = None;
    for attempt in 0..REQUEST_ATTEMPTS {
        match try_request(port, method, path, body) {
            Ok(response) if is_complete_response(&response) => return response,
            Ok(response) if attempt + 1 < REQUEST_ATTEMPTS => {
                last_incomplete = Some(response);
                std::thread::sleep(REQUEST_RETRY_BACKOFF);
            }
            Ok(response) => return response,
            Err(error) if attempt + 1 < REQUEST_ATTEMPTS && is_transient(&error) => {
                last_error = Some(error);
                std::thread::sleep(REQUEST_RETRY_BACKOFF);
            }
            Err(error) => panic!(
                "request to the fixture failed after {} attempt(s): {error}",
                attempt + 1
            ),
        }
    }
    unreachable!(
        "loop above always returns or panics; last transient error: {last_error:?}; \
         last incomplete response: {last_incomplete:?}"
    )
}

#[test]
fn it_hands_the_handler_the_method_the_path_and_the_body() {
    // All three matter to the stage that uses this: the method is the half of
    // the wire contract no in-memory check can reach, the path routes, and the
    // body carries the question.
    let seen = Arc::new(std::sync::Mutex::new(Vec::<(String, Vec<u8>)>::new()));
    let recorder = Arc::clone(&seen);
    let fixture = FakeHttp::start(move |route, body| {
        recorder
            .lock()
            .unwrap()
            .push((route.to_string(), body.to_vec()));
        (200, br#"{"ok":true}"#.to_vec())
    })
    .unwrap();

    let response = request(fixture.port(), "POST", "/resolve", br#"{"provider":"admin"}"#);
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.ends_with(r#"{"ok":true}"#), "{response}");

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].0, "POST /resolve");
    assert_eq!(seen[0].1, br#"{"provider":"admin"}"#);
}

#[test]
fn a_handlers_status_reaches_the_client() {
    // The stage's fake agent answers 404 for an unknown peer; if the fixture
    // flattened every answer to 200, the client would read a refusal as an
    // address.
    let fixture = FakeHttp::start(|_, _| (404, br#"{"code":"unknown_peer"}"#.to_vec())).unwrap();
    let response = request(fixture.port(), "POST", "/resolve", b"{}");
    assert!(response.starts_with("HTTP/1.1 404 Not Found"), "{response}");
}

#[test]
fn it_serves_more_than_one_connection() {
    // `Connection: close` per request means the gateway opens a fresh connection
    // per resolve — it asks eight times. A fixture that served one and stopped
    // would hang the boot it is supposed to be measuring.
    let fixture = FakeHttp::start(|_, _| (200, b"{}".to_vec())).unwrap();
    for _ in 0..8 {
        assert!(request(fixture.port(), "POST", "/resolve", b"{}").starts_with("HTTP/1.1 200"));
    }
}

#[test]
fn dropping_it_releases_the_port() {
    // The stage boots a fleet after this; a leaked listener would be a stale
    // listener for the next rollout to trip over — and weles fails loudly on
    // exactly that.
    let port = {
        let fixture = FakeHttp::start(|_, _| (200, b"{}".to_vec())).unwrap();
        fixture.port()
    };
    // The join in Drop has already happened, so the bind must succeed now.
    TcpListener::bind(("127.0.0.1", port)).expect("the fixture's port must be free after drop");
}
