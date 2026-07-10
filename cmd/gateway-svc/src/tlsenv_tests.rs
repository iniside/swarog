//! Unit tests for the TLS-front env parsing (`tlsenv.rs`) — pure values in/out, no
//! process-global env mutation.

use std::path::PathBuf;

use crate::tlsenv::parse_tls_front;

fn s(v: &str) -> Option<String> {
    Some(v.to_string())
}

#[test]
fn unset_and_blank_and_off_mean_plain_http() {
    for mode in [None, s(""), s("  "), s("off"), s("OFF"), s(" off ")] {
        let front = parse_tls_front(mode.clone(), None, None, None, None, None).unwrap();
        assert_eq!(front, None, "mode {mode:?}");
    }
}

#[test]
fn unknown_mode_bails_loudly() {
    let err = parse_tls_front(s("acme-staging"), None, None, None, None, None).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("acme-staging"), "{msg}");
    assert!(msg.contains("TLS_MODE"), "{msg}");
}

#[test]
fn files_mode_requires_both_paths() {
    // Both present → Files front with both paths.
    let front = parse_tls_front(s("files"), s("/etc/tls/cert.pem"), s("/etc/tls/key.pem"), None, None, None)
        .unwrap();
    assert_eq!(
        front,
        Some(app::TlsFront::Files {
            cert: PathBuf::from("/etc/tls/cert.pem"),
            key: PathBuf::from("/etc/tls/key.pem"),
        })
    );

    // Missing/blank cert or key → loud bail naming the missing var.
    let err = parse_tls_front(s("files"), None, s("/k.pem"), None, None, None).unwrap_err();
    assert!(err.to_string().contains("TLS_CERT_PATH"), "{err}");
    let err = parse_tls_front(s("files"), s("/c.pem"), s("   "), None, None, None).unwrap_err();
    assert!(err.to_string().contains("TLS_KEY_PATH"), "{err}");
}

#[test]
fn files_mode_is_case_insensitive() {
    let front =
        parse_tls_front(s("FILES"), s("c.pem"), s("k.pem"), None, None, None).unwrap();
    assert!(matches!(front, Some(app::TlsFront::Files { .. })));
}

#[test]
fn acme_mode_parses_domains_contact_and_cache_dir() {
    let front = parse_tls_front(
        s("acme"),
        None,
        None,
        s(" example.com, admin.example.com ,, "),
        s("ops@example.com"),
        s("/var/lib/acme"),
    )
    .unwrap();
    assert_eq!(
        front,
        Some(app::TlsFront::Acme {
            domains: vec!["example.com".to_string(), "admin.example.com".to_string()],
            cache_dir: PathBuf::from("/var/lib/acme"),
            contact: Some("ops@example.com".to_string()),
        })
    );
}

#[test]
fn acme_mode_defaults_cache_dir_and_optional_contact() {
    let front = parse_tls_front(s("acme"), None, None, s("example.com"), None, None).unwrap();
    assert_eq!(
        front,
        Some(app::TlsFront::Acme {
            domains: vec!["example.com".to_string()],
            cache_dir: PathBuf::from("run/acme-cache"),
            contact: None,
        })
    );
    // Blank contact reads as absent, same as unset.
    let front =
        parse_tls_front(s("acme"), None, None, s("example.com"), s("  "), s(" ")).unwrap();
    assert_eq!(
        front,
        Some(app::TlsFront::Acme {
            domains: vec!["example.com".to_string()],
            cache_dir: PathBuf::from("run/acme-cache"),
            contact: None,
        })
    );
}

#[test]
fn acme_mode_requires_at_least_one_domain() {
    for domains in [None, s(""), s("  ,  , ")] {
        let err = parse_tls_front(s("acme"), None, None, domains.clone(), None, None).unwrap_err();
        assert!(err.to_string().contains("ACME_DOMAINS"), "domains {domains:?}: {err}");
    }
}

#[test]
fn tls_paths_ignore_acme_vars_and_vice_versa() {
    // files mode ignores ACME vars entirely.
    let front = parse_tls_front(s("files"), s("c.pem"), s("k.pem"), s("example.com"), None, None)
        .unwrap();
    assert!(matches!(front, Some(app::TlsFront::Files { .. })));
    // off ignores everything — no partial-config error when the front is off.
    let front = parse_tls_front(None, s("c.pem"), None, s("example.com"), None, None).unwrap();
    assert_eq!(front, None);
}
