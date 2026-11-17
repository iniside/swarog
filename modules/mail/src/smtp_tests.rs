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

use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use sqlx::PgPool;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

use crate::config::{
    MailConfig, FROM_ENV, PROVIDER_ENV, SEND_TIMEOUT_ENV, SMTP_HOST_ENV, SMTP_PASSWORD_ENV,
    SMTP_PORT_ENV, SMTP_TLS_ENV, SMTP_USERNAME_ENV,
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
#[derive(Clone)]
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
    /// Greet in the clear advertising STARTTLS — and AUTH, as bait for a client that would
    /// authenticate before upgrading — answer `STARTTLS` with `220`, hand the raw socket to
    /// this acceptor, and speak the accepted dialogue inside the TLS session.
    StartTls(TlsAcceptor),
    /// The same dialogue with this acceptor wrapping the socket from the first byte.
    ImplicitTls(TlsAcceptor),
}

/// A `250` EHLO reply that advertises no STARTTLS: a positive multi-line response the
/// client parses successfully, followed by the sender's own refusal to downgrade.
const CAPABILITIES_WITHOUT_STARTTLS: &str = "250-fixture.invalid\r\n250 8BITMIME\r\n";

/// A permanent verdict delivered as early as EHLO.
const PERMANENT_REFUSAL: &str = "550 5.7.1 relay access denied\r\n";

const BANNER: &[u8] = b"220 fixture.invalid ESMTP\r\n";

/// The cleartext EHLO reply of [`Script::StartTls`]. AUTH is on offer BEFORE the upgrade,
/// so a client that would authenticate in the clear is given every chance to.
const CAPABILITIES_WITH_STARTTLS: &str =
    "250-fixture.invalid\r\n250-STARTTLS\r\n250-AUTH PLAIN\r\n250 8BITMIME\r\n";

/// The EHLO reply inside a TLS session: no STARTTLS (the session is already encrypted),
/// AUTH PLAIN on offer.
const CAPABILITIES_INSIDE_TLS: &str = "250-fixture.invalid\r\n250-AUTH PLAIN\r\n250 8BITMIME\r\n";

/// Not a client line: the fixture's own entry, pushed the moment its acceptor completes a
/// handshake. Everything after it in a transcript crossed the wire encrypted; everything
/// before it crossed in the clear.
const TLS_UP: &str = "<fixture: TLS handshake complete>";

/// The relay credential the TLS tests configure, and its `AUTH PLAIN` initial response:
/// base64 of `\0user\0pass`.
const USERNAME: &str = "user";
const PASSWORD: &str = "pass";
const AUTH_PLAIN_LINE: &str = "AUTH PLAIN AHVzZXIAcGFzcw==";

/// The certificate's subject alternative name, and the host every TLS test dials — an IP
/// literal, which lettre turns into an IP `ServerName` that the leaf's IP SAN must match.
const RELAY_HOST: &str = "127.0.0.1";

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
                match &script {
                    Script::DropOnConnect => drop(stream),
                    Script::NeverGreet => held.push(stream),
                    Script::AnswerEhlo(reply) => {
                        converse(stream, true, reply, None, &recorded).await;
                    }
                    Script::RecordFirstBytesThenClose => record_bytes(stream, &recorded).await,
                    Script::Dialogue(replies) => {
                        converse(
                            stream,
                            true,
                            CAPABILITIES_WITHOUT_STARTTLS,
                            Some(*replies),
                            &recorded,
                        )
                        .await;
                    }
                    Script::StartTls(acceptor) => {
                        let upgrade = converse(
                            stream,
                            true,
                            CAPABILITIES_WITH_STARTTLS,
                            Some(ACCEPTED),
                            &recorded,
                        )
                        .await;
                        if let Some(stream) = upgrade {
                            // No banner: the client re-issues EHLO on the fresh session.
                            serve_tls(acceptor, stream, false, &recorded).await;
                        }
                    }
                    Script::ImplicitTls(acceptor) => {
                        serve_tls(acceptor, stream, true, &recorded).await;
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

/// Greets (when `greet`), records every line the client writes, and answers it. Each line
/// is recorded BEFORE its reply is written, so a client that observed a reply necessarily
/// left its command in the transcript first — a happens-before, not a race.
///
/// Generic over the stream so the same dialogue runs over a bare socket and inside a TLS
/// session. A `STARTTLS` is answered `220` and ends the conversation by handing the stream
/// BACK, reunited, for the caller to upgrade; every other ending returns `None`.
///
/// With `replies` absent the fixture answers the first line with `ehlo_reply` and every
/// later one with `221`: lettre answers a rejected EHLO by sending `QUIT` and BLOCKING on
/// its response, so a fixture that went silent there would be held open until the send
/// budget expired.
async fn converse<S>(
    stream: S,
    greet: bool,
    ehlo_reply: &str,
    replies: Option<Replies>,
    transcript: &Arc<Mutex<Vec<String>>>,
) -> Option<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (rx, mut tx) = tokio::io::split(stream);
    if greet && !answer(&mut tx, BANNER).await {
        return None;
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
            if !answer(&mut tx, reply).await {
                return None;
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
            if !answer(&mut tx, replies.data.as_bytes()).await {
                return None;
            }
            continue;
        }
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("STARTTLS") {
            if !answer(&mut tx, b"220 2.0.0 ready to start TLS\r\n").await {
                return None;
            }
            let rx = lines.into_inner();
            // The client waits for the `220` before it writes its ClientHello, so nothing
            // past the command can have been read ahead; a non-empty buffer would mean
            // handshake bytes were swallowed by the line reader.
            assert!(rx.buffer().is_empty(), "bytes after STARTTLS were read as SMTP lines");
            return Some(rx.into_inner().unsplit(tx));
        }
        let reply: &[u8] = if upper.starts_with("EHLO") {
            ehlo_reply.as_bytes()
        } else if upper.starts_with("AUTH") {
            b"235 2.7.0 authentication successful\r\n"
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
        if !answer(&mut tx, reply).await {
            return None;
        }
    }
    None
}

/// Writes and FLUSHES a reply: a TLS stream may hold written records until flushed, and a
/// reply the client never receives would stall the dialogue until the hang guard.
async fn answer<W: AsyncWrite + Unpin>(tx: &mut W, reply: &[u8]) -> bool {
    tx.write_all(reply).await.is_ok() && tx.flush().await.is_ok()
}

/// Runs the server side of a handshake on `stream` and, if it completes, marks the
/// transcript with [`TLS_UP`] and speaks the accepted dialogue inside the session. A
/// handshake the client abandons — a certificate it refused — ends the connection with no
/// marker, which is what lets a test tell "never encrypted" from "encrypted, then failed".
async fn serve_tls(
    acceptor: &TlsAcceptor,
    stream: TcpStream,
    greet: bool,
    transcript: &Arc<Mutex<Vec<String>>>,
) {
    let Ok(session) = acceptor.accept(stream).await else {
        return;
    };
    transcript.lock().expect("the transcript lock").push(TLS_UP.to_string());
    converse(session, greet, CAPABILITIES_INSIDE_TLS, Some(ACCEPTED), transcript).await;
}

/// A minted trust chain: the CA a test client may trust, and an acceptor serving a leaf
/// that CA signed.
struct Minted {
    ca_pem: String,
    acceptor: TlsAcceptor,
}

/// Mints a CA and a leaf for `san`, signed by it. The two get DISTINCT common names: a
/// leaf whose subject equals its issuer's reads as self-issued, which is what produced a
/// Windows `TRUST_E_CERT_SIGNATURE` elsewhere in this repo. The server config names the
/// ring provider explicitly instead of installing a process default that would leak into
/// every other test in the binary.
fn mint(san: &str) -> Minted {
    use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose};

    let ca_key = KeyPair::generate().expect("a CA key");
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA parameters");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params.distinguished_name.push(DnType::CommonName, "mail smtp_tests CA");
    let ca = ca_params.self_signed(&ca_key).expect("the self-signed CA");

    let leaf_key = KeyPair::generate().expect("a leaf key");
    let mut leaf_params = CertificateParams::new(vec![san.to_string()]).expect("leaf parameters");
    leaf_params.distinguished_name.push(DnType::CommonName, "mail smtp_tests relay");
    let leaf = leaf_params.signed_by(&leaf_key, &ca, &ca_key).expect("the CA-signed leaf");

    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("the ring provider supports the default versions")
    .with_no_client_auth()
    .with_single_cert(vec![leaf.der().clone()], key)
    .expect("the leaf and its key form a server identity");
    Minted {
        ca_pem: ca.pem(),
        acceptor: TlsAcceptor::from(Arc::new(config)),
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
    smtp_sender_with(addr, tls, send_timeout_ms, None)
}

/// [`smtp_sender`] with the relay credential set through its two configuration variables,
/// as an operator would.
fn smtp_sender_with(
    addr: SocketAddr,
    tls: &str,
    send_timeout_ms: u64,
    credentials: Option<(&str, &str)>,
) -> (Arc<dyn Sender>, Duration) {
    let port = addr.port().to_string();
    let ms = send_timeout_ms.to_string();
    let mut pairs = vec![
        (PROVIDER_ENV, SMTP),
        (FROM_ENV, FROM),
        (SMTP_HOST_ENV, RELAY_HOST),
        (SMTP_PORT_ENV, port.as_str()),
        (SMTP_TLS_ENV, tls),
        (SEND_TIMEOUT_ENV, ms.as_str()),
    ];
    if let Some((username, password)) = credentials {
        pairs.push((SMTP_USERNAME_ENV, username));
        pairs.push((SMTP_PASSWORD_ENV, password));
    }
    let vars: BTreeMap<String, String> = pairs
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

/// The same `SmtpSender` over TLS that trusts ONLY the minted CA, carrying the test
/// credential — the dial on which the handshake, and so `AUTH`, can complete.
fn trusting_sender(addr: SocketAddr, mode: TlsMode, minted: &Minted) -> Arc<dyn Sender> {
    Arc::new(
        SmtpSender::tls_trusting_for_tests(
            RELAY_HOST,
            addr.port(),
            FROM,
            Duration::from_millis(SEND_TIMEOUT_MS),
            mode,
            minted.ca_pem.as_bytes(),
            Some((USERNAME.to_string(), PASSWORD.to_string())),
        )
        .expect("the trusting transport builds"),
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

// ============================================================================
// 5. TLS negotiated: the certificate check, the upgrade, and AUTH inside it.
// ============================================================================

/// The rustls verdict that ended a handshake, dug out of the send error: tokio-rustls
/// carries it as the payload of an `io::Error`, which lettre wraps as the error's source.
/// `io::Error::source` skips its own payload, so the payload is read with `get_ref`.
fn rustls_failure(result: &Result<(), SendError>) -> Option<rustls::Error> {
    let Err(SendError::Infra(e) | SendError::Rejected(e)) = result else {
        return None;
    };
    e.chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())
        .and_then(|io| io.get_ref())
        .and_then(|payload| payload.downcast_ref::<rustls::Error>())
        .cloned()
}

/// Every transcript line the client wrote as an `AUTH` command, by position.
fn auth_lines(transcript: &[String]) -> Vec<usize> {
    transcript
        .iter()
        .enumerate()
        .filter(|(_, l)| l.to_ascii_uppercase().starts_with("AUTH"))
        .map(|(i, _)| i)
        .collect()
}

/// The part of a dialogue that has to happen INSIDE the session that opened at
/// `tls_up`: a fresh EHLO first, then exactly one `AUTH PLAIN` carrying the configured
/// credential, then the envelope — and the relay's `250` for the message, which the
/// sender reported as a delivery.
fn assert_authenticated_delivery_inside_tls(
    result: Result<(), SendError>,
    transcript: &[String],
    tls_up: usize,
) {
    assert!(result.is_ok(), "the relay accepted the message, got {result:?} over {transcript:?}");
    assert_eq!(
        transcript.get(tls_up + 1).map(String::as_str),
        Some(HELLO),
        "the client greets the encrypted session afresh: {transcript:?}"
    );
    let auths = auth_lines(transcript);
    assert_eq!(auths.len(), 1, "exactly one AUTH command, got {transcript:?}");
    let auth_at = auths[0];
    assert!(
        auth_at > tls_up,
        "AUTH must only ever cross the encrypted session, got {transcript:?}"
    );
    assert_eq!(
        transcript[auth_at], AUTH_PLAIN_LINE,
        "AUTH PLAIN carries base64 of \0user\0pass"
    );
    let mail_at = transcript
        .iter()
        .position(|l| l.starts_with(&format!("MAIL FROM:<{FROM}>")))
        .unwrap_or_else(|| panic!("the envelope reached the relay: {transcript:?}"));
    assert!(mail_at > auth_at, "the envelope follows authentication: {transcript:?}");
    assert!(
        transcript.iter().any(|l| l == &format!("Subject: {SUBJECT}")),
        "the message itself crossed the session: {transcript:?}"
    );
    assert_eq!(row_write(result), Disposition::Sent);
}

/// The deployed constructor VERIFIES the relay's certificate. The fixture advertises
/// STARTTLS and AUTH and would complete either; the certificate it serves is signed by a CA
/// minted here, which no `webpki-roots` anchor issued. So the dialogue must stop at the
/// handshake — after `STARTTLS`, before any `AUTH` or envelope — and the reason must be
/// rustls refusing the issuer, not some other failure along the way.
///
/// What this pins: `SmtpSender::new` under `starttls` performs the upgrade (the `STARTTLS`
/// line), checks the certificate against a trust store that does not hold our CA (the
/// `UnknownIssuer` verdict), and sends the credential nowhere when that fails (no `AUTH`).
/// A sender that skipped TLS would put `AUTH` and `MAIL FROM` on the wire in the clear —
/// the fixture offers AUTH before the upgrade precisely to catch that.
#[tokio::test]
async fn the_production_starttls_dial_refuses_an_untrusted_certificate_before_auth() {
    let minted = mint(RELAY_HOST);
    let relay = Relay::start(Script::StartTls(minted.acceptor.clone())).await;
    let (sender, _) = smtp_sender_with(
        relay.addr,
        TlsMode::STARTTLS,
        SEND_TIMEOUT_MS,
        Some((USERNAME, PASSWORD)),
    );

    let result = guarded_send(&sender).await;

    let transcript = relay.transcript();
    assert_eq!(relay.hits(), 1, "the client did dial the relay");
    assert!(
        transcript.iter().any(|l| l == "STARTTLS"),
        "the production dial must ask to upgrade, got {transcript:?}"
    );
    assert!(
        auth_lines(&transcript).is_empty(),
        "no credential may leave the client when the certificate is refused, got {transcript:?}"
    );
    assert!(
        !transcript.iter().any(|l| l.starts_with("MAIL FROM")),
        "no envelope may leave the client when the certificate is refused, got {transcript:?}"
    );
    assert!(
        !relay.wrote(TLS_UP),
        "the handshake must not complete on an untrusted certificate, got {transcript:?}"
    );
    assert_eq!(
        rustls_failure(&result),
        Some(rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer)),
        "the certificate check is what stopped the dial, got {result:?}"
    );
    assert_infra(&result);
    assert!(
        matches!(row_write(result), Disposition::Retry { .. }),
        "a relay whose certificate is refused is infrastructure, not an undeliverable message"
    );
}

/// STARTTLS completed end to end against a relay whose CA the client trusts: the only
/// things in the clear are the first EHLO and the `STARTTLS` itself, and `AUTH PLAIN` — the
/// credential lettre sends only after a successful upgrade — crosses inside the session,
/// followed by a delivered message.
#[tokio::test]
async fn starttls_upgrades_before_auth_and_delivers_inside_the_session() {
    let minted = mint(RELAY_HOST);
    let relay = Relay::start(Script::StartTls(minted.acceptor.clone())).await;
    let sender = trusting_sender(relay.addr, TlsMode::StartTls, &minted);

    let result = guarded_send(&sender).await;

    let transcript = relay.transcript();
    assert_eq!(relay.hits(), 1, "exactly one connection carried the message");
    let tls_up = transcript
        .iter()
        .position(|l| l == TLS_UP)
        .unwrap_or_else(|| panic!("the handshake completed: {result:?} over {transcript:?}"));
    assert_eq!(
        &transcript[..tls_up],
        &[HELLO.to_string(), "STARTTLS".to_string()],
        "only the greeting and the upgrade request may cross in the clear"
    );
    assert_authenticated_delivery_inside_tls(result, &transcript, tls_up);
}

/// Implicit TLS completed end to end: the acceptor owns the socket from the first byte,
/// so the session marker is the transcript's FIRST entry — no SMTP line, not even the
/// greeting, preceded the handshake — and no `STARTTLS` is ever issued.
#[tokio::test]
async fn implicit_tls_encrypts_from_the_first_byte_and_authenticates_inside_the_session() {
    let minted = mint(RELAY_HOST);
    let relay = Relay::start(Script::ImplicitTls(minted.acceptor.clone())).await;
    let sender = trusting_sender(relay.addr, TlsMode::Implicit, &minted);

    let result = guarded_send(&sender).await;

    let transcript = relay.transcript();
    assert_eq!(relay.hits(), 1, "exactly one connection carried the message");
    assert_eq!(
        transcript.first().map(String::as_str),
        Some(TLS_UP),
        "the handshake precedes every SMTP line: {result:?} over {transcript:?}"
    );
    assert!(
        !transcript.iter().any(|l| l == "STARTTLS"),
        "an implicit session never asks to upgrade: {transcript:?}"
    );
    assert_authenticated_delivery_inside_tls(result, &transcript, 0);
}
