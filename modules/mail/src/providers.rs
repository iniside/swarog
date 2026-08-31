//! The sending seam: the provider naming authority, the `Sender` face every provider
//! implements, and the error taxonomy that decides whether a failed send is permanent.
//!
//! The names live HERE, not in `mailevents`: no field of `SendRequested` carries a
//! provider name — the provider is a deployment's own `MAIL_PROVIDER` choice — so putting
//! them in the contract crate would freeze an internal configuration value into a durable
//! contract's public-api baseline.

use std::sync::Arc;

use async_trait::async_trait;

/// The development sink: it renders the send decision into the log and delivers nothing.
pub const LOG: &str = "log";

/// The real relay. It is NOT in [`KNOWN_PROVIDERS`] yet — the name joins the list in the
/// same commit that ships its sender, so a boot-time failure never names a provider this
/// build cannot construct (`accounts` paid for learning that with a permanent,
/// operator-unfixable 503 on `apple`).
pub const SMTP: &str = "smtp";

/// Every provider name this build can construct a sender for. The list is what an unknown
/// `MAIL_PROVIDER` is reported against, and [`ProviderKind::from_name`] is what actually
/// resolves one; a name in one and not the other is the drift the module tests pin.
pub const KNOWN_PROVIDERS: &[&str] = &[LOG];

/// One message as a sender sees it, borrowed from the outbox row being drained.
pub struct Outgoing<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub subject: &'a str,
    pub body: &'a str,
    pub kind: &'a str,
}

/// Why a send failed — the taxonomy the drain worker maps to a row state, mirroring
/// `accounts::providers::VerifyError`. Getting this wrong in either direction is
/// expensive: a retried `Rejected` hammers a relay that will never accept the message,
/// and a parked `Infra` drops a deliverable message on a transient socket error.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    /// The message itself will never be accepted (a refused recipient, a permanent 5xx).
    /// Permanent — the row parks for an operator.
    #[error("message rejected: {0}")]
    Rejected(#[source] anyhow::Error),
    /// No verdict was reachable: the relay was unreachable, timed out, or answered a
    /// transient 4xx. The row stays pending and backs off.
    #[error("mail provider unavailable: {0}")]
    Infra(#[source] anyhow::Error),
}

/// One provider's sending face.
#[async_trait]
pub trait Sender: Send + Sync {
    /// The name this sender is recorded under on a delivered row.
    fn name(&self) -> &'static str;

    async fn send(&self, m: &Outgoing<'_>) -> Result<(), SendError>;
}

/// The resolved provider. An enum rather than a registry map: mail configures exactly ONE
/// provider per process, and the type makes a duplicate registration unrepresentable
/// instead of a panic to remember.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    Log,
}

impl ProviderKind {
    /// The name → kind authority. `None` is an unknown name, which [`crate::config`] turns
    /// into a startup failure naming [`KNOWN_PROVIDERS`].
    pub fn from_name(name: &str) -> Option<ProviderKind> {
        match name {
            LOG => Some(ProviderKind::Log),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ProviderKind::Log => LOG,
        }
    }

    /// Construction is PURE — no socket, no DNS, no I/O (constraint #8): a provider that
    /// cannot be built from validated configuration alone is a configuration error the
    /// parse must have already refused.
    pub fn sender(self) -> Arc<dyn Sender> {
        match self {
            ProviderKind::Log => Arc::new(LogSender),
        }
    }
}

/// The dev sink. It logs the ENVELOPE and the body's size, never the body: a rendered body
/// is a verification link or a reset token, and a log line is the one place a delivered
/// secret would outlive the row that carries it.
pub struct LogSender;

#[async_trait]
impl Sender for LogSender {
    fn name(&self) -> &'static str {
        LOG
    }

    async fn send(&self, m: &Outgoing<'_>) -> Result<(), SendError> {
        tracing::info!(
            from = m.from,
            to = m.to,
            subject = m.subject,
            kind = m.kind,
            body_bytes = m.body.len(),
            "mail: log provider accepted a message (nothing was delivered)"
        );
        Ok(())
    }
}
