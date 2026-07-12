use super::*;
use crate::tls::DevCA;

/// A dial to a bound-but-silent UDP socket (nothing speaks QUIC there) must fail
/// within [`DIAL_DEADLINE`], not wait out the transport idle machinery — the seam
/// the gateway's route table depends on so one dead svc can't stall its callers.
#[tokio::test]
async fn dial_to_silent_socket_fails_within_deadline() {
    // Bind a real UDP socket so the packets aren't ICMP-rejected; never answer.
    let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = silent.local_addr().unwrap();

    let ca = DevCA::generate().unwrap();
    let started = std::time::Instant::now();
    let res = Client::dial(addr, &ca).await;
    let elapsed = started.elapsed();

    let err = res.err().expect("dial to a silent socket must fail");
    assert!(
        matches!(err, Error::Connect(_) | Error::Connection(_)),
        "expected a connect/connection error, got: {err}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "dial must fail within the deadline (took {elapsed:?})"
    );
}

/// Pin the client-side timing invariants: the pinned idle timeout must exceed the
/// keepalive (twin of `server_tests::edge_timing_invariants`), and the dial
/// deadline stays at the documented 5s.
#[test]
fn client_timing_invariants() {
    assert!(
        Duration::from_millis(CLIENT_IDLE_TIMEOUT_MS as u64) > KEEPALIVE_INTERVAL,
        "idle timeout must exceed the keepalive or live connections get reaped"
    );
    assert_eq!(DIAL_DEADLINE, Duration::from_secs(5));
}
