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
    let cfg = Config::from_values(None, None, None, None, None, None, None);
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
        Some("  ".into()),
        Some("  ".into()),
        Some("  ".into()),
    );
    assert_eq!(cfg.database_url.as_deref(), Some(DEFAULT_DSN));
    assert_eq!(cfg.listen_addr, ":8080");
    assert_eq!(cfg.edge_addr, ":9000");
    assert_eq!(cfg.player_edge_addr, ":9100");
    assert_eq!(cfg.edge_drain_grace, std::time::Duration::from_millis(5000));
    assert_eq!(cfg.http_drain_grace, std::time::Duration::from_millis(5000));
    assert_eq!(
        cfg.module_stop_grace,
        std::time::Duration::from_millis(5000)
    );
}

#[test]
fn config_overrides_from_env() {
    let cfg = Config::from_values(
        Some("postgres://u:p@db:5432/x".into()),
        Some("9090".into()),
        Some(":9001".into()),
        Some(":9101".into()),
        Some("250".into()),
        Some("750".into()),
        Some("1200".into()),
    );
    assert_eq!(cfg.database_url.as_deref(), Some("postgres://u:p@db:5432/x"));
    // Bare port gets the leading colon (Go's normalizeAddr).
    assert_eq!(cfg.listen_addr, ":9090");
    assert_eq!(cfg.edge_addr, ":9001");
    assert_eq!(cfg.player_edge_addr, ":9101");
    assert_eq!(cfg.edge_drain_grace, std::time::Duration::from_millis(250));
    assert_eq!(cfg.http_drain_grace, std::time::Duration::from_millis(750));
    assert_eq!(
        cfg.module_stop_grace,
        std::time::Duration::from_millis(1200)
    );
}

#[test]
fn config_drain_grace_defaults_when_unset_or_unparseable() {
    let cfg = Config::from_values(None, None, None, None, None, None, None);
    assert_eq!(cfg.edge_drain_grace, std::time::Duration::from_millis(5000));
    assert_eq!(cfg.http_drain_grace, std::time::Duration::from_millis(5000));
    let cfg = Config::from_values(
        None,
        None,
        None,
        None,
        Some("not-a-number".into()),
        Some("not-a-number".into()),
        Some("not-a-number".into()),
    );
    assert_eq!(cfg.edge_drain_grace, std::time::Duration::from_millis(5000));
    assert_eq!(cfg.http_drain_grace, std::time::Duration::from_millis(5000));
    assert_eq!(
        cfg.module_stop_grace,
        std::time::Duration::from_millis(5000)
    );
}

#[test]
fn config_accepts_colon_port_form() {
    let cfg = Config::from_values(None, Some(":8081".into()), None, None, None, None, None);
    assert_eq!(cfg.listen_addr, ":8081");
}

#[test]
fn without_db_clears_dsn_and_keeps_the_rest() {
    let cfg = Config::from_values(
        Some("postgres://u:p@db:5432/x".into()),
        Some("9090".into()),
        Some(":9001".into()),
        Some(":9101".into()),
        None,
        None,
        None,
    )
    .without_db();
    assert_eq!(cfg.database_url, None);
    // Everything else survives the opt-out.
    assert_eq!(cfg.listen_addr, ":9090");
    assert_eq!(cfg.edge_addr, ":9001");
    assert_eq!(cfg.player_edge_addr, ":9101");
}

#[test]
fn rate_limit_default_off_unless_set() {
    // Module hosts leave it unset (opt-in); the gateway builder turns it always-on.
    let cfg = Config::from_values(None, None, None, None, None, None, None);
    assert_eq!(cfg.rate_limit_default, None);
    let gw = cfg.without_db().with_rate_limit_default(20.0, 40);
    assert_eq!(gw.rate_limit_default, Some((20.0, 40)));
    // The other opt-outs survive alongside it.
    assert_eq!(gw.database_url, None);
}

// ============================================================================
// /readyz fold-in (Step 13): baseline DB ping + contributed ReadyChecks, 503 with a
// per-failed-check JSON body. Exercised without a DB by passing `None` for the pool.
// ============================================================================

/// Reads a Response's status + body into (StatusCode, String).
async fn read_response(resp: axum::response::Response) -> (StatusCode, String) {
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn readyz_all_green_is_200() {
    // No pool (nothing to ping) + a passing check → 200 "ok".
    let checks = vec![httpmw::ReadyCheck::new("cache", || async { Ok(()) })];
    let (status, body) =
        read_response(readyz_response(None, checks, READY_CHECK_TIMEOUT).await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok");
}

#[tokio::test]
async fn readyz_failure_is_503_with_named_json_body() {
    let checks = vec![
        httpmw::ReadyCheck::new("cache", || async { Ok(()) }),
        httpmw::ReadyCheck::new("downstream", || async {
            Err("peer unreachable".to_string())
        }),
    ];
    let (status, body) =
        read_response(readyz_response(None, checks, READY_CHECK_TIMEOUT).await).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    // The body maps the FAILED check's name to its error; the passing one is absent.
    assert_eq!(body, r#"{"downstream":"peer unreachable"}"#);
}

#[tokio::test]
async fn readyz_hung_check_times_out_fast_as_503() {
    // A check that never resolves must not hang /readyz forever — a short bound (instead
    // of the real 2s READY_CHECK_TIMEOUT) keeps this test fast while still exercising the
    // Elapsed → named-failure path.
    let bound = std::time::Duration::from_millis(50);
    let checks = vec![httpmw::ReadyCheck::new("hang", || {
        std::future::pending::<Result<(), String>>()
    })];
    let (status, body) = read_response(readyz_response(None, checks, bound).await).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, format!(r#"{{"hang":"timed out after {bound:?}"}}"#));
}

// ============================================================================
// The startup unwind (security-review hardening Step 3): `ordered_teardown` is the
// ONE teardown sequence for graceful shutdown and every startup-failure path. The
// double-stop rule: `app` is passed only when `App::start` succeeded — after an
// `App::start` failure the started prefix was already stopped inside `App::start`,
// so the unwind must NOT call `App::stop` (it would stop never-started modules).
// ============================================================================

/// A module that records its `stop` calls, so the test can see whether the
/// teardown reached `App::stop`.
struct StopRec {
    log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl Module for StopRec {
    fn name(&self) -> &str {
        "stoprec"
    }
    fn init(&self, _ctx: &Context) -> anyhow::Result<()> {
        Ok(())
    }
    async fn stop(&self, _ctx: &Context) -> anyhow::Result<()> {
        self.log.lock().unwrap().push("stop:stoprec".to_string());
        Ok(())
    }
}

#[tokio::test]
async fn ordered_teardown_skips_module_stop_when_modules_never_started() {
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut app = App::new(std::sync::Arc::new(Context::new()));
    app.add(Box::new(StopRec { log: log.clone() }));
    app.build().unwrap();
    let ctx = app.context().clone();

    // No listeners, no planes, modules never started (`app` omitted) — e.g. a
    // migrate failure or an `App::start` Err: bus close still runs, module stop
    // does NOT.
    ordered_teardown(None, None, std::time::Duration::from_millis(100), &mut None, &mut None, &ctx, None).await;
    assert!(log.lock().unwrap().is_empty(), "stop must not run: {log:?}");
}

#[tokio::test]
async fn ordered_teardown_stops_modules_after_a_successful_start() {
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut app = App::new(std::sync::Arc::new(Context::new()));
    app.add(Box::new(StopRec { log: log.clone() }));
    app.build().unwrap();
    app.start().await.unwrap();
    let ctx = app.context().clone();

    // Modules started → `app` is passed → `App::stop` runs (last, after bus close).
    ordered_teardown(None, None, std::time::Duration::from_millis(100), &mut None, &mut None, &ctx, Some(&app)).await;
    assert_eq!(*log.lock().unwrap(), vec!["stop:stoprec"]);
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

// ============================================================================
// The LAYER_SLOT drain: modules contribute httpmw::HttpLayer in init; `run` applies
// them AFTER rate limiting so the last-contributed layer is the OUTERMOST — the metrics
// recorder wraps the limiter and records its 429s.
// ============================================================================

/// Contributions apply in CONTRIBUTION ORDER. Since `run` adds the rate-limit `.layer`
/// BEFORE this drain, and axum nests a later `.layer` OUTSIDE earlier ones, the (last)
/// contributed metrics layer ends up outermost — so a 429 the limiter issues still flows
/// through it and is recorded. This test pins the order the outer-most guarantee rests on.
#[test]
fn contributed_http_layers_apply_in_contribution_order() {
    use std::sync::{Arc, Mutex as StdMutex};

    let ctx = Context::new();
    let order = Arc::new(StdMutex::new(Vec::<u8>::new()));
    for id in [1u8, 2, 3] {
        let sink = order.clone();
        ctx.contribute(
            httpmw::LAYER_SLOT,
            httpmw::HttpLayer::new(move |r: axum::Router| {
                sink.lock().unwrap().push(id);
                r
            }),
        );
    }

    let _ = apply_http_layers(&ctx, axum::Router::new());
    assert_eq!(*order.lock().unwrap(), vec![1, 2, 3]);

    // A re-drain re-runs nothing: each HttpLayer is one-shot (the closure was taken).
    let _ = apply_http_layers(&ctx, axum::Router::new());
    assert_eq!(*order.lock().unwrap(), vec![1, 2, 3], "spent layers never re-run");
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
