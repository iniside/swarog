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

use crate::address::check_address;
use crate::providers::{ProviderKind, KNOWN_PROVIDERS};

pub const PROVIDER_ENV: &str = "MAIL_PROVIDER";
pub const FROM_ENV: &str = "MAIL_FROM";
pub const SEND_TIMEOUT_ENV: &str = "MAIL_SEND_TIMEOUT_MS";
pub const MAX_ATTEMPTS_ENV: &str = "MAIL_MAX_ATTEMPTS";

/// Every variable this parse reads — the authority [`MailConfig::from_env`] collects, so
/// the process-env read stays narrow (a test harness mutating `set_var` has a smaller
/// unsound window) and a new knob is added in one place.
const MAIL_VARS: &[&str] = &[PROVIDER_ENV, FROM_ENV, SEND_TIMEOUT_ENV, MAX_ATTEMPTS_ENV];

pub const DEFAULT_SEND_TIMEOUT_MS: u64 = 10_000;
pub const DEFAULT_MAX_ATTEMPTS: i32 = 20;
pub const MAX_MAX_ATTEMPTS: i32 = 1_000;

/// A configured provider and the envelope address it sends as. The two travel together
/// because neither is usable alone: a provider with no `From` cannot address a message,
/// and an address with no provider drains nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSettings {
    pub kind: ProviderKind,
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

        let provider = match (kind, from) {
            (Some(kind), Some(from)) => Some(ProviderSettings { kind, from }),
            (Some(kind), None) => anyhow::bail!(
                "{PROVIDER_ENV}={} but {FROM_ENV} is not set — a provider cannot address a \
                 message without an envelope sender",
                kind.name()
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

fn checked_provider(raw: &str) -> anyhow::Result<ProviderKind> {
    ProviderKind::from_name(raw.trim()).ok_or_else(|| {
        anyhow::anyhow!("invalid {PROVIDER_ENV}: unknown provider {raw:?} (known: {KNOWN_PROVIDERS:?})")
    })
}

/// The envelope sender, held to the SAME shape rule as the recipient it will be sent
/// alongside ([`crate::address::check_address`]) — one policy, two callers.
fn checked_address(key: &str, raw: &str) -> anyhow::Result<String> {
    let value = raw.trim();
    check_address(value).map_err(|reason| anyhow::anyhow!("invalid {key}: {reason}"))?;
    Ok(value.to_string())
}

/// `0` is rejected with everything else out of range: a zero deadline is a send that can
/// never succeed, which would burn every attempt of every row before parking it.
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
    if ms == 0 {
        anyhow::bail!(
            "invalid {SEND_TIMEOUT_ENV}: 0 leaves no time for any send; unset it for the \
             default {DEFAULT_SEND_TIMEOUT_MS}"
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
