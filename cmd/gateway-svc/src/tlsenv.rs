//! TLS-front env parsing for the public front door (admin hardening Step 4). The env
//! is read HERE, in the composition root — the MECHANISM lives in `core/app`
//! ([`app::TlsFront`] + `Config::with_tls`), and modules see nothing. No other
//! `cmd/*` main parses these vars today: the gateway is the single public front door,
//! so it is the only process that terminates public TLS.
//!
//! Contract (fail loudly on partial config, defaults stay fail-closed-plain):
//! - `TLS_MODE` — `off` (default when unset/blank) | `files` | `acme`; anything else
//!   is a startup error, never silently "off".
//! - `files`: `TLS_CERT_PATH` + `TLS_KEY_PATH` BOTH required (PEM paths).
//! - `acme`: `ACME_DOMAINS` required (comma-separated, at least one non-blank),
//!   `ACME_CONTACT` optional (email), `ACME_CACHE_DIR` optional
//!   (default `run/acme-cache`).

use std::path::PathBuf;

/// Default ACME account/cert cache directory, next to the repo's other `run/` state.
const DEFAULT_ACME_CACHE_DIR: &str = "run/acme-cache";

/// Reads the `TLS_*`/`ACME_*` env into an [`app::TlsFront`] choice for
/// `Config::with_tls`. `Ok(None)` = plain HTTP (mode `off`).
pub fn tls_front_from_env() -> anyhow::Result<Option<app::TlsFront>> {
    parse_tls_front(
        std::env::var("TLS_MODE").ok(),
        std::env::var("TLS_CERT_PATH").ok(),
        std::env::var("TLS_KEY_PATH").ok(),
        std::env::var("ACME_DOMAINS").ok(),
        std::env::var("ACME_CONTACT").ok(),
        std::env::var("ACME_CACHE_DIR").ok(),
    )
}

/// The pure core of [`tls_front_from_env`] — env values in, front out. Split out so
/// the mode/requirement logic is unit-testable without mutating process-global env
/// (the same shape as `app::Config::from_values`).
pub fn parse_tls_front(
    mode: Option<String>,
    cert_path: Option<String>,
    key_path: Option<String>,
    acme_domains: Option<String>,
    acme_contact: Option<String>,
    acme_cache_dir: Option<String>,
) -> anyhow::Result<Option<app::TlsFront>> {
    let mode = non_blank(mode).unwrap_or_else(|| "off".to_string());
    match mode.to_ascii_lowercase().as_str() {
        "off" => Ok(None),
        "files" => {
            let cert = non_blank(cert_path)
                .ok_or_else(|| anyhow::anyhow!("TLS_MODE=files requires TLS_CERT_PATH"))?;
            let key = non_blank(key_path)
                .ok_or_else(|| anyhow::anyhow!("TLS_MODE=files requires TLS_KEY_PATH"))?;
            Ok(Some(app::TlsFront::Files {
                cert: PathBuf::from(cert),
                key: PathBuf::from(key),
            }))
        }
        "acme" => {
            let domains: Vec<String> = acme_domains
                .as_deref()
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .map(str::to_string)
                .collect();
            if domains.is_empty() {
                anyhow::bail!(
                    "TLS_MODE=acme requires ACME_DOMAINS (comma-separated, at least one domain)"
                );
            }
            let cache_dir =
                non_blank(acme_cache_dir).unwrap_or_else(|| DEFAULT_ACME_CACHE_DIR.to_string());
            Ok(Some(app::TlsFront::Acme {
                domains,
                cache_dir: PathBuf::from(cache_dir),
                contact: non_blank(acme_contact),
            }))
        }
        other => anyhow::bail!(
            "unknown TLS_MODE {other:?} — expected \"off\", \"files\" or \"acme\" \
             (refusing to guess; unset means off)"
        ),
    }
}

/// Trims and drops empty values — unset and blank env read the same.
fn non_blank(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
