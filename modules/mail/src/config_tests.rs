//! The env table, driven through `MailConfig::from_vars` — vars as DATA, so every failing
//! branch is provable without mutating process env. The `smtp` arm needs two variables at
//! once and is unreachable from the conformance harness's one-variable fixtures.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::config::*;
use crate::providers::{Provider, ProviderKind, LOG, SMTP};
use crate::smtp::TlsMode;

fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn ok(pairs: &[(&str, &str)]) -> MailConfig {
    MailConfig::from_vars(&vars(pairs))
        .unwrap_or_else(|e| panic!("{pairs:?} must be accepted, got {e:#}"))
}

/// The rejection AND its message: an operator who cannot see which variable is wrong reads
/// a startup failure as "mail is broken".
fn refused(pairs: &[(&str, &str)], needle: &str) -> String {
    let e = MailConfig::from_vars(&vars(pairs))
        .map(|c| format!("{c:?}"))
        .expect_err(&format!("{pairs:?} must FAIL startup"));
    let msg = format!("{e:#}");
    assert!(msg.contains(needle), "{pairs:?}: {msg:?} must name {needle:?}");
    msg
}

/// A LOG-provider configuration every other case varies one field of.
fn log_provider() -> Vec<(&'static str, &'static str)> {
    vec![(PROVIDER_ENV, LOG), (FROM_ENV, "noreply@example.com")]
}

fn smtp_provider() -> Vec<(&'static str, &'static str)> {
    vec![
        (PROVIDER_ENV, SMTP),
        (FROM_ENV, "noreply@example.com"),
        (SMTP_HOST_ENV, "relay.example.com"),
        (SMTP_TLS_ENV, TlsMode::STARTTLS),
    ]
}

fn with(base: &[(&'static str, &'static str)], extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
    let mut all = base.to_vec();
    all.extend_from_slice(extra);
    all
}

// ============================================================================
// The unconfigured channel and the two half-configurations.
// ============================================================================

/// An empty environment is the deliberately UNDRAINED channel, not a failure: requests are
/// still enqueued and checkpointed, and `/readyz` is what says nothing sends them.
#[test]
fn an_empty_environment_yields_an_undrained_channel_on_the_compiled_defaults() {
    let cfg = ok(&[]);
    assert!(cfg.provider.is_none());
    assert_eq!(cfg.send_timeout, Duration::from_millis(DEFAULT_SEND_TIMEOUT_MS));
    assert_eq!(cfg.max_attempts, DEFAULT_MAX_ATTEMPTS);
    assert_eq!(cfg.retention_days, DEFAULT_RETENTION_DAYS);
}

#[test]
fn an_address_without_a_provider_fails_startup() {
    refused(&[(FROM_ENV, "noreply@example.com")], PROVIDER_ENV);
}

#[test]
fn a_provider_without_an_address_fails_startup() {
    refused(&[(PROVIDER_ENV, LOG)], FROM_ENV);
}

// ============================================================================
// The `set but empty` bail — the branch the conformance fixture's `"   "` spelling
// deliberately routes around.
// ============================================================================

/// `MAIL_MAX_ATTEMPTS=${ATTEMPTS}` with the outer variable unset expands to EMPTY, and
/// defaulting it would silently replace an operator's number with this file's. Every
/// variable takes the same rule, so the test walks the whole table rather than a sample.
#[test]
fn every_variable_set_but_empty_is_refused_rather_than_defaulted() {
    for key in [
        PROVIDER_ENV,
        FROM_ENV,
        SEND_TIMEOUT_ENV,
        MAX_ATTEMPTS_ENV,
        SMTP_HOST_ENV,
        SMTP_PORT_ENV,
        SMTP_USERNAME_ENV,
        SMTP_PASSWORD_ENV,
        SMTP_TLS_ENV,
        RETENTION_ENV,
    ] {
        let msg = refused(&with(&smtp_provider(), &[(key, "")]), key);
        assert!(msg.contains("set but empty"), "{key}: {msg:?}");
    }
}

// ============================================================================
// The numeric knobs, at both ends of every range.
// ============================================================================

#[test]
fn the_send_timeout_is_range_checked_at_both_ends() {
    assert_eq!(ok(&[(SEND_TIMEOUT_ENV, "1")]).send_timeout, Duration::from_millis(1));
    assert_eq!(
        ok(&[(SEND_TIMEOUT_ENV, &MAX_SEND_TIMEOUT_MS.to_string())]).send_timeout,
        Duration::from_millis(MAX_SEND_TIMEOUT_MS)
    );
    // `0` leaves no time for any send, which would burn every attempt of every row before
    // parking it.
    refused(&[(SEND_TIMEOUT_ENV, "0")], SEND_TIMEOUT_ENV);
    // Past the ceiling the value stops being a send budget and makes the whole drain inert
    // behind a green `/readyz`.
    refused(
        &[(SEND_TIMEOUT_ENV, &(MAX_SEND_TIMEOUT_MS + 1).to_string())],
        SEND_TIMEOUT_ENV,
    );
    refused(&[(SEND_TIMEOUT_ENV, "10s")], SEND_TIMEOUT_ENV);
    refused(&[(SEND_TIMEOUT_ENV, "-1")], SEND_TIMEOUT_ENV);
}

/// The ceiling is DERIVED from the retry ladder's longest gap, not picked: an attempt
/// allowed to outlast it inverts the ladder and stops `MAIL_MAX_ATTEMPTS` bounding
/// anything.
#[test]
fn the_send_timeout_ceiling_is_the_backoff_cap() {
    assert_eq!(MAX_SEND_TIMEOUT_MS, crate::worker::BACKOFF_MAX_SECS * 1_000);
}

#[test]
fn max_attempts_is_range_checked_at_both_ends() {
    assert_eq!(ok(&[(MAX_ATTEMPTS_ENV, "1")]).max_attempts, 1);
    assert_eq!(
        ok(&[(MAX_ATTEMPTS_ENV, &MAX_MAX_ATTEMPTS.to_string())]).max_attempts,
        MAX_MAX_ATTEMPTS
    );
    refused(&[(MAX_ATTEMPTS_ENV, "0")], MAX_ATTEMPTS_ENV);
    refused(&[(MAX_ATTEMPTS_ENV, &(MAX_MAX_ATTEMPTS + 1).to_string())], MAX_ATTEMPTS_ENV);
    refused(&[(MAX_ATTEMPTS_ENV, "many")], MAX_ATTEMPTS_ENV);
}

/// Out of range at BOOT, where it stops the process — unchecked, a value `make_interval`
/// cannot subtract raises `22008` on every prune fire instead, which pauses the
/// subscription rather than failing the boot.
#[test]
fn the_retention_window_is_range_checked_at_both_ends() {
    assert_eq!(ok(&[(RETENTION_ENV, "1")]).retention_days, 1);
    assert_eq!(
        ok(&[(RETENTION_ENV, &MAX_RETENTION_DAYS.to_string())]).retention_days,
        MAX_RETENTION_DAYS
    );
    refused(&[(RETENTION_ENV, "0")], RETENTION_ENV);
    refused(&[(RETENTION_ENV, &(MAX_RETENTION_DAYS + 1).to_string())], RETENTION_ENV);
    refused(&[(RETENTION_ENV, "forever")], RETENTION_ENV);
}

// ============================================================================
// The provider name and the envelope sender.
// ============================================================================

#[test]
fn an_unknown_provider_names_the_list_it_was_checked_against() {
    let msg = refused(
        &[(PROVIDER_ENV, "sendgrid"), (FROM_ENV, "noreply@example.com")],
        PROVIDER_ENV,
    );
    assert!(msg.contains("sendgrid"), "{msg:?}");
    assert!(msg.contains(LOG) && msg.contains(SMTP), "the known list must be shown: {msg:?}");
}

#[test]
fn the_log_provider_resolves_with_its_envelope_sender() {
    let settings = ok(&log_provider()).provider.expect("a configured provider");
    assert_eq!(settings.provider, Provider::Log);
    assert_eq!(settings.provider.kind(), ProviderKind::Log);
    assert_eq!(settings.from, "noreply@example.com");
    // Surrounding whitespace is trimmed, never carried into an SMTP envelope.
    assert_eq!(
        ok(&[(PROVIDER_ENV, " log "), (FROM_ENV, " noreply@example.com ")])
            .provider
            .unwrap()
            .from,
        "noreply@example.com"
    );
}

/// A `From` no relay accepts parks EVERY row behind a green readiness probe, so it must
/// fail the boot — under the SAME parser the recipient goes through.
#[test]
fn an_unroutable_envelope_sender_fails_startup() {
    for value in ["noreply@", "@example.com", "not-an-address", "a\r\nb@example.com"] {
        refused(&[(PROVIDER_ENV, LOG), (FROM_ENV, value)], FROM_ENV);
    }
}

// ============================================================================
// The whole `smtp` arm — ~10 startup branches, each needing two variables at once.
// ============================================================================

#[test]
fn a_complete_smtp_configuration_resolves_with_the_default_port_and_no_credentials() {
    let settings = ok(&smtp_provider()).provider.expect("a configured provider");
    assert_eq!(settings.provider.kind(), ProviderKind::Smtp);
    let Provider::Smtp(smtp) = settings.provider else {
        panic!("MAIL_PROVIDER=smtp must resolve to the smtp arm")
    };
    assert_eq!(smtp.host, "relay.example.com");
    assert_eq!(smtp.port, DEFAULT_SMTP_PORT);
    assert_eq!(smtp.tls, TlsMode::StartTls);
    assert!(smtp.credentials.is_none(), "an internal MTA legitimately has none");
}

#[test]
fn the_smtp_host_is_required_and_shape_checked() {
    let mut without_host = smtp_provider();
    without_host.retain(|(k, _)| *k != SMTP_HOST_ENV);
    refused(&without_host, SMTP_HOST_ENV);
    // A host is a TLS certificate name and an EHLO target.
    for bad in ["relay example.com", "relay\r\n.example.com", "   "] {
        refused(&with(&smtp_provider(), &[(SMTP_HOST_ENV, bad)]), SMTP_HOST_ENV);
    }
}

/// A wrong TLS mode is a connection that silently never completes, so it has no default —
/// unlike the port, where 587 is the submission port every relay agrees on.
#[test]
fn the_tls_mode_is_required_and_named_against_its_list() {
    let mut without_tls = smtp_provider();
    without_tls.retain(|(k, _)| *k != SMTP_TLS_ENV);
    refused(&without_tls, SMTP_TLS_ENV);
    let msg = refused(&with(&smtp_provider(), &[(SMTP_TLS_ENV, "ssl")]), SMTP_TLS_ENV);
    assert!(msg.contains(TlsMode::STARTTLS) && msg.contains(TlsMode::IMPLICIT), "{msg:?}");
    for (name, mode) in [
        (TlsMode::STARTTLS, TlsMode::StartTls),
        (TlsMode::IMPLICIT, TlsMode::Implicit),
    ] {
        let cfg = ok(&with(&smtp_provider(), &[(SMTP_TLS_ENV, name)]));
        let Some(crate::config::ProviderSettings { provider: Provider::Smtp(smtp), .. }) =
            cfg.provider
        else {
            panic!("{name} must resolve")
        };
        assert_eq!(smtp.tls, mode);
    }
    assert!(TlsMode::from_name("plaintext").is_none(), "there is no plaintext arm");
}

#[test]
fn the_smtp_port_defaults_and_is_range_checked() {
    let cfg = ok(&with(&smtp_provider(), &[(SMTP_PORT_ENV, "465")]));
    let Some(crate::config::ProviderSettings { provider: Provider::Smtp(smtp), .. }) = cfg.provider
    else {
        panic!("a port must resolve")
    };
    assert_eq!(smtp.port, 465);
    refused(&with(&smtp_provider(), &[(SMTP_PORT_ENV, "0")]), SMTP_PORT_ENV);
    refused(&with(&smtp_provider(), &[(SMTP_PORT_ENV, "65536")]), SMTP_PORT_ENV);
    refused(&with(&smtp_provider(), &[(SMTP_PORT_ENV, "smtp")]), SMTP_PORT_ENV);
}

/// Both-or-neither: an internal MTA that authorizes by network has no credential, but half
/// a credential is a typo, not a configuration.
#[test]
fn smtp_credentials_are_both_or_neither() {
    let cfg = ok(&with(
        &smtp_provider(),
        &[(SMTP_USERNAME_ENV, "relay-user"), (SMTP_PASSWORD_ENV, "s3cret")],
    ));
    let Some(crate::config::ProviderSettings { provider: Provider::Smtp(smtp), .. }) = cfg.provider
    else {
        panic!("a credentialed relay must resolve")
    };
    let (user, secret) = smtp.credentials.expect("both halves were set");
    assert_eq!(user, "relay-user");
    // `MailConfig` derives Debug, so an operator password must never reach a `{:?}` of the
    // whole configuration — a log line, a panic message, an admin cell.
    let printed = format!("{secret:?}");
    assert!(!printed.contains("s3cret"), "the secret leaked into Debug: {printed}");
    assert!(printed.contains("redacted"), "{printed}");

    refused(
        &with(&smtp_provider(), &[(SMTP_USERNAME_ENV, "relay-user")]),
        SMTP_PASSWORD_ENV,
    );
    refused(
        &with(&smtp_provider(), &[(SMTP_PASSWORD_ENV, "s3cret")]),
        SMTP_USERNAME_ENV,
    );
}

/// An operator who set a relay host believes mail is configured; leaving those variables
/// inert under that belief is the failure this refuses. Every member of the group refuses
/// on its own, so a single stray one cannot slip through.
#[test]
fn any_smtp_variable_set_under_another_provider_fails_startup() {
    for (key, value) in [
        (SMTP_HOST_ENV, "relay.example.com"),
        (SMTP_PORT_ENV, "587"),
        (SMTP_USERNAME_ENV, "relay-user"),
        (SMTP_PASSWORD_ENV, "s3cret"),
        (SMTP_TLS_ENV, TlsMode::STARTTLS),
    ] {
        let msg = refused(&with(&log_provider(), &[(key, value)]), key);
        assert!(msg.contains(PROVIDER_ENV), "the remedy must name the provider: {msg:?}");
        // The same variable with NO provider at all: reported against MAIL_FROM's own
        // half-configuration rule, never silently accepted.
        MailConfig::from_vars(&vars(&[(key, value)]))
            .expect_err("an smtp variable with no provider must fail startup");
    }
}

#[test]
fn a_debug_of_the_whole_configuration_never_prints_a_credential() {
    let cfg = ok(&with(
        &smtp_provider(),
        &[(SMTP_USERNAME_ENV, "relay-user"), (SMTP_PASSWORD_ENV, "s3cret")],
    ));
    let printed = format!("{cfg:?}");
    assert!(!printed.contains("s3cret"), "{printed}");
}
