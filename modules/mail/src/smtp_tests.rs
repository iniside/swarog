//! The `smtp` provider driven against a REAL socket: a loopback relay on an ephemeral
//! port, a hit counter and the lines the client actually wrote. The counter is the point —
//! it turns "did the I/O happen?" into an assertion instead of an absence of errors, the
//! `serve_counting_jwks` instrument from `modules/accounts/src/oidc_tests.rs`.
//!
//! **The fixture is plaintext, and the sender has no plaintext arm** ([`crate::smtp::TlsMode`]
//! is `starttls` or `implicit`, deliberately — this channel carries verification links).
//! lettre therefore reaches the banner and EHLO in the clear and refuses to go further, so
//! what these tests execute over the wire is the greeting, the EHLO the envelope sender's
//! domain names, and every arm of [`crate::smtp::classify`] reachable before the upgrade.
//! `MAIL FROM`/`RCPT TO`/`DATA` and a `250` acceptance are past the TLS upgrade and are NOT
//! covered here — a fixture that scripted them would be dead code reading as proof. Both
//! that and TLS negotiation itself are recorded gaps, not silent ones.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::PgPool;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::config::{
    MailConfig, FROM_ENV, PROVIDER_ENV, SEND_TIMEOUT_ENV, SMTP_HOST_ENV, SMTP_PORT_ENV,
    SMTP_TLS_ENV,
};
use crate::providers::{Outgoing, SendError, Sender, SMTP};
use crate::smtp::TlsMode;
use crate::store::{Claimed, Disposition};
use crate::tests::DEFAULT_DSN;
use crate::worker::{attempt, backoff_secs, disposition, Drain};

/// The envelope sender. Its DOMAIN is what [`crate::smtp`] puts in the EHLO, which is the
/// one piece of the envelope a plaintext fixture ever sees.
const FROM: &str = "noreply@mail-fixture.test";
const HELLO: &str = "EHLO mail-fixture.test";
const RECIPIENT: &str = "player@example.com";
const SUBJECT: &str = "Verify your address";

/// Every socket-touching test runs under this, with orders of magnitude of headroom over
/// the budgets under test — a wedge fails the test loudly instead of hanging the binary.
const HANG_GUARD: Duration = Duration::from_secs(10);

/// The send budget for the hang test. Short enough that the assertion is quick, and two
/// orders of magnitude under [`HANG_GUARD`], so which bound fired is never ambiguous.
const SHORT_SEND_TIMEOUT_MS: u64 = 300;

/// The budget for the tests whose fixture answers immediately; nothing waits on it.
const SEND_TIMEOUT_MS: u64 = 2_000;

// ============================================================================
// The loopback relay.
// ============================================================================

/// What the fixture does with a connection it accepted.
#[derive(Clone, Copy)]
enum Script {
    /// Close the moment the peer connects — the relay that vanishes mid-dialogue.
    DropOnConnect,
    /// Hold the socket open and never write a banner — the hang the send budget exists for,
    /// and the case an error-free assertion would miss entirely.
    NeverGreet,
    /// Greet with `220`, then answer the client's EHLO with this literal reply.
    AnswerEhlo(&'static str),
}

/// A `250` EHLO reply that advertises no STARTTLS: a positive multi-line response the
/// client parses successfully, followed by the sender's own refusal to downgrade.
const CAPABILITIES_WITHOUT_STARTTLS: &str = "250-fixture.invalid\r\n250 8BITMIME\r\n";

/// A permanent relay verdict.
const PERMANENT_REFUSAL: &str = "550 5.7.1 relay access denied\r\n";

struct Relay {
    addr: SocketAddr,
    hits: Arc<AtomicUsize>,
    transcript: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Relay {
    /// Aborting the accept loop drops the connections it owns with it, so a `NeverGreet`
    /// socket lives exactly as long as the fixture and not one test longer.
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Relay {
    async fn start(script: Script) -> Relay {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral loopback port");
        let addr = listener.local_addr().expect("the bound address");
        let hits = Arc::new(AtomicUsize::new(0));
        let transcript = Arc::new(Mutex::new(Vec::new()));
        let counter = hits.clone();
        let recorded = transcript.clone();
        let task = tokio::spawn(async move {
            // Connections are handled inline: one send opens exactly one of them (the
            // `pool` feature is off), and holding them here is what keeps `NeverGreet`
            // hanging rather than resetting.
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                match script {
                    Script::DropOnConnect => drop(stream),
                    Script::NeverGreet => held.push(stream),
                    Script::AnswerEhlo(reply) => converse(stream, reply, &recorded).await,
                }
            }
        });
        Relay {
            addr,
            hits,
            transcript,
            task,
        }
    }

    /// An address nothing is listening on: bound to reserve it, then dropped. The refusal
    /// is proven by construction rather than by picking a port and hoping.
    async fn dead_address() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral loopback port");
        listener.local_addr().expect("the bound address")
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    fn transcript(&self) -> Vec<String> {
        self.transcript.lock().expect("the transcript lock").clone()
    }
}

/// Greets, records every line the client writes, and answers the first of them. The line is
/// recorded BEFORE the reply is written, so a client that observed the reply necessarily
/// left its command in the transcript first — a happens-before, not a race.
///
/// Every later line gets a `221`. lettre answers a rejected EHLO by sending `QUIT` and
/// BLOCKING on its response, so a fixture that went silent after the rejection would be
/// held open until the send budget expired — a property of the fixture, not of the mapping
/// under test.
async fn converse(stream: TcpStream, ehlo_reply: &str, transcript: &Arc<Mutex<Vec<String>>>) {
    let (rx, mut tx) = stream.into_split();
    if tx.write_all(b"220 fixture.invalid ESMTP\r\n").await.is_err() {
        return;
    }
    let mut lines = BufReader::new(rx).lines();
    let mut answered = false;
    while let Ok(Some(line)) = lines.next_line().await {
        transcript.lock().expect("the transcript lock").push(line);
        let reply: &[u8] = if answered {
            b"221 fixture.invalid closing connection\r\n"
        } else {
            answered = true;
            ehlo_reply.as_bytes()
        };
        if tx.write_all(reply).await.is_err() {
            return;
        }
    }
}

// ============================================================================
// The sender under test, built the way `Mail::register` builds it.
// ============================================================================

/// Goes through `MailConfig::from_vars` and `Provider::sender` — the same two calls
/// `lib.rs` makes — so the configuration authority is on the path and the connect bound is
/// the send budget, exactly as in production.
fn smtp_sender(addr: SocketAddr, tls: &str, send_timeout_ms: u64) -> (Arc<dyn Sender>, Duration) {
    let port = addr.port().to_string();
    let ms = send_timeout_ms.to_string();
    let vars: BTreeMap<String, String> = [
        (PROVIDER_ENV, SMTP),
        (FROM_ENV, FROM),
        (SMTP_HOST_ENV, "127.0.0.1"),
        (SMTP_PORT_ENV, port.as_str()),
        (SMTP_TLS_ENV, tls),
        (SEND_TIMEOUT_ENV, ms.as_str()),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
    .collect();
    let cfg = MailConfig::from_vars(&vars).expect("the fixture's relay configuration");
    let settings = cfg
        .provider
        .as_ref()
        .expect("an smtp provider is configured");
    let sender = settings
        .provider
        .sender(&settings.from, cfg.send_timeout)
        .expect("the transport builds with no socket and no DNS");
    (sender, cfg.send_timeout)
}

async fn send_once(sender: &Arc<dyn Sender>) -> Result<(), SendError> {
    sender
        .send(&Outgoing {
            from: FROM,
            to: RECIPIENT,
            subject: SUBJECT,
            body: "verify your address",
            kind: "verification",
        })
        .await
}

/// The send under [`HANG_GUARD`]: nothing here may depend on a real clock, but a wedged
/// dialogue must fail the test rather than stall the binary.
async fn guarded_send(sender: &Arc<dyn Sender>) -> Result<(), SendError> {
    tokio::time::timeout(HANG_GUARD, send_once(sender))
        .await
        .expect("the sender must not hang past the guard")
}

fn assert_infra(result: &Result<(), SendError>) {
    assert!(
        matches!(result, Err(SendError::Infra(_))),
        "a message that never reached a verdict must be transient, got {result:?}"
    );
}

/// A claimed row on its first attempt — the input [`disposition`] turns into a row write.
fn claimed() -> Claimed {
    Claimed {
        id: "00000000-0000-0000-0000-000000000001".to_string(),
        recipient: RECIPIENT.to_string(),
        subject: SUBJECT.to_string(),
        body: "verify your address".to_string(),
        kind: "verification".to_string(),
        attempts: 1,
        generation: 1,
    }
}

/// What the drain would write for this outcome, on the first of twenty attempts — so
/// `Parked` here can only mean "permanent", never "out of attempts".
fn row_write(result: Result<(), SendError>) -> Disposition {
    disposition(result, 1, 20)
}

// ============================================================================
// 1. A permanent relay verdict parks the row.
// ============================================================================

/// A `550` over the wire is the ONE outcome that must stop retrying: `classify` reads
/// lettre's `is_permanent()` and `disposition` turns that into a park. Revert either leg —
/// classify to always-`Infra`, or disposition's `Rejected` arm to a retry — and the row
/// write below becomes `Retry`.
#[tokio::test]
async fn a_permanent_relay_verdict_maps_to_rejected_and_parks_the_row() {
    let relay = Relay::start(Script::AnswerEhlo(PERMANENT_REFUSAL)).await;
    let (sender, _) = smtp_sender(relay.addr, TlsMode::STARTTLS, SEND_TIMEOUT_MS);
    assert_eq!(sender.name(), SMTP, "the parked row records the provider name");

    let result = guarded_send(&sender).await;

    assert!(
        matches!(result, Err(SendError::Rejected(_))),
        "a 5xx from the relay is permanent, got {result:?}"
    );
    assert_eq!(relay.hits(), 1, "the dialogue reached the relay");
    assert!(
        relay.transcript().iter().any(|l| l == HELLO),
        "the envelope sender's domain must reach the wire, got {:?}",
        relay.transcript()
    );
    assert!(
        matches!(row_write(result), Disposition::Parked { .. }),
        "a permanent verdict parks the row for an operator instead of retrying it"
    );
}

// ============================================================================
// 2. Everything short of a verdict retries.
// ============================================================================

/// A relay that accepts the TCP connection and then disappears has said nothing about the
/// message. Parking it would drop a deliverable message on a socket error; the hit counter
/// is what proves the connection was made at all, so this is not a disguised refusal.
#[tokio::test]
async fn a_dropped_connection_maps_to_infra_and_retries() {
    let relay = Relay::start(Script::DropOnConnect).await;
    let (sender, _) = smtp_sender(relay.addr, TlsMode::STARTTLS, SEND_TIMEOUT_MS);

    let result = guarded_send(&sender).await;

    assert_infra(&result);
    assert_eq!(relay.hits(), 1, "the peer accepted the connection before dropping it");
    match row_write(result) {
        Disposition::Retry { backoff_secs: b, .. } => assert_eq!(b, backoff_secs(1)),
        other => panic!("a dropped connection must back off, got {other:?}"),
    }
}

/// Nothing is listening, by construction: the port was bound to reserve it and released.
/// A refused connection never produced a verdict either.
#[tokio::test]
async fn a_refused_connection_maps_to_infra_and_retries() {
    let addr = Relay::dead_address().await;
    let (sender, _) = smtp_sender(addr, TlsMode::STARTTLS, SEND_TIMEOUT_MS);

    let result = guarded_send(&sender).await;

    assert_infra(&result);
    match row_write(result) {
        Disposition::Retry { backoff_secs: b, .. } => assert_eq!(b, backoff_secs(1)),
        other => panic!("a refused connection must back off, got {other:?}"),
    }
}

/// The no-downgrade rule: a plaintext relay that answers EHLO with a positive `250` but
/// offers no STARTTLS gets nothing sent to it. That refusal is the sender's, not the
/// relay's, and it must back off rather than park — a relay is reconfigured, a message is
/// not. The transcript proves the `250` was parsed and the EHLO was really written.
#[tokio::test]
async fn a_relay_that_will_not_offer_starttls_is_infra_not_a_park() {
    let relay = Relay::start(Script::AnswerEhlo(CAPABILITIES_WITHOUT_STARTTLS)).await;
    let (sender, _) = smtp_sender(relay.addr, TlsMode::STARTTLS, SEND_TIMEOUT_MS);

    let result = guarded_send(&sender).await;

    assert_infra(&result);
    assert_eq!(relay.hits(), 1, "the dialogue reached the relay");
    assert!(
        relay.transcript().iter().any(|l| l == HELLO),
        "the client greeted before refusing the downgrade, got {:?}",
        relay.transcript()
    );
    assert!(
        matches!(row_write(result), Disposition::Retry { .. }),
        "an unencryptable relay is infrastructure, not an undeliverable message"
    );
}

/// `implicit` TLS against a peer that is not a TLS server fails in the handshake. That is
/// the fourth thing `classify`'s doc promises backs off, and the arm no other test reaches.
#[tokio::test]
async fn a_failed_implicit_tls_handshake_is_infra_not_a_park() {
    let relay = Relay::start(Script::DropOnConnect).await;
    let (sender, _) = smtp_sender(relay.addr, TlsMode::IMPLICIT, SEND_TIMEOUT_MS);

    let result = guarded_send(&sender).await;

    assert_infra(&result);
    assert_eq!(relay.hits(), 1, "the client got as far as the handshake");
    assert!(
        matches!(row_write(result), Disposition::Retry { .. }),
        "a TLS failure must back off — parking it would drop a deliverable message"
    );
}

// ============================================================================
// 3. The hang.
// ============================================================================

/// lettre's tokio transport bounds the TCP connect and NOTHING after it, so a relay that
/// accepts and then never writes its banner is stopped only by the drain's aggregate
/// `tokio::time::timeout` in [`attempt`]. Delete that timeout and this test hangs until the
/// guard fires; weaken `attempt`'s elapsed arm from `Infra` to `Rejected` and the row write
/// below becomes a park.
///
/// The pool is LAZY and never dialled — `attempt` performs no DB work, and a pool that
/// cannot answer is how that is proven rather than asserted.
#[tokio::test]
async fn a_relay_that_never_greets_is_stopped_by_the_configured_send_timeout() {
    let relay = Relay::start(Script::NeverGreet).await;
    let (sender, send_timeout) = smtp_sender(relay.addr, TlsMode::STARTTLS, SHORT_SEND_TIMEOUT_MS);
    let drain = Drain {
        pool: PgPool::connect_lazy(DEFAULT_DSN).expect("a lazy pool from a well-formed DSN"),
        sender,
        from: FROM.to_string(),
        send_timeout,
        max_attempts: 20,
    };
    let row = claimed();

    let result = tokio::time::timeout(HANG_GUARD, attempt(&drain, &row))
        .await
        .expect("the send budget must fire long before the hang guard");

    assert_infra(&result);
    let reason = format!("{:#}", result.as_ref().unwrap_err());
    assert!(
        reason.contains(&format!("send exceeded {SHORT_SEND_TIMEOUT_MS}ms")),
        "the drain's budget must be what fired, not lettre's connect bound: {reason:?}"
    );
    assert_eq!(relay.hits(), 1, "the connection was accepted — the hang is past the connect");
    assert!(
        matches!(row_write(result), Disposition::Retry { .. }),
        "an elapsed budget is infrastructure: the message may well be deliverable"
    );
}
