//! Unit tests for `TlsFront::from_values` — pure values in/out through a map-backed
//! getter, no process-global env mutation.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::TlsFront;

fn parse(vars: &[(&str, &str)]) -> anyhow::Result<Option<TlsFront>> {
    let map: HashMap<String, String> =
        vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    TlsFront::from_values(|k| map.get(k).cloned())
}

#[test]
fn unset_and_blank_and_off_mean_plain_http() {
    assert_eq!(parse(&[]).unwrap(), None, "unset");
    for mode in ["", "  ", "off", "OFF", " off "] {
        let front = parse(&[("TLS_MODE", mode)]).unwrap();
        assert_eq!(front, None, "mode {mode:?}");
    }
}

#[test]
fn unknown_mode_bails_loudly() {
    let err = parse(&[("TLS_MODE", "acme-staging")]).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("acme-staging"), "{msg}");
    assert!(msg.contains("TLS_MODE"), "{msg}");
}

#[test]
fn files_mode_requires_both_paths() {
    let front = parse(&[
        ("TLS_MODE", "files"),
        ("TLS_CERT_PATH", "/etc/tls/cert.pem"),
        ("TLS_KEY_PATH", "/etc/tls/key.pem"),
    ])
    .unwrap();
    assert_eq!(
        front,
        Some(TlsFront::Files {
            cert: PathBuf::from("/etc/tls/cert.pem"),
            key: PathBuf::from("/etc/tls/key.pem"),
        })
    );

    let err = parse(&[("TLS_MODE", "files"), ("TLS_KEY_PATH", "/k.pem")]).unwrap_err();
    assert!(err.to_string().contains("TLS_CERT_PATH"), "{err}");
    let err = parse(&[
        ("TLS_MODE", "files"),
        ("TLS_CERT_PATH", "/c.pem"),
        ("TLS_KEY_PATH", "   "),
    ])
    .unwrap_err();
    assert!(err.to_string().contains("TLS_KEY_PATH"), "{err}");
}

#[test]
fn files_mode_is_case_insensitive() {
    let front = parse(&[
        ("TLS_MODE", "FILES"),
        ("TLS_CERT_PATH", "c.pem"),
        ("TLS_KEY_PATH", "k.pem"),
    ])
    .unwrap();
    assert!(matches!(front, Some(TlsFront::Files { .. })));
}

#[test]
fn acme_mode_parses_domains_contact_and_cache_dir() {
    let front = parse(&[
        ("TLS_MODE", "acme"),
        ("ACME_DOMAINS", " example.com, admin.example.com ,, "),
        ("ACME_CONTACT", "ops@example.com"),
        ("ACME_CACHE_DIR", "/var/lib/acme"),
    ])
    .unwrap();
    assert_eq!(
        front,
        Some(TlsFront::Acme {
            domains: vec!["example.com".to_string(), "admin.example.com".to_string()],
            cache_dir: PathBuf::from("/var/lib/acme"),
            contact: Some("ops@example.com".to_string()),
        })
    );
}

#[test]
fn acme_mode_defaults_cache_dir_and_optional_contact() {
    let front = parse(&[("TLS_MODE", "acme"), ("ACME_DOMAINS", "example.com")]).unwrap();
    assert_eq!(
        front,
        Some(TlsFront::Acme {
            domains: vec!["example.com".to_string()],
            cache_dir: PathBuf::from("run/acme-cache"),
            contact: None,
        })
    );
    let front = parse(&[
        ("TLS_MODE", "acme"),
        ("ACME_DOMAINS", "example.com"),
        ("ACME_CONTACT", "  "),
        ("ACME_CACHE_DIR", " "),
    ])
    .unwrap();
    assert_eq!(
        front,
        Some(TlsFront::Acme {
            domains: vec!["example.com".to_string()],
            cache_dir: PathBuf::from("run/acme-cache"),
            contact: None,
        })
    );
}

#[test]
fn acme_mode_requires_at_least_one_domain() {
    let unset = parse(&[("TLS_MODE", "acme")]).unwrap_err();
    assert!(unset.to_string().contains("ACME_DOMAINS"), "unset: {unset}");
    for domains in ["", "  ,  , "] {
        let err = parse(&[("TLS_MODE", "acme"), ("ACME_DOMAINS", domains)]).unwrap_err();
        assert!(err.to_string().contains("ACME_DOMAINS"), "domains {domains:?}: {err}");
    }
}

#[test]
fn tls_paths_ignore_acme_vars_and_vice_versa() {
    let front = parse(&[
        ("TLS_MODE", "files"),
        ("TLS_CERT_PATH", "c.pem"),
        ("TLS_KEY_PATH", "k.pem"),
        ("ACME_DOMAINS", "example.com"),
    ])
    .unwrap();
    assert!(matches!(front, Some(TlsFront::Files { .. })));
    let front = parse(&[("TLS_CERT_PATH", "c.pem"), ("ACME_DOMAINS", "example.com")]).unwrap();
    assert_eq!(front, None);
}
