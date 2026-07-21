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
    for attempt in 0..REQUEST_ATTEMPTS {
        match try_request(port, method, path, body) {
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
    unreachable!("loop above always returns or panics; last transient error: {last_error:?}")
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
