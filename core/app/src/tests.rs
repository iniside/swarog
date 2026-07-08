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
fn metrics_enabled_by_default_and_survives_without_db() {
    // Every module-hosting process gets metrics by default; dropping the DB (a DB-less
    // module host like admin-svc) does NOT drop metrics — the two concerns are separate.
    let cfg = Config::from_values(None, None, None, None);
    assert!(cfg.metrics_enabled);
    assert!(cfg.without_db().metrics_enabled);
}

#[test]
fn without_metrics_opts_out_and_keeps_the_rest() {
    let cfg = Config::from_values(
        Some("postgres://u:p@db:5432/x".into()),
        Some("9090".into()),
        None,
        None,
    )
    .without_db()
    .without_metrics();
    assert!(!cfg.metrics_enabled);
    // The other opt-outs/values survive.
    assert_eq!(cfg.database_url, None);
    assert_eq!(cfg.listen_addr, ":9090");
}

#[test]
fn to_bind_addr_expands_colon_port() {
    assert_eq!(to_bind_addr(":9000"), "0.0.0.0:9000");
    assert_eq!(to_bind_addr("127.0.0.1:9000"), "127.0.0.1:9000");
}

// ============================================================================
// The EDGE_SLOT drain (Step 3): modules contribute EdgeReg unconditionally in
// init; `run` applies them only when the process has an internal edge server.
// ============================================================================

/// The edge-hosting path: everything contributed to EDGE_SLOT is applied to the
/// process's edge server, in contribution order.
#[test]
fn contributed_edge_registrations_are_applied_when_an_edge_server_exists() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let ctx = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        let counted = calls.clone();
        ctx.contribute(
            edge::EDGE_SLOT,
            edge::EdgeReg::new(move |_s: &mut edge::Server| {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        );
    }

    let mut server = edge::Server::new();
    let applied = apply_edge_registrations(&ctx, &mut server);
    assert_eq!(applied, 2);
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // A hypothetical re-drain double-registers nothing: each EdgeReg is one-shot.
    let applied_again = apply_edge_registrations(&ctx, &mut server);
    assert_eq!(applied_again, 2, "the slot still holds the (spent) contributions");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "but no closure runs twice");
}

/// The monolith path: `run` never drains the slot (no edge server), so a
/// contributed registration is silently skipped — no effect, no error.
#[test]
fn contributed_edge_registrations_are_silently_skipped_without_an_edge_server() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let ctx = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    ctx.contribute(
        edge::EDGE_SLOT,
        edge::EdgeReg::new(move |_s: &mut edge::Server| {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
    );

    // The monolith simply never calls apply_edge_registrations. Dropping the
    // context (and with it the unapplied closure) is the whole story.
    drop(ctx);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
