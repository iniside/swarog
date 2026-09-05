//! The `smtp` provider: a real relay behind the same [`Sender`] face the dev sink
//! implements.
//!
//! **The drain's aggregate `MAIL_SEND_TIMEOUT_MS` is the only bound on the dialogue.**
//! lettre's tokio transport applies its `timeout` around each candidate address's TCP
//! connect and NOWHERE else (`client/async_net.rs`'s `connect_tokio1`); the per-socket
//! read/write timeouts exist only on its blocking transport, so DNS, the TLS handshake,
//! the banner, EHLO, STARTTLS, AUTH and MAIL/RCPT/DATA are each unbounded here. A relay
//! that connects in 50ms and then trickles the banner is stopped by the drain's
//! `tokio::time::timeout` and by nothing else. The connect bound is still worth setting —
//! it is applied PER RESOLVED ADDRESS, so an MX with N records could otherwise spend N
//! times as long in the connect phase alone.

use std::time::Duration;

use async_trait::async_trait;
use lettre::message::header::ContentType;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::extension::ClientId;
use lettre::{Address, AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::address::parse_address;
use crate::providers::{Outgoing, SendError, Sender, SMTP};

/// How the connection is encrypted. There is no plaintext arm: this channel carries
/// verification links and reset tokens, and an unencrypted submission would put them on
/// the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlsMode {
    /// Connect in the clear on the submission port, then upgrade with STARTTLS. lettre's
    /// `Tls::Required` refuses to send anything if the upgrade fails, so this is not a
    /// downgrade path.
    StartTls,
    /// TLS from the first byte (SMTPS / implicit TLS).
    Implicit,
}

impl TlsMode {
    pub const STARTTLS: &'static str = "starttls";
    pub const IMPLICIT: &'static str = "implicit";

    /// The name → mode authority; `None` is an unknown spelling, which the config parse
    /// turns into a startup failure naming [`TlsMode::NAMES`].
    pub fn from_name(name: &str) -> Option<TlsMode> {
        match name {
            Self::STARTTLS => Some(TlsMode::StartTls),
            Self::IMPLICIT => Some(TlsMode::Implicit),
            _ => None,
        }
    }

    pub const NAMES: &'static [&'static str] = &[Self::STARTTLS, Self::IMPLICIT];
}

/// A relay credential. Its `Debug` prints a placeholder: [`crate::MailConfig`] derives
/// `Debug`, so an operator password would otherwise reach any `{:?}` of the whole
/// configuration — a log line, a panic message, an admin cell.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Secret {
        Secret(value)
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// The validated relay coordinates, produced by [`crate::config`] and nothing else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmtpSettings {
    pub host: String,
    pub port: u16,
    pub tls: TlsMode,
    /// `None` is an unauthenticated relay (an internal MTA that authorizes by network).
    /// Half a credential is refused by the parse, never carried here.
    pub credentials: Option<(String, Secret)>,
}

/// The domain an EHLO announces. Derived from the envelope sender because lettre's
/// default without the `hostname` feature is the literal `localhost`, which strict relays
/// reject. The address is already parsed, so a domain always exists.
fn hello_domain(from: &Address) -> String {
    from.domain().to_string()
}

/// Whether a relay verdict is permanent. Taken as DATA rather than as the lettre error so
/// the mapping is provable without a relay: only a permanent response parks a row — a
/// timeout, a TLS failure, a refused connection or a transient 4xx must back off, because
/// parking them drops a deliverable message on a transport hiccup.
pub(crate) fn classify(permanent: bool, detail: anyhow::Error) -> SendError {
    if permanent {
        SendError::Rejected(detail)
    } else {
        SendError::Infra(detail)
    }
}

pub struct SmtpSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    /// Parsed ONCE, at construction. Parsing it per message instead would turn an
    /// unroutable `MAIL_FROM` into a permanent rejection of EVERY row — the whole channel
    /// parked behind a green readiness probe, with no recovery verb — rather than the
    /// boot failure this module's configuration authority promises.
    from: Mailbox,
}

impl SmtpSender {
    /// Construction is PURE — no socket, no DNS (constraint #8). `relay`/`starttls_relay`
    /// only build TLS parameters from the host name; the first packet is sent by
    /// [`Sender::send`].
    pub fn new(
        settings: &SmtpSettings,
        from: &str,
        connect_timeout: Duration,
    ) -> anyhow::Result<SmtpSender> {
        let from = parse_address(from)
            .map_err(|reason| anyhow::anyhow!("the envelope sender {reason}"))?;
        let mut builder = match settings.tls {
            TlsMode::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&settings.host),
            TlsMode::Implicit => AsyncSmtpTransport::<Tokio1Executor>::relay(&settings.host),
        }?;
        builder = builder
            .port(settings.port)
            .timeout(Some(connect_timeout))
            .hello_name(ClientId::Domain(hello_domain(&from)));
        if let Some((username, password)) = &settings.credentials {
            builder = builder.credentials(Credentials::new(
                username.clone(),
                password.expose().to_string(),
            ));
        }
        Ok(SmtpSender {
            transport: builder.build(),
            from: Mailbox::new(None, from),
        })
    }

    /// A PLAINTEXT transport for the loopback-relay tests. `#[cfg(test)]`, so it adds no
    /// production surface and gives [`TlsMode`] no third arm: the deployed sender still
    /// cannot be talked out of TLS. Without it nothing past the upgrade — `MAIL FROM`,
    /// `RCPT TO`, `DATA`, and a `250` acceptance — is reachable by any test, because both
    /// `TlsMode` arms abandon a plaintext peer at EHLO and `webpki-roots` will not validate
    /// a loopback certificate.
    ///
    /// It repeats [`SmtpSender::new`]'s port/timeout/hello chain rather than sharing it,
    /// because the branch under test is what `new` does with `settings.tls` — a helper both
    /// called would test the helper.
    #[cfg(test)]
    pub(crate) fn plaintext_for_tests(
        host: &str,
        port: u16,
        from: &str,
        connect_timeout: Duration,
    ) -> anyhow::Result<SmtpSender> {
        let from = parse_address(from)
            .map_err(|reason| anyhow::anyhow!("the envelope sender {reason}"))?;
        let transport = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
            .port(port)
            .timeout(Some(connect_timeout))
            .hello_name(ClientId::Domain(hello_domain(&from)))
            .build();
        Ok(SmtpSender {
            transport,
            from: Mailbox::new(None, from),
        })
    }
}

#[async_trait]
impl Sender for SmtpSender {
    fn name(&self) -> &'static str {
        SMTP
    }

    async fn send(&self, m: &Outgoing<'_>) -> Result<(), SendError> {
        let message = self.build_message(m).map_err(SendError::Rejected)?;
        self.transport
            .send(message)
            .await
            .map(|_| ())
            .map_err(|e| classify(e.is_permanent(), anyhow::anyhow!(e)))
    }
}

impl SmtpSender {
    /// The only address parsed here is the recipient, through the SAME parser that refused
    /// it at ingress — so this can fail only for a row enqueued before that rule existed.
    /// Such a row will never be accepted by any relay, which is why the caller maps this to
    /// the permanent arm.
    fn build_message(&self, m: &Outgoing<'_>) -> anyhow::Result<Message> {
        let to = parse_address(m.to)
            .map_err(|reason| anyhow::anyhow!("recipient {reason}"))?;
        Ok(Message::builder()
            .from(self.from.clone())
            .to(Mailbox::new(None, to))
            .subject(m.subject)
            .header(ContentType::TEXT_PLAIN)
            .body(m.body.to_string())?)
    }
}
