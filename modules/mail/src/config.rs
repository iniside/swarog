//! The one validating parse of mail's environment. Every value the module acts on is
//! decided here, at boot, so a misconfiguration is a startup failure rather than a
//! per-message error discovered by the drain loop hours later.
//!
//! The split is the rule: a value with a domain that is PRESENT and unusable kills the
//! process (the `ASYNCEVENTS_HANDLER_TIMEOUT` convention), and only an absent variable
//! takes a compiled default. A set-but-empty value is never a silent default —
//! `MAIL_MAX_ATTEMPTS=${ATTEMPTS}` with the outer variable unset expands to empty, and
//! defaulting it would silently replace an operator's number with this file's.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::address::parse_address;
use crate::providers::{Provider, ProviderKind, KNOWN_PROVIDERS, SMTP};
use crate::smtp::{Secret, SmtpSettings, TlsMode};

pub const PROVIDER_ENV: &str = "MAIL_PROVIDER";
pub const FROM_ENV: &str = "MAIL_FROM";
pub const SEND_TIMEOUT_ENV: &str = "MAIL_SEND_TIMEOUT_MS";
pub const MAX_ATTEMPTS_ENV: &str = "MAIL_MAX_ATTEMPTS";
pub const SMTP_HOST_ENV: &str = "MAIL_SMTP_HOST";
pub const SMTP_PORT_ENV: &str = "MAIL_SMTP_PORT";
pub const SMTP_USERNAME_ENV: &str = "MAIL_SMTP_USERNAME";
pub const SMTP_PASSWORD_ENV: &str = "MAIL_SMTP_PASSWORD";
pub const SMTP_TLS_ENV: &str = "MAIL_SMTP_TLS";

/// Every variable that belongs to the `smtp` provider. Read as a group so a provider that
/// is not `smtp` can refuse them as a group.
const SMTP_VARS: &[&str] = &[
    SMTP_HOST_ENV,
    SMTP_PORT_ENV,
    SMTP_USERNAME_ENV,
    SMTP_PASSWORD_ENV,
    SMTP_TLS_ENV,
];

/// Every variable this parse reads — the authority [`MailConfig::from_env`] collects, so
/// the process-env read stays narrow (a test harness mutating `set_var` has a smaller
/// unsound window) and a new knob is added in one place.
const MAIL_VARS: &[&str] = &[
    PROVIDER_ENV,
    FROM_ENV,
    SEND_TIMEOUT_ENV,
    MAX_ATTEMPTS_ENV,
    SMTP_HOST_ENV,
    SMTP_PORT_ENV,
    SMTP_USERNAME_ENV,
    SMTP_PASSWORD_ENV,
    SMTP_TLS_ENV,
];

pub const DEFAULT_SEND_TIMEOUT_MS: u64 = 10_000;

/// The send budget's ceiling, DERIVED from the drain's backoff cap
/// (the drain's `BACKOFF_MAX_SECS`) rather than picked: an attempt allowed to outlast
/// the longest gap the retry ladder ever waits inverts the ladder. A ceiling is not
/// optional here — the value also sets the pass budget (which becomes a Postgres
/// `statement_timeout`, an int32 count of milliseconds) and the readiness stall threshold,
/// so an absurd value would make every pass fail before it claimed a row while `/readyz`
/// stayed green for weeks.
pub const MAX_SEND_TIMEOUT_MS: u64 = crate::worker::BACKOFF_MAX_SECS * 1_000;
pub const DEFAULT_MAX_ATTEMPTS: i32 = 20;
pub const MAX_MAX_ATTEMPTS: i32 = 1_000;
pub const DEFAULT_SMTP_PORT: u16 = 587;

/// A configured provider and the envelope address it sends as. The two travel together
/// because neither is usable alone: a provider with no `From` cannot address a message,
/// and an address with no provider drains nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSettings {
    pub provider: Provider,
    pub from: String,
}

/// The validated configuration. `provider: None` is the deliberately UNDRAINED channel:
/// requests are still enqueued and checkpointed, nothing sends them, and the module
/// contributes a permanently-failing readiness check so the process says so on `/readyz`
/// instead of relying on a boot warning nobody re-reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailConfig {
    pub provider: Option<ProviderSettings>,
    pub send_timeout: Duration,
    pub max_attempts: i32,
}

impl MailConfig {
    pub fn from_env() -> anyhow::Result<MailConfig> {
        let mut vars = BTreeMap::new();
        for key in MAIL_VARS {
            let Some(raw) = std::env::var_os(key) else {
                continue;
            };
            let value = raw
                .into_string()
                .map_err(|_| anyhow::anyhow!("invalid {key}: value is not valid UTF-8"))?;
            vars.insert(key.to_string(), value);
        }
        MailConfig::from_vars(&vars)
    }

    /// Takes the variables as DATA so every failing branch is provable without mutating
    /// process env (`accounts::providers::ProviderConfig::from_vars`).
    ///
    /// Per-FIELD validation first, cross-field completeness second: a malformed value is
    /// rejected on its own merits, never contingent on which sibling happens to be set.
    pub fn from_vars(vars: &BTreeMap<String, String>) -> anyhow::Result<MailConfig> {
        for key in MAIL_VARS {
            if vars.get(*key).is_some_and(String::is_empty) {
                anyhow::bail!("invalid {key}: set but empty — unset it to leave it unconfigured");
            }
        }

        let send_timeout = Duration::from_millis(parse_send_timeout_ms(vars.get(SEND_TIMEOUT_ENV))?);
        let max_attempts = parse_max_attempts(vars.get(MAX_ATTEMPTS_ENV))?;
        let from = match vars.get(FROM_ENV) {
            Some(raw) => Some(checked_address(FROM_ENV, raw)?),
            None => None,
        };
        let kind = match vars.get(PROVIDER_ENV) {
            Some(raw) => Some(checked_provider(raw)?),
            None => None,
        };
        let resolved = match kind {
            Some(ProviderKind::Log) => Some(Provider::Log),
            Some(ProviderKind::Smtp) => Some(Provider::Smtp(smtp_settings(vars)?)),
            None => None,
        };
        // An operator who set a relay host believes mail is configured; the same refusal
        // FROM_ENV gets below, for the same reason.
        if resolved.as_ref().map(Provider::kind) != Some(ProviderKind::Smtp) {
            if let Some(key) = SMTP_VARS.iter().find(|key| vars.contains_key(**key)) {
                anyhow::bail!(
                    "{key} is set but {PROVIDER_ENV} is not {SMTP} — set \
                     {PROVIDER_ENV}={SMTP}, or unset the {SMTP_HOST_ENV}/{SMTP_PORT_ENV}/\
                     {SMTP_USERNAME_ENV}/{SMTP_PASSWORD_ENV}/{SMTP_TLS_ENV} group"
                );
            }
        }

        let provider = match (resolved, from) {
            (Some(provider), Some(from)) => Some(ProviderSettings { provider, from }),
            (Some(provider), None) => anyhow::bail!(
                "{PROVIDER_ENV}={} but {FROM_ENV} is not set — a provider cannot address a \
                 message without an envelope sender",
                provider.kind().name()
            ),
            // An operator who set an address believes mail is configured; leaving the
            // channel undrained under that belief is the failure this refuses.
            (None, Some(_)) => anyhow::bail!(
                "{FROM_ENV} is set but {PROVIDER_ENV} is not — set {PROVIDER_ENV} to one of \
                 {KNOWN_PROVIDERS:?}, or unset {FROM_ENV} to leave the channel undrained"
            ),
            (None, None) => None,
        };

        Ok(MailConfig {
            provider,
            send_timeout,
            max_attempts,
        })
    }
}

/// The `smtp` arm's own parse. `MAIL_SMTP_HOST` and `MAIL_SMTP_TLS` are REQUIRED: the
/// port has a default because 587 is the submission port every relay agrees on, while a
/// wrong TLS mode is a connection that silently never completes — an operator who picked
/// port 465 and left the mode implicit-by-default would watch every row back off with a
/// timeout. Credentials are both-or-neither: an internal MTA that authorizes by network
/// legitimately has none, but half a credential is a typo, not a configuration.
fn smtp_settings(vars: &BTreeMap<String, String>) -> anyhow::Result<SmtpSettings> {
    let host = match vars.get(SMTP_HOST_ENV) {
        Some(raw) => checked_host(raw.trim())?,
        None => anyhow::bail!("{PROVIDER_ENV}={SMTP} but {SMTP_HOST_ENV} is not set"),
    };
    let port = parse_port(vars.get(SMTP_PORT_ENV))?;
    let tls = match vars.get(SMTP_TLS_ENV) {
        Some(raw) => TlsMode::from_name(raw.trim()).ok_or_else(|| {
            anyhow::anyhow!(
                "invalid {SMTP_TLS_ENV}: unknown mode {raw:?} (known: {:?})",
                TlsMode::NAMES
            )
        })?,
        None => anyhow::bail!(
            "{PROVIDER_ENV}={SMTP} but {SMTP_TLS_ENV} is not set — set it to one of {:?}",
            TlsMode::NAMES
        ),
    };
    let credentials = match (vars.get(SMTP_USERNAME_ENV), vars.get(SMTP_PASSWORD_ENV)) {
        (Some(username), Some(password)) => {
            Some((username.to_string(), Secret::new(password.to_string())))
        }
        (None, None) => None,
        (Some(_), None) => anyhow::bail!(
            "{SMTP_USERNAME_ENV} is set but {SMTP_PASSWORD_ENV} is not — set both, or \
             neither for an unauthenticated relay"
        ),
        (None, Some(_)) => anyhow::bail!(
            "{SMTP_PASSWORD_ENV} is set but {SMTP_USERNAME_ENV} is not — set both, or \
             neither for an unauthenticated relay"
        ),
    };
    Ok(SmtpSettings {
        host,
        port,
        tls,
        credentials,
    })
}

/// The host is a TLS certificate name and an EHLO target, so it carries the same
/// no-control-characters rule the addresses do, plus no embedded whitespace.
fn checked_host(value: &str) -> anyhow::Result<String> {
    if value.is_empty() {
        anyhow::bail!("invalid {SMTP_HOST_ENV}: is required");
    }
    if value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        anyhow::bail!("invalid {SMTP_HOST_ENV}: must not contain whitespace or control characters");
    }
    Ok(value.to_string())
}

fn parse_port(raw: Option<&String>) -> anyhow::Result<u16> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_SMTP_PORT);
    };
    let port: u16 = raw.trim().parse().map_err(|_| {
        anyhow::anyhow!(
            "invalid {SMTP_PORT_ENV}: must be a TCP port between 1 and 65535 (got {raw:?}); \
             unset it for the default {DEFAULT_SMTP_PORT}"
        )
    })?;
    if port == 0 {
        anyhow::bail!(
            "invalid {SMTP_PORT_ENV}: 0 is not a TCP port; unset it for the default \
             {DEFAULT_SMTP_PORT}"
        );
    }
    Ok(port)
}

fn checked_provider(raw: &str) -> anyhow::Result<ProviderKind> {
    ProviderKind::from_name(raw.trim()).ok_or_else(|| {
        anyhow::anyhow!("invalid {PROVIDER_ENV}: unknown provider {raw:?} (known: {KNOWN_PROVIDERS:?})")
    })
}

/// The envelope sender, held to the SAME shape rule as the recipient it will be sent
/// alongside ([`crate::address::parse_address`]) — one policy, two callers. The parsed
/// value is dropped here and re-derived by the transport from this same string, which is
/// safe only because both go through that one parser.
fn checked_address(key: &str, raw: &str) -> anyhow::Result<String> {
    let value = raw.trim();
    parse_address(value).map_err(|reason| anyhow::anyhow!("invalid {key}: {reason}"))?;
    Ok(value.to_string())
}

/// Range-checked at BOTH ends. `0` leaves no time for any send, which would burn every
/// attempt of every row before parking it; past [`MAX_SEND_TIMEOUT_MS`] the value stops
/// being a send budget and becomes a way to make the whole drain inert — it is also the
/// pass budget and the readiness stall threshold, so an unchecked ceiling buys a channel
/// that claims nothing while `/readyz` reports healthy.
fn parse_send_timeout_ms(raw: Option<&String>) -> anyhow::Result<u64> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_SEND_TIMEOUT_MS);
    };
    let ms: u64 = raw.trim().parse().map_err(|_| {
        anyhow::anyhow!(
            "invalid {SEND_TIMEOUT_ENV}: must be a whole number of milliseconds (got {raw:?}); \
             unset it for the default {DEFAULT_SEND_TIMEOUT_MS}"
        )
    })?;
    if !(1..=MAX_SEND_TIMEOUT_MS).contains(&ms) {
        anyhow::bail!(
            "invalid {SEND_TIMEOUT_ENV}: must be between 1 and {MAX_SEND_TIMEOUT_MS} \
             milliseconds (got {ms}); unset it for the default {DEFAULT_SEND_TIMEOUT_MS}"
        );
    }
    Ok(ms)
}

fn parse_max_attempts(raw: Option<&String>) -> anyhow::Result<i32> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_MAX_ATTEMPTS);
    };
    let attempts: i32 = raw.trim().parse().map_err(|_| {
        anyhow::anyhow!(
            "invalid {MAX_ATTEMPTS_ENV}: must be a whole number (got {raw:?}); unset it for \
             the default {DEFAULT_MAX_ATTEMPTS}"
        )
    })?;
    if !(1..=MAX_MAX_ATTEMPTS).contains(&attempts) {
        anyhow::bail!(
            "invalid {MAX_ATTEMPTS_ENV}: must be between 1 and {MAX_MAX_ATTEMPTS} (got \
             {attempts}); unset it for the default {DEFAULT_MAX_ATTEMPTS}"
        );
    }
    Ok(attempts)
}
