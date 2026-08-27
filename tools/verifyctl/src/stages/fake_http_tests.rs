//! The fixture is a measuring instrument: if it answers wrong, the stage it
//! serves proves nothing. These tests drive it over a real socket.

use super::*;

/// Speaks HTTP/1.1 to the fixture without reqwest, so the test does not depend
/// on the same client the stage uses. One exchange, no retries: every failure
/// here is the fixture's, and it must be loud.
fn request(port: u16, method: &str, path: &str, body: &[u8]) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to the fixture");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .expect("write the request head");
    stream.write_all(body).expect("write the request body");
    stream.flush().unwrap();
    let mut buffer = Vec::new();
    stream
        .read_to_end(&mut buffer)
        .expect("read the fixture's answer");
    String::from_utf8_lossy(&buffer).into_owned()
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

#[test]
fn it_answers_a_client_that_connects_before_it_writes() {
    // The failure this pins is a race the fixture usually wins, so the client
    // loses it on purpose: it connects and holds its bytes back, which leaves
    // the fixture's first read facing an empty socket — the one state where an
    // accepted-but-still-non-blocking stream reports EAGAIN instead of waiting.
    // The client not having written IS the happens-before here; the sleep only
    // gives the accept loop's poll room to have gotten there.
    let served = Arc::new(std::sync::Mutex::new(0u32));
    let counter = Arc::clone(&served);
    let fixture = FakeHttp::start(move |route, _| {
        *counter.lock().unwrap() += 1;
        assert_eq!(route, "POST /resolve");
        (200, br#"{"ok":true}"#.to_vec())
    })
    .unwrap();

    let mut stream = TcpStream::connect(("127.0.0.1", fixture.port())).expect("connect");
    // Hang guard with headroom, not a timing assertion: a fixture that never
    // answers fails the test instead of parking the run forever.
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));

    let body = br#"{"provider":"admin"}"#;
    write!(
        stream,
        "POST /resolve HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
    stream.flush().unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read the answer");
    let response = String::from_utf8_lossy(&response);

    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.ends_with(r#"{"ok":true}"#), "{response}");
    assert_eq!(*served.lock().unwrap(), 1);
}
