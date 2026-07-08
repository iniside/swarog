use super::*;
use lifecycle::Context;

/// A minimal module for the topology tests: a name + a requires manifest. A
/// "remote stub" is indistinguishable here — it too just reports a name.
struct Fake {
    name: String,
    requires: Vec<String>,
}

impl Fake {
    fn boxed(name: &str, requires: &[&str]) -> Box<dyn Module> {
        Box::new(Fake {
            name: name.to_string(),
            requires: requires.iter().map(|s| s.to_string()).collect(),
        })
    }
}

#[async_trait::async_trait]
impl Module for Fake {
    fn name(&self) -> &str {
        &self.name
    }
    fn requires(&self) -> Vec<String> {
        self.requires.clone()
    }
    fn init(&self, _ctx: &Context) -> anyhow::Result<()> {
        Ok(())
    }
}

#[test]
fn validate_requires_passes_when_provider_present() {
    let mods = vec![
        Fake::boxed("characters", &[]),
        Fake::boxed("inventory", &["characters"]),
    ];
    validate_requires(&mods).unwrap();
}

#[test]
fn validate_requires_fails_when_provider_absent() {
    let mods = vec![Fake::boxed("inventory", &["characters"])];
    let err = validate_requires(&mods).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("\"inventory\""), "{msg}");
    assert!(msg.contains("\"characters\""), "{msg}");
    assert!(msg.contains("no provider is present"), "{msg}");
}

#[test]
fn validate_requires_satisfied_by_remote_stub() {
    // The provider is a name-only stand-in (as `remote::Stub` will be) reporting
    // the provider's name — the name-based check can't tell it from a real module.
    let mods = vec![
        Fake::boxed("characters", &[]), // stub for a peer's `characters`
        Fake::boxed("inventory", &["characters"]),
    ];
    validate_requires(&mods).unwrap();
}

#[test]
fn config_defaults_when_env_absent() {
    let cfg = Config::from_values(None, None, None, None);
    assert_eq!(cfg.database_url.as_deref(), Some(DEFAULT_DSN));
    assert_eq!(cfg.listen_addr, ":8080");
    assert_eq!(cfg.edge_addr, ":9000");
    assert_eq!(cfg.player_edge_addr, ":9100");
}

#[test]
fn config_defaults_when_env_blank() {
    let cfg = Config::from_values(
        Some("  ".into()),
        Some("".into()),
        Some("   ".into()),
        Some(" ".into()),
    );
    assert_eq!(cfg.database_url.as_deref(), Some(DEFAULT_DSN));
    assert_eq!(cfg.listen_addr, ":8080");
    assert_eq!(cfg.edge_addr, ":9000");
    assert_eq!(cfg.player_edge_addr, ":9100");
}

#[test]
fn config_overrides_from_env() {
    let cfg = Config::from_values(
        Some("postgres://u:p@db:5432/x".into()),
        Some("9090".into()),
        Some(":9001".into()),
        Some(":9101".into()),
    );
    assert_eq!(cfg.database_url.as_deref(), Some("postgres://u:p@db:5432/x"));
    // Bare port gets the leading colon (Go's normalizeAddr).
    assert_eq!(cfg.listen_addr, ":9090");
    assert_eq!(cfg.edge_addr, ":9001");
    assert_eq!(cfg.player_edge_addr, ":9101");
}

#[test]
fn config_accepts_colon_port_form() {
    let cfg = Config::from_values(None, Some(":8081".into()), None, None);
    assert_eq!(cfg.listen_addr, ":8081");
}

#[test]
fn without_db_clears_dsn_and_keeps_the_rest() {
    let cfg = Config::from_values(
        Some("postgres://u:p@db:5432/x".into()),
        Some("9090".into()),
        Some(":9001".into()),
        Some(":9101".into()),
    )
    .without_db();
    assert_eq!(cfg.database_url, None);
    // Everything else survives the opt-out.
    assert_eq!(cfg.listen_addr, ":9090");
    assert_eq!(cfg.edge_addr, ":9001");
    assert_eq!(cfg.player_edge_addr, ":9101");
}

#[test]
fn to_bind_addr_expands_colon_port() {
    assert_eq!(to_bind_addr(":9000"), "0.0.0.0:9000");
    assert_eq!(to_bind_addr("127.0.0.1:9000"), "127.0.0.1:9000");
}
