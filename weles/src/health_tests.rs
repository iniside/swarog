use super::*;

// `Read as _` / `Write as _` arrive via `use super::*` (glob imports from the
// parent module re-export its imports within the same crate).
use std::net::TcpListener;

/// Binds an ephemeral local listener that serves exactly one connection with
/// a canned response, on its own thread.
fn serve_once(response: &'static [u8]) -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
    let port = listener.local_addr().expect("listener local addr").port();
    let handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut request = [0u8; 512];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(response);
        }
    });
    (port, handle)
}

/// A port with (very probably) nothing listening: bind ephemeral, then drop.
fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind for closed-port discovery");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

#[test]
fn probe_reports_ready_on_200() {
    let (port, server) = serve_once(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n");
    assert_eq!(probe(port), ProbeResult::Ready);
    server.join().expect("server thread");
}

#[test]
fn probe_reports_not_ready_on_503() {
    let (port, server) = serve_once(b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n");
    assert_eq!(probe(port), ProbeResult::NotReady);
    server.join().expect("server thread");
}

#[test]
fn probe_reports_not_ready_on_a_non_http_answer() {
    let (port, server) = serve_once(b"definitely not http");
    assert_eq!(probe(port), ProbeResult::NotReady);
    server.join().expect("server thread");
}

#[test]
fn probe_reports_connect_failed_on_a_closed_port() {
    assert_eq!(probe(closed_port()), ProbeResult::ConnectFailed);
}

#[test]
fn stale_listener_detection_fires_on_a_live_listener() {
    // No accept() needed: the OS backlog completes the connect regardless.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stale listener");
    let port = listener.local_addr().expect("local addr").port();
    let error = ensure_no_stale_listener("test-svc", port)
        .expect_err("a live listener must be detected as stale");
    let message = format!("{error:#}");
    assert!(message.contains("already has a listener"), "got: {message}");
    assert!(message.contains("test-svc"), "must name the service, got: {message}");
    assert!(message.contains(&format!(":{port}")), "must name the port, got: {message}");
    drop(listener);
}

#[test]
fn stale_listener_check_passes_on_a_closed_port() {
    ensure_no_stale_listener("test-svc", closed_port())
        .expect("a closed port must pass the stale-listener check");
}
