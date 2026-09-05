//! The `smtp` provider driven against a REAL socket: a loopback relay on an ephemeral
//! port, a hit counter and the lines the client actually wrote. The counter is the point —
//! it turns "did the I/O happen?" into an assertion instead of an absence of errors, the
//! `serve_counting_jwks` instrument from `modules/accounts/src/oidc_tests.rs`.
//!
//! Two constructors are exercised, and which one a test uses is load-bearing. The
//! PRODUCTION one ([`SmtpSender::new`], reached through `MailConfig`) decides between
//! `starttls_relay` and `relay`, and against a plaintext peer both abandon the dialogue at
//! EHLO — so those tests pin the TLS choice and the error taxonomy, and can never reach a
//! delivery. The whole dialogue is reached through [`SmtpSender::plaintext_for_tests`], a
//! `#[cfg(test)]` constructor that adds no production surface and no third [`TlsMode`] arm.
//!
//! Two gaps, stated so Step 13 copies them rather than paraphrasing:
//!
//! 1. **TLS is never NEGOTIATED here at all** — no handshake completes, so nothing proves
//!    the STARTTLS upgrade, the certificate/trust check (`webpki-roots`, which is also why
//!    a self-signed loopback TLS fixture cannot be validated), or SMTP `AUTH`, which lettre
//!    applies only after a successful upgrade. What IS pinned is the CHOICE between `relay`
//!    and `starttls_relay`, and that an implicit dial emits no cleartext command.
//! 2. **A relay that answers `5xx` and then goes SILENT is reported as infrastructure.**
//!    lettre answers a rejected command by sending `QUIT` and blocking in `read_response`,
//!    which nothing in lettre bounds, so `worker::attempt`'s budget is what returns — as
//!    `Infra` carrying `"send exceeded …"`. The row then spends its whole attempt ladder
//!    before parking with a timeout reason instead of the relay's own `550`. The bound is
//!    genuinely absorbed (the fixture's `221`-after-`550` exists for exactly this reason),
//!    so this is a reporting gap, not a hang.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::PgPool;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::config::{
    MailConfig, FROM_ENV, PROVIDER_ENV, SEND_TIMEOUT_ENV, SMTP_HOST_ENV, SMTP_PORT_ENV,
    SMTP_TLS_ENV,
};
use crate::providers::{Outgoing, SendError, Sender, SMTP};
use crate::smtp::{SmtpSender, TlsMode};
use crate::store::{Claimed, Disposition};
use crate::tests::DEFAULT_DSN;
use crate::worker::{attempt, backoff_secs, disposition, Drain};

/// The envelope sender. Its DOMAIN is what [`crate::smtp`] puts in the EHLO, and the string
/// a cleartext-leak assertion looks for.
const FROM: &str = "noreply@mail-fixture.test";
const FROM_DOMAIN: &str = "mail-fixture.test";
const HELLO: &str = "EHLO mail-fixture.test";
const RECIPIENT: &str = "player@example.com";
const SUBJECT: &str = "Verify your address";
const BODY: &str = "verify your address";

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

/// The replies of a fixture that speaks the whole dialogue.
#[derive(Clone, Copy)]
struct Replies {
    /// The answer to `RCPT TO` — a `5xx` here is a relay refusing the recipient.
    rcpt: &'static str,
    /// The answer to the message body's terminating `.`.
    data: &'static str,
}

const ACCEPTED: Replies = Replies {
    rcpt: "250 2.1.5 ok\r\n",
    data: "250 2.0.0 Ok: queued as ABC123\r\n",
};

const RECIPIENT_REFUSED: Replies = Replies {
    rcpt: "550 5.1.1 no such user here\r\n",
    data: "250 2.0.0 Ok: queued\r\n",
};

/// What the fixture does with a connection it accepted.
#[derive(Clone, Copy)]
enum Script {
    /// Close the moment the peer connects — the relay that vanishes mid-dialogue.
    DropOnConnect,
    /// Hold the socket open and never write a banner — the hang the send budget exists for,
    /// and the case an error-free assertion would miss entirely.
    NeverGreet,
    /// Greet with `220`, then answer the client's EHLO with this literal reply and go no
    /// further.
    AnswerEhlo(&'static str),
    /// Greet with `220`, record the FIRST bytes the client writes verbatim, then close.
    /// Bytes, not lines: the client under this script is mid-TLS-handshake and its
    /// ClientHello is binary, so a line-oriented read would be a coin flip on whether it
    /// contains a newline.
    RecordFirstBytesThenClose,
    /// Speak the whole plaintext dialogue through `DATA`.
    Dialogue(Replies),
}

/// A `250` EHLO reply that advertises no STARTTLS: a positive multi-line response the
/// client parses successfully, followed by the sender's own refusal to downgrade.
const CAPABILITIES_WITHOUT_STARTTLS: &str = "250-fixture.invalid\r\n250 8BITMIME\r\n";

/// A permanent verdict delivered as early as EHLO.
const PERMANENT_REFUSAL: &str = "550 5.7.1 relay access denied\r\n";

const BANNER: &[u8] = b"220 fixture.invalid ESMTP\r\n";

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
            // Connections are handled inline: one send opens exactly one of them (lettre's
            // `pool` feature is off), and holding them here is what keeps `NeverGreet`
            // hanging rather than resetting.
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                match script {
                    Script::DropOnConnect => drop(stream),
                    Script::NeverGreet => held.push(stream),
                    Script::AnswerEhlo(reply) => converse(stream, reply, None, &recorded).await,
                    Script::RecordFirstBytesThenClose => record_bytes(stream, &recorded).await,
                    Script::Dialogue(replies) => {
                        converse(
                            stream,
                            CAPABILITIES_WITHOUT_STARTTLS,
                            Some(replies),
                            &recorded,
                        )
                        .await
                    }
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

    fn wrote(&self, needle: &str) -> bool {
        self.transcript().iter().any(|l| l.contains(needle))
    }
}

/// Greets, records every line the client writes, and answers it. Each line is recorded
/// BEFORE its reply is written, so a client that observed a reply necessarily left its
/// command in the transcript first — a happens-before, not a race.
///
/// With `replies` absent the fixture answers the first line with `ehlo_reply` and every
/// later one with `221`: lettre answers a rejected EHLO by sending `QUIT` and BLOCKING on
/// its response, so a fixture that went silent there would be held open until the send
/// budget expired.
async fn converse(
    stream: TcpStream,
    ehlo_reply: &str,
    replies: Option<Replies>,
    transcript: &Arc<Mutex<Vec<String>>>,
) {
    let (rx, mut tx) = stream.into_split();
    if tx.write_all(BANNER).await.is_err() {
        return;
    }
    let mut lines = BufReader::new(rx).lines();
    let mut greeted = false;
    let mut in_body = false;
    while let Ok(Some(line)) = lines.next_line().await {
        transcript.lock().expect("the transcript lock").push(line.clone());
        let Some(replies) = replies else {
            let reply: &[u8] = if greeted {
                b"221 fixture.invalid closing connection\r\n"
            } else {
                greeted = true;
                ehlo_reply.as_bytes()
            };
            if tx.write_all(reply).await.is_err() {
                return;
            }
            continue;
        };
        // Inside DATA every line is message content until the lone `.` terminator; only
        // then does the relay pronounce on the message.
        if in_body {
            if line != "." {
                continue;
            }
            in_body = false;
            if tx.write_all(replies.data.as_bytes()).await.is_err() {
                return;
            }
            continue;
        }
        let upper = line.to_ascii_uppercase();
        let reply: &[u8] = if upper.starts_with("EHLO") {
            ehlo_reply.as_bytes()
        } else if upper.starts_with("RCPT") {
            replies.rcpt.as_bytes()
        } else if upper.starts_with("DATA") {
            in_body = true;
            b"354 end data with <CR><LF>.<CR><LF>\r\n"
        } else if upper.starts_with("QUIT") {
            b"221 fixture.invalid closing connection\r\n"
        } else {
            b"250 2.1.0 ok\r\n"
        };
        if tx.write_all(reply).await.is_err() {
            return;
        }
    }
}

/// Greets, then records the first bytes the peer wrote — lossily, because under an implicit
/// TLS dial they are a ClientHello — and closes. Closing is what makes the client's failure
/// deterministic instead of dependent on parsing binary as SMTP.
async fn record_bytes(stream: TcpStream, transcript: &Arc<Mutex<Vec<String>>>) {
    let (mut rx, mut tx) = stream.into_split();
    if tx.write_all(BANNER).await.is_err() {
        return;
    }
    let mut buf = [0u8; 2048];
    if let Ok(n) = rx.read(&mut buf).await {
        transcript
            .lock()
            .expect("the transcript lock")
            .push(String::from_utf8_lossy(&buf[..n]).into_owned());
    }
}

// ============================================================================
// The senders under test.
// ============================================================================

/// The PRODUCTION construction path: `MailConfig::from_vars` then `Provider::sender` — the
/// same two calls `lib.rs` makes — so the configuration authority decides the TLS mode and
/// the connect bound is the send budget, exactly as deployed.
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

/// The same `SmtpSender`, dialled in the clear. Test-only, and the only way the dialogue
/// past the upgrade is reachable at all.
fn plaintext_sender(addr: SocketAddr) -> Arc<dyn Sender> {
    Arc::new(
        SmtpSender::plaintext_for_tests(
            "127.0.0.1",
            addr.port(),
            FROM,
            Duration::from_millis(SEND_TIMEOUT_MS),
        )
        .expect("the plaintext transport builds"),
    )
}

async fn send_to(sender: &Arc<dyn Sender>, to: &str) -> Result<(), SendError> {
    sender
        .send(&Outgoing {
            from: FROM,
            to,
            subject: SUBJECT,
            body: BODY,
            kind: "verification",
        })
        .await
}

/// The send under [`HANG_GUARD`]: nothing here may depend on a real clock, but a wedged
/// dialogue must fail the test rather than stall the binary.
async fn guarded_send(sender: &Arc<dyn Sender>) -> Result<(), SendError> {
    guarded_send_to(sender, RECIPIENT).await
}

async fn guarded_send_to(sender: &Arc<dyn Sender>, to: &str) -> Result<(), SendError> {
    tokio::time::timeout(HANG_GUARD, send_to(sender, to))
        .await
        .expect("the sender must not hang past the guard")
}

fn assert_infra(result: &Result<(), SendError>) {
    assert!(
        matches!(result, Err(SendError::Infra(_))),
        "a message that never reached a verdict must be transient, got {result:?}"
    );
}

/// The whole reason string an operator would read off the parked row.
fn reason(result: &Result<(), SendError>) -> String {
    format!("{:#}", result.as_ref().expect_err("an error was expected"))
}

/// A claimed row on its first attempt — the input [`disposition`] turns into a row write.
fn claimed() -> Claimed {
    Claimed {
        id: "00000000-0000-0000-0000-000000000001".to_string(),
        recipient: RECIPIENT.to_string(),
        subject: SUBJECT.to_string(),
        body: BODY.to_string(),
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
// 1. The whole dialogue, over the test-only plaintext dial.
// ============================================================================

/// The delivered path: `MAIL FROM`, `RCPT TO`, `DATA` and a `250` become `Ok` and a `sent`
/// row. The transcript is the assertion — the envelope's sender, its recipient, the subject
/// and the body all have to appear on the wire, which is what proves
/// `SmtpSender::build_message` renders the `Outgoing` it was handed rather than something
/// else. Blank the `.to()` or the `.subject()` in that builder and this fails.
#[tokio::test]
async fn the_whole_dialogue_delivers_and_puts_the_envelope_subject_and_body_on_the_wire() {
    let relay = Relay::start(Script::Dialogue(ACCEPTED)).await;
    let sender = plaintext_sender(relay.addr);

    let result = guarded_send(&sender).await;

    assert!(result.is_ok(), "a 250 at DATA is a delivery, got {result:?}");
    assert_eq!(relay.hits(), 1, "exactly one connection carried the message");
    assert!(relay.wrote(HELLO), "the EHLO names the envelope sender's domain");
    assert!(
        relay.wrote(&format!("MAIL FROM:<{FROM}>")),
        "the envelope sender must reach the wire, got {:?}",
        relay.transcript()
    );
    assert!(
        relay.wrote(&format!("RCPT TO:<{RECIPIENT}>")),
        "the recipient must reach the wire, got {:?}",
        relay.transcript()
    );
    assert!(
        relay.wrote(&format!("Subject: {SUBJECT}")),
        "the subject must reach the wire, got {:?}",
        relay.transcript()
    );
    assert!(relay.wrote(BODY), "the body must reach the wire");
    assert_eq!(row_write(result), Disposition::Sent);
}

/// The realistic permanent verdict: the relay takes the envelope and refuses the RECIPIENT.
/// `classify` reads lettre's `is_permanent()` and `disposition` turns that into a park —
/// force `classify` to always-`Infra` and this row write becomes a retry instead.
#[tokio::test]
async fn a_relay_that_refuses_the_recipient_parks_the_row() {
    let relay = Relay::start(Script::Dialogue(RECIPIENT_REFUSED)).await;
    let sender = plaintext_sender(relay.addr);

    let result = guarded_send(&sender).await;

    assert!(
        matches!(result, Err(SendError::Rejected(_))),
        "a 5xx at RCPT is permanent, got {result:?}"
    );
    assert!(
        reason(&result).contains("no such user here"),
        "the parked row carries the relay's own words: {:?}",
        reason(&result)
    );
    assert!(relay.wrote(&format!("RCPT TO:<{RECIPIENT}>")), "the refusal was of OUR recipient");
    assert!(
        matches!(row_write(result), Disposition::Parked { .. }),
        "a permanent verdict parks the row for an operator instead of retrying it"
    );
}

/// The FIRST failure arm of `SmtpSender::send`, before any socket: a recipient the parser
/// refuses can never be accepted by any relay, so it must park rather than burn twenty
/// attempts against one. `hits() == 0` against a LIVE relay is the proof that nothing was
/// dialled — an absent connection, not an absent error.
#[tokio::test]
async fn a_malformed_recipient_parks_without_opening_a_socket() {
    let relay = Relay::start(Script::Dialogue(ACCEPTED)).await;
    let (sender, _) = smtp_sender(relay.addr, TlsMode::STARTTLS, SEND_TIMEOUT_MS);

    let result = guarded_send_to(&sender, "not-an-address").await;

    assert!(
        matches!(result, Err(SendError::Rejected(_))),
        "an unparseable recipient is permanently undeliverable, got {result:?}"
    );
    assert!(
        reason(&result).contains("recipient"),
        "the parked row must name WHICH address was refused: {:?}",
        reason(&result)
    );
    assert_eq!(relay.hits(), 0, "a message this broken must never reach a relay");
    assert!(
        matches!(row_write(result), Disposition::Parked { .. }),
        "a message no relay can accept parks instead of retrying"
    );
}

// ============================================================================
// 2. The TLS choice, over the production constructor.
// ============================================================================

/// `implicit` means SMTPS: TLS from the first byte, and NO SMTP command may appear in the
/// clear. The relay here would happily conduct a plaintext dialogue — it greets and
/// advertises capabilities — so the only thing stopping one is `SmtpSender::new` picking
/// `relay` over `starttls_relay`. Swap that arm and the plaintext `EHLO` lands in the
/// transcript and this fails; nothing else in this file observes the choice.
#[tokio::test]
async fn an_implicit_dial_never_lets_an_smtp_command_out_in_the_clear() {
    let relay = Relay::start(Script::RecordFirstBytesThenClose).await;
    let (sender, _) = smtp_sender(relay.addr, TlsMode::IMPLICIT, SEND_TIMEOUT_MS);

    let result = guarded_send(&sender).await;

    assert_eq!(relay.hits(), 1, "the client did dial the relay");
    assert!(
        !relay.wrote(FROM_DOMAIN),
        "nothing about this envelope may cross an unencrypted socket, got {:?}",
        relay.transcript()
    );
    assert_infra(&result);
    assert!(
        matches!(row_write(result), Disposition::Retry { .. }),
        "a connection that never got encrypted is infrastructure, not an undeliverable message"
    );
}

/// The no-downgrade rule: a relay that answers EHLO with a positive `250` but offers no
/// STARTTLS gets nothing sent to it. The refusal is the SENDER's, and the reason string is
/// what distinguishes it from any other pre-verdict failure — a garbled reply would also be
/// `Infra`/`Retry` with an `EHLO` in the transcript, so the variant alone pins nothing.
#[tokio::test]
async fn a_relay_that_will_not_offer_starttls_is_refused_by_the_sender_and_retried() {
    let relay = Relay::start(Script::AnswerEhlo(CAPABILITIES_WITHOUT_STARTTLS)).await;
    let (sender, _) = smtp_sender(relay.addr, TlsMode::STARTTLS, SEND_TIMEOUT_MS);

    let result = guarded_send(&sender).await;

    assert_infra(&result);
    assert!(
        reason(&result).contains("STARTTLS is not supported on this server"),
        "the sender's own refusal to downgrade, not some other pre-verdict failure: {:?}",
        reason(&result)
    );
    assert_eq!(relay.hits(), 1, "the dialogue reached the relay");
    assert!(relay.wrote(HELLO), "the client greeted before refusing the downgrade");
    assert!(
        matches!(row_write(result), Disposition::Retry { .. }),
        "an unencryptable relay is infrastructure, not an undeliverable message"
    );
}

// ============================================================================
// 3. Everything short of a verdict retries.
// ============================================================================

/// A `550` as early as the greeting is still a verdict, and still permanent. This is the
/// only permanent case that runs on the PRODUCTION constructor — the recipient refusal
/// above needs the plaintext dial to get as far as `RCPT`.
#[tokio::test]
async fn a_permanent_verdict_over_the_production_dial_parks_the_row() {
    let relay = Relay::start(Script::AnswerEhlo(PERMANENT_REFUSAL)).await;
    let (sender, _) = smtp_sender(relay.addr, TlsMode::STARTTLS, SEND_TIMEOUT_MS);
    assert_eq!(sender.name(), SMTP, "the parked row records the provider name");

    let result = guarded_send(&sender).await;

    assert!(
        matches!(result, Err(SendError::Rejected(_))),
        "a 5xx from the relay is permanent, got {result:?}"
    );
    assert_eq!(relay.hits(), 1, "the dialogue reached the relay");
    assert!(relay.wrote(HELLO), "the envelope sender's domain reached the wire");
    assert!(
        matches!(row_write(result), Disposition::Parked { .. }),
        "a permanent verdict parks the row for an operator instead of retrying it"
    );
}

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

// ============================================================================
// 4. The hang.
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
    assert!(
        reason(&result).contains(&format!("send exceeded {SHORT_SEND_TIMEOUT_MS}ms")),
        "the drain's budget must be what fired, not lettre's connect bound: {:?}",
        reason(&result)
    );
    assert_eq!(relay.hits(), 1, "the connection was accepted — the hang is past the connect");
    assert!(
        matches!(row_write(result), Disposition::Retry { .. }),
        "an elapsed budget is infrastructure: the message may well be deliverable"
    );
}
