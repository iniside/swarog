//! The `smtp` provider: a real relay behind the same [`Sender`] face the dev sink
//! implements.
//!
//! Two bounds, not one. An SMTP delivery is a multi-round-trip dialogue (banner, EHLO,
//! STARTTLS, AUTH, MAIL/RCPT/DATA, the final status), so a single whole-send deadline
//! cannot tell a relay that stalled mid-dialogue from one that is simply slow: the
//! transport carries a PER-I/O-STEP timeout here, and the drain wraps the whole attempt
//! in the same budget as the aggregate backstop (`modules/gateway/src/proxy.rs` records
//! the same reasoning for the passthrough client). Both are derived from
//! `MAIL_SEND_TIMEOUT_MS` — there is no second knob.

use std::time::Duration;

use async_trait::async_trait;
use lettre::message::header::ContentType;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::extension::ClientId;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

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
/// reject.
fn hello_domain(from: &str) -> String {
    match from.rsplit_once('@') {
        Some((_, domain)) if !domain.is_empty() => domain.to_string(),
        _ => from.to_string(),
    }
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
}

impl SmtpSender {
    /// Construction is PURE — no socket, no DNS (constraint #8). `relay`/`starttls_relay`
    /// only build TLS parameters from the host name; the first packet is sent by
    /// [`Sender::send`].
    pub fn new(
        settings: &SmtpSettings,
        from: &str,
        per_step_timeout: Duration,
    ) -> anyhow::Result<SmtpSender> {
        let mut builder = match settings.tls {
            TlsMode::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&settings.host),
            TlsMode::Implicit => AsyncSmtpTransport::<Tokio1Executor>::relay(&settings.host),
        }?;
        builder = builder
            .port(settings.port)
            .timeout(Some(per_step_timeout))
            .hello_name(ClientId::Domain(hello_domain(from)));
        if let Some((username, password)) = &settings.credentials {
            builder = builder.credentials(Credentials::new(
                username.clone(),
                password.expose().to_string(),
            ));
        }
        Ok(SmtpSender {
            transport: builder.build(),
        })
    }
}

#[async_trait]
impl Sender for SmtpSender {
    fn name(&self) -> &'static str {
        SMTP
    }

    async fn send(&self, m: &Outgoing<'_>) -> Result<(), SendError> {
        let message = build_message(m).map_err(SendError::Rejected)?;
        self.transport
            .send(message)
            .await
            .map(|_| ())
            .map_err(|e| classify(e.is_permanent(), anyhow::anyhow!(e)))
    }
}

/// An address or header the message builder refuses will never be accepted by any relay,
/// so the caller maps this to the permanent arm.
fn build_message(m: &Outgoing<'_>) -> anyhow::Result<Message> {
    let from: Mailbox = m
        .from
        .parse()
        .map_err(|e| anyhow::anyhow!("envelope sender is not a mailbox: {e}"))?;
    let to: Mailbox = m
        .to
        .parse()
        .map_err(|e| anyhow::anyhow!("recipient is not a mailbox: {e}"))?;
    Ok(Message::builder()
        .from(from)
        .to(to)
        .subject(m.subject)
        .header(ContentType::TEXT_PLAIN)
        .body(m.body.to_string())?)
}
