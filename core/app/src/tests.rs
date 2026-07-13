use super::*;

#[test]
fn player_request_limit_defaults_and_burst_semantics() {
    let cfg = Config::from_values(None, None, None, None, None, None, None, None, None, None);
    assert_eq!(cfg.player_rate_limit_rps, 20.0);
    assert_eq!(cfg.player_rate_limit_burst, 40);
    assert_eq!(cfg.player_conn_rate_limit_rps, 10.0);
    assert_eq!(cfg.player_conn_rate_limit_burst, 20);
    assert_eq!(parse_number(Some("0"), 20u32), 0);
    assert_eq!(parse_number(Some("bad"), 40u32), 40);
}

#[test]
fn rate_parser_table() {
    let cases = [
        ("unset", None, RateZeroPolicy::Allow, Ok(None)),
        ("blank", Some("  "), RateZeroPolicy::Reject, Ok(None)),
        (
            "positive",
            Some(" 7.5 "),
            RateZeroPolicy::Allow,
            Ok(Some(7.5)),
        ),
        (
            "zero allowed",
            Some("0"),
            RateZeroPolicy::Allow,
            Ok(Some(0.0)),
        ),
        (
            "zero rejected",
            Some("0"),
            RateZeroPolicy::Reject,
            Err(RateParseError::ZeroRejected),
        ),
        (
            "negative",
            Some("-0.25"),
            RateZeroPolicy::Allow,
            Err(RateParseError::Negative),
        ),
        (
            "nan",
            Some("NaN"),
            RateZeroPolicy::Allow,
            Err(RateParseError::NonFinite),
        ),
        (
            "infinity",
            Some("inf"),
            RateZeroPolicy::Allow,
            Err(RateParseError::NonFinite),
        ),
        (
            "negative infinity",
            Some("-inf"),
            RateZeroPolicy::Allow,
            Err(RateParseError::NonFinite),
        ),
        (
            "malformed",
            Some("fast"),
            RateZeroPolicy::Allow,
            Err(RateParseError::Malformed),
        ),
    ];

    for (name, value, zero_policy, expected) in cases {
        assert_eq!(parse_rate(value, zero_policy), expected, "{name}");
    }
}

#[test]
fn rate_resolver_applies_surface_fallbacks_and_zero_policy() {
    let cases = [
        (
            "gateway rejects zero",
            Some("0"),
            20.0,
            RateZeroPolicy::Reject,
            ResolvedRate {
                value: 20.0,
                invalid: Some(RateParseError::ZeroRejected),
            },
        ),
        (
            "gateway invalid falls back on",
            Some("bad"),
            20.0,
            RateZeroPolicy::Reject,
            ResolvedRate {
                value: 20.0,
                invalid: Some(RateParseError::Malformed),
            },
        ),
        (
            "module zero stays off",
            Some("0"),
            0.0,
            RateZeroPolicy::Allow,
            ResolvedRate {
                value: 0.0,
                invalid: None,
            },
        ),
        (
            "module invalid falls back off",
            Some("-1"),
            0.0,
            RateZeroPolicy::Allow,
            ResolvedRate {
                value: 0.0,
                invalid: Some(RateParseError::Negative),
            },
        ),
        (
            "player per-ip allows zero",
            Some("0"),
            20.0,
            RateZeroPolicy::Allow,
            ResolvedRate {
                value: 0.0,
                invalid: None,
            },
        ),
        (
            "player per-ip invalid uses default",
            Some("NaN"),
            20.0,
            RateZeroPolicy::Allow,
            ResolvedRate {
                value: 20.0,
                invalid: Some(RateParseError::NonFinite),
            },
        ),
        (
            "player per-connection invalid uses default",
            Some("many"),
            10.0,
            RateZeroPolicy::Allow,
            ResolvedRate {
                value: 10.0,
                invalid: Some(RateParseError::Malformed),
            },
        ),
    ];

    for (name, value, default, zero_policy, expected) in cases {
        assert_eq!(
            resolve_rate(value, default, zero_policy),
            expected,
            "{name}"
        );
    }
}

#[test]
fn normalize_mailto_avoids_double_prefix() {
    assert_eq!(normalize_mailto("you@example.com"), "mailto:you@example.com");
    assert_eq!(
        normalize_mailto("mailto:you@example.com"),
        "mailto:you@example.com"
    );
}

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
    let cfg = Config::from_values(None, None, None, None, None, None, None, None, None, None);
    assert_eq!(cfg.database_url.as_deref(), Some(DEFAULT_DSN));
    assert_eq!(cfg.listen_addr, ":8080");
    assert_eq!(cfg.edge_addr, ":9000");
    assert_eq!(cfg.player_edge_addr, ":9100");
    // Player-QUIC connection caps default to their env-owned baselines.
    assert_eq!(cfg.player_max_conns, 1024);
    assert_eq!(cfg.player_max_conns_per_ip, 32);
}

#[test]
fn config_player_conn_caps_override_and_fall_back() {
    // Explicit values parse; blank/unparseable falls back to the defaults (the grace-knob
    // shape), and `0` is an accepted explicit opt-out (unlimited).
    let cfg = Config::from_values(
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some("4096".into()),
        Some("8".into()),
    );
    assert_eq!(cfg.player_max_conns, 4096);
    assert_eq!(cfg.player_max_conns_per_ip, 8);

    let cfg = Config::from_values(
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some("  ".into()),
        Some("not-a-number".into()),
    );
    assert_eq!(cfg.player_max_conns, 1024);
    assert_eq!(cfg.player_max_conns_per_ip, 32);

    let cfg = Config::from_values(
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some("0".into()),
        Some("0".into()),
    );
    assert_eq!(cfg.player_max_conns, 0);
    assert_eq!(cfg.player_max_conns_per_ip, 0);
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
        None,
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
        None,
        None,
        None,
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
    let cfg = Config::from_values(None, None, None, None, None, None, None, None, None, None);
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
        None,
        None,
        None,
    );
    assert_eq!(cfg.edge_drain_grace, std::time::Duration::from_millis(5000));
    assert_eq!(cfg.http_drain_grace, std::time::Duration::from_millis(5000));
    assert_eq!(
        cfg.module_stop_grace,
        std::time::Duration::from_millis(5000)
    );
}

#[test]
fn config_http_request_timeout_default_override_and_zero_disables() {
    // Default ON at 30s when the env var is unset/blank/unparseable (grace-knob shape).
    let cfg = Config::from_values(None, None, None, None, None, None, None, None, None, None);
    assert_eq!(
        cfg.http_request_timeout,
        Some(std::time::Duration::from_millis(30000))
    );
    let cfg = Config::from_values(
        None, None, None, None, None, None, None,
        Some("not-a-number".into()),
        None, None,
    );
    assert_eq!(
        cfg.http_request_timeout,
        Some(std::time::Duration::from_millis(30000))
    );
    // Explicit positive value overrides.
    let cfg = Config::from_values(
        None, None, None, None, None, None, None,
        Some("1500".into()),
        None, None,
    );
    assert_eq!(
        cfg.http_request_timeout,
        Some(std::time::Duration::from_millis(1500))
    );
    // Explicit `0` disables the layer entirely.
    let cfg = Config::from_values(
        None, None, None, None, None, None, None,
        Some("0".into()),
        None, None,
    );
    assert_eq!(cfg.http_request_timeout, None);
    // The builder mirrors the env: a zero Duration also disables.
    let cfg = Config::from_values(None, None, None, None, None, None, None, None, None, None)
        .with_request_timeout_default(std::time::Duration::from_secs(5));
    assert_eq!(
        cfg.http_request_timeout,
        Some(std::time::Duration::from_secs(5))
    );
    let cfg = Config::from_values(None, None, None, None, None, None, None, None, None, None)
        .with_request_timeout_default(std::time::Duration::ZERO);
    assert_eq!(cfg.http_request_timeout, None);
}

#[test]
fn config_accepts_colon_port_form() {
    let cfg =
        Config::from_values(None, Some(":8081".into()), None, None, None, None, None, None, None, None);
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
    let cfg = Config::from_values(None, None, None, None, None, None, None, None, None, None);
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

#[tokio::test]
async fn readiness_snapshot_freezes_membership_but_keeps_checks_live() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let ctx = Context::new();
    let healthy = Arc::new(AtomicBool::new(false));
    let observed = healthy.clone();
    ctx.contribute(
        httpmw::READINESS_SLOT,
        httpmw::ReadyCheck::new("live", move || {
            let observed = observed.clone();
            async move {
                observed
                    .load(Ordering::SeqCst)
                    .then_some(())
                    .ok_or_else(|| "not ready".to_string())
            }
        }),
    );

    let snapshot = snapshot_readiness_checks(&ctx);
    ctx.contribute(
        httpmw::READINESS_SLOT,
        httpmw::ReadyCheck::new("late", || async { Ok(()) }),
    );
    assert_eq!(snapshot.len(), 1, "snapshot membership must stay fixed");
    assert_eq!(snapshot[0].name(), "live");

    assert_eq!(snapshot[0].run().await.unwrap_err(), "not ready");
    healthy.store(true, Ordering::SeqCst);
    snapshot[0].run().await.unwrap();
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
    ordered_teardown(None, None, std::time::Duration::from_millis(100), None, None, ctx, None).await;
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
    ordered_teardown(
        None,
        None,
        std::time::Duration::from_millis(100),
        None,
        None,
        ctx,
        Some(std::sync::Arc::new(app)),
    )
    .await;
    assert_eq!(*log.lock().unwrap(), vec!["stop:stoprec"]);
}

#[test]
fn to_bind_addr_expands_colon_port() {
    assert_eq!(to_bind_addr(":9000"), "0.0.0.0:9000");
    assert_eq!(to_bind_addr("127.0.0.1:9000"), "127.0.0.1:9000");
}

// ============================================================================
// Native TLS front (admin hardening Step 4): `Config::with_tls` plumbing + the
// files-mode TLS branch served for real — rcgen-minted localhost cert, reqwest
// (rustls/ring, the same provider stack) trusting it, and the SAME watch-signal
// graceful shutdown contract as the plain branch. ACME is not E2E-testable locally
// (needs a public domain); its config parsing is unit-tested in cmd/gateway-svc.
// ============================================================================

#[test]
fn with_tls_sets_the_front_and_default_is_off() {
    let cfg = Config::from_values(None, None, None, None, None, None, None, None, None, None);
    assert_eq!(cfg.tls, None, "TLS is opt-in; every existing process stays plain");

    let front = TlsFront::Files {
        cert: std::path::PathBuf::from("c.pem"),
        key: std::path::PathBuf::from("k.pem"),
    };
    let cfg = cfg.without_db().with_tls(Some(front.clone()));
    assert_eq!(cfg.tls, Some(front));
    // The other builder opt-outs survive alongside it.
    assert_eq!(cfg.database_url, None);
    // And with_tls(None) is an explicit off.
    assert_eq!(cfg.with_tls(None).tls, None);
}

/// Mints a self-signed localhost cert (SANs: localhost + 127.0.0.1) and writes the
/// PEM pair under a unique temp dir; returns (cert_path, key_path, cert_pem).
fn mint_localhost_cert(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, String) {
    let ck = rcgen::generate_simple_self_signed(vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
    ])
    .expect("mint cert");
    let dir = std::env::temp_dir().join(format!("app-tls-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk temp dir");
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    let cert_pem = ck.cert.pem();
    std::fs::write(&cert_path, &cert_pem).expect("write cert");
    std::fs::write(&key_path, ck.key_pair.serialize_pem()).expect("write key");
    (cert_path, key_path, cert_pem)
}

/// A reqwest client that trusts ONLY the freshly minted test cert.
fn tls_client(cert_pem: &str) -> reqwest::Client {
    let ca = reqwest::Certificate::from_pem(cert_pem.as_bytes()).expect("parse test ca");
    reqwest::Client::builder()
        .add_root_certificate(ca)
        .build()
        .expect("build client")
}

/// Picks an OS-assigned free port (bind :0, read, drop — fine for a local test).
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("probe port")
        .local_addr()
        .expect("local addr")
        .port()
}

/// Polls `url` over the trusted-TLS client until the server answers (bounded).
async fn get_when_up(client: &reqwest::Client, url: &str) -> reqwest::Response {
    for _ in 0..100 {
        match client.get(url).send().await {
            Ok(resp) => return resp,
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
        }
    }
    panic!("server never came up at {url}");
}

/// The TLS serve path end-to-end: `serve_https` (the exact branch `run` dispatches
/// to) serves the router over HTTPS from PEM files, and flipping the SAME watch
/// signal `run` wires drains and returns `Ok` within the grace bound.
#[tokio::test(flavor = "multi_thread")]
async fn serve_https_files_serves_router_and_drains_on_signal() {
    // Mirror production: the front-door main pins the ring provider (idempotent).
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (cert, key, cert_pem) = mint_localhost_cert("direct");
    let port = free_port();
    let bind = format!("127.0.0.1:{port}");

    let router = axum::Router::new().route("/hello", get(|| async { "tls ok" }));
    let (sig_tx, sig_rx) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(async move {
        serve_https(
            TlsFront::Files { cert, key },
            &bind,
            router,
            std::time::Duration::from_secs(2),
            sig_rx,
        )
        .await
    });

    let client = tls_client(&cert_pem);
    let resp = get_when_up(&client, &format!("https://127.0.0.1:{port}/hello")).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "tls ok");
    drop(client); // close the pooled keep-alive connection so the drain is instant

    // Graceful shutdown: the same signal `run` sends on Ctrl-C/SIGTERM.
    sig_tx.send(true).unwrap();
    let served = tokio::time::timeout(std::time::Duration::from_secs(10), server)
        .await
        .expect("drain-bounded shutdown must not hang")
        .expect("serve task panicked");
    served.expect("serve_https returned an error");
}

/// The wiring through the REAL boot path: `app::run` with `with_tls(Files)` on an
/// ephemeral port serves `/healthz` over HTTPS — proving `run` dispatches the TLS
/// branch off `Config.tls` (DB-less, empty module set; `run` only returns on a
/// process signal, so the task is aborted once the roundtrip proved the point).
#[tokio::test(flavor = "multi_thread")]
async fn run_with_tls_files_serves_https() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (cert, key, cert_pem) = mint_localhost_cert("run");
    let port = free_port();

    let mut cfg =
        Config::from_values(None, None, None, None, None, None, None, None, None, None).without_db();
    cfg.listen_addr = format!("127.0.0.1:{port}");
    let cfg = cfg.with_tls(Some(TlsFront::Files { cert, key }));

    let server = tokio::spawn(run(cfg, Vec::new(), None, None));

    let client = tls_client(&cert_pem);
    let resp = get_when_up(&client, &format!("https://127.0.0.1:{port}/healthz")).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "ok");

    server.abort();
}

// ============================================================================
// Whole-request inbound HTTP timeout (round 4, finding 3): `run` wraps the served
// router in a `tower_http::timeout::TimeoutLayer` (default 30s, `0` disables) that
// emits 408 on elapse. Exercised through the REAL plain-HTTP boot path (`run`,
// DB-less) with a route-mounting module — the same branch every process serves on.
// ============================================================================

/// A test module that mounts a slow route (`/slow`, sleeps `slow_ms`) and an instant
/// one (`/fast`) — enough to prove the timeout layer trips the slow leg and leaves the
/// fast leg untouched.
struct SlowRoutes {
    slow_ms: u64,
}

#[async_trait::async_trait]
impl Module for SlowRoutes {
    fn name(&self) -> &str {
        "slowroutes"
    }

    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        let slow_ms = self.slow_ms;
        let router = axum::Router::new()
            .route(
                "/slow",
                get(move || async move {
                    tokio::time::sleep(std::time::Duration::from_millis(slow_ms)).await;
                    "slow"
                }),
            )
            .route("/fast", get(|| async { "fast" }));
        ctx.mount(router);
        Ok(())
    }
}

/// A short-configured timeout trips the slow handler (408) but leaves a fast request
/// untouched (200) — proving `run` applied the layer on the plain-HTTP branch.
#[tokio::test(flavor = "multi_thread")]
async fn run_http_request_timeout_408s_slow_handler_but_not_fast() {
    let port = free_port();
    let mut cfg = Config::from_values(None, None, None, None, None, None, None, None, None, None)
        .without_db();
    cfg.listen_addr = format!("127.0.0.1:{port}");
    cfg.http_request_timeout = Some(std::time::Duration::from_millis(150));

    let server = tokio::spawn(run(cfg, vec![Box::new(SlowRoutes { slow_ms: 5000 })], None, None));
    let client = reqwest::Client::new();

    // A fast request is unaffected by the layer.
    let fast = get_when_up(&client, &format!("http://127.0.0.1:{port}/fast")).await;
    assert_eq!(fast.status(), reqwest::StatusCode::OK);
    assert_eq!(fast.text().await.unwrap(), "fast");

    // The slow handler exceeds the 150ms budget → 408 Request Timeout (deliberately 408,
    // not 504 — see the layer-site comment in `run`).
    let slow = client
        .get(format!("http://127.0.0.1:{port}/slow"))
        .send()
        .await
        .expect("slow request");
    assert_eq!(slow.status(), reqwest::StatusCode::REQUEST_TIMEOUT);

    server.abort();
}

/// `http_request_timeout = None` (env `HTTP_REQUEST_TIMEOUT_MS=0`) drops the layer:
/// a slow handler runs to completion with 200.
#[tokio::test(flavor = "multi_thread")]
async fn run_http_request_timeout_zero_disables_the_layer() {
    let port = free_port();
    let mut cfg = Config::from_values(None, None, None, None, None, None, None, None, None, None)
        .without_db();
    cfg.listen_addr = format!("127.0.0.1:{port}");
    cfg.http_request_timeout = None; // the `0` disables-case

    let server = tokio::spawn(run(cfg, vec![Box::new(SlowRoutes { slow_ms: 300 })], None, None));
    let client = reqwest::Client::new();

    // With no layer, the 300ms handler completes instead of being timed out.
    let slow = get_when_up(&client, &format!("http://127.0.0.1:{port}/slow")).await;
    assert_eq!(slow.status(), reqwest::StatusCode::OK);
    assert_eq!(slow.text().await.unwrap(), "slow");

    server.abort();
}

// ============================================================================
// Plain-HTTP graceful drain owns its connection tasks (Step 8). `serve_http` (the
// exact branch `run` dispatches to for a non-TLS process) serves through
// axum-server's `Handle` — each connection task owns its hyper future and a
// grace-expiry abort DROPS it in place (true cancellation), NOT the detached-task
// leak `axum::serve` produced. Driven directly with the same watch signal `run`
// wires (like `serve_https_files_serves_router_and_drains_on_signal`), so the test
// SYNCHRONIZES on the handler actually running rather than sleeping past a race —
// the old `axum::serve` code would FALSE-PASS a sleep-only test because the outer
// select! returned before the detached handler resumed.
// ============================================================================

/// The negative path the fix closes: a handler stuck well past the drain grace must
/// be CANCELLED at grace expiry — its future dropped in place — and must therefore
/// never wake to run its post-sleep body. On the old detached-task code the abandoned
/// handler would resume after `serve` had already returned (teardown underway) and set
/// both flags; post-fix it stays false because the future was dropped.
#[tokio::test(flavor = "multi_thread")]
async fn serve_http_cancels_hung_handler_at_grace_and_never_wakes_it() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    // Signalled the instant the handler starts executing (synchronize, don't sleep).
    let entered = Arc::new(tokio::sync::Notify::new());
    // Set once `serve_http` returns — i.e. ordered teardown is beginning; a handler that
    // wakes AFTER this and reads it is exactly the abandoned-task defect.
    let stopped = Arc::new(AtomicBool::new(false));
    // The handler's post-sleep effects — must stay false (handler cancelled, never woke).
    let handler_finished = Arc::new(AtomicBool::new(false));
    let touched_after_stop = Arc::new(AtomicBool::new(false));

    let (e, st, hf, tas) = (
        entered.clone(),
        stopped.clone(),
        handler_finished.clone(),
        touched_after_stop.clone(),
    );
    let router = axum::Router::new().route(
        "/hang",
        get(move || {
            let (e, st, hf, tas) = (e.clone(), st.clone(), hf.clone(), tas.clone());
            async move {
                e.notify_one();
                // Sleeps WELL past the drain grace below; on the fixed path the enclosing
                // future is dropped before this resolves.
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                // Only reachable if the handler was NOT cancelled — records whether it woke
                // after teardown had begun.
                tas.store(st.load(Ordering::SeqCst), Ordering::SeqCst);
                hf.store(true, Ordering::SeqCst);
                "done"
            }
        }),
    );

    let port = free_port();
    let bind = format!("127.0.0.1:{port}");
    let grace = std::time::Duration::from_millis(300);
    let (sig_tx, sig_rx) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(async move { serve_http(&bind, router, grace, sig_rx).await });

    // Fire the request that hangs inside the handler (own task; we never expect a body).
    let hang = tokio::spawn(async move {
        let client = reqwest::Client::new();
        let _ = client
            .get(format!("http://127.0.0.1:{port}/hang"))
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await;
    });

    // SYNCHRONIZE: wait until the handler is actually running, not a blind sleep.
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .expect("handler never entered");

    // Flip the shutdown signal; `serve_http` must return within ~grace (drain then abort),
    // NOT wait for the 2s handler.
    sig_tx.send(true).unwrap();
    let served = tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .expect("serve_http must return within the drain grace, not hang on the handler")
        .expect("serve task panicked");
    served.expect("serve_http returned an error");

    // Teardown has begun.
    stopped.store(true, Ordering::SeqCst);

    // Wait comfortably past the handler's own 2s sleep: on the OLD detached-task code the
    // abandoned handler WOULD wake by now and set both flags. Post-fix its future was
    // dropped at grace, so it never resumes.
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    assert!(
        !handler_finished.load(Ordering::SeqCst),
        "hung handler must be cancelled at grace, not run to completion after teardown"
    );
    assert!(
        !touched_after_stop.load(Ordering::SeqCst),
        "cancelled handler must not touch state after teardown began"
    );

    hang.abort();
}

/// The graceful half of the same ordering: a request already in flight when the signal
/// fires that finishes WITHIN the grace must complete successfully (drain first, abort
/// only on grace expiry) — proving the fix drains gracefully and does not hard-kill
/// live connections the instant the signal arrives.
#[tokio::test(flavor = "multi_thread")]
async fn serve_http_completes_in_flight_request_within_grace() {
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let e = entered.clone();
    let router = axum::Router::new().route(
        "/drain",
        get(move || {
            let e = e.clone();
            async move {
                e.notify_one();
                // Finishes well WITHIN the 2s grace below.
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                "drained"
            }
        }),
    );

    let port = free_port();
    let bind = format!("127.0.0.1:{port}");
    let grace = std::time::Duration::from_secs(2);
    let (sig_tx, sig_rx) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(async move { serve_http(&bind, router, grace, sig_rx).await });

    let resp = tokio::spawn(async move {
        reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/drain"))
            .send()
            .await
    });

    // SYNCHRONIZE on the handler running, then flip shutdown WHILE it is in flight.
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .expect("handler never entered");
    sig_tx.send(true).unwrap();

    // The in-flight request must still get its response during the graceful drain.
    let resp = tokio::time::timeout(std::time::Duration::from_secs(5), resp)
        .await
        .expect("in-flight request hung")
        .expect("request task panicked")
        .expect("request errored");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "drained");

    let served = tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .expect("serve_http must return after the drain")
        .expect("serve task panicked");
    served.expect("serve_http returned an error");
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

// ============================================================================
// Startup order (event-plane hardening Step 2.1): the invalidation plane's
// synchronous first refresh completes BEFORE durable delivery starts — a durable
// handler reading a replica-local cache must never run against a cold cache.
// Live-Postgres test through the REAL boot path (`run`).
// ============================================================================

/// A probe module: registers an invalidation callback with a deliberately SLOW
/// first refresh (2s — under the old `plane.start()`-first order delivery would
/// race ahead of it) plus a durable subscription whose handler records how many
/// refreshes had completed at delivery time; `start` emits the probe event.
struct OrderProbe {
    topic: &'static str,
    sub_id: &'static str,
    channel: &'static str,
    refreshes: std::sync::Arc<std::sync::atomic::AtomicU32>,
    /// Refresh count observed by the durable handler; -1 = not delivered yet.
    refreshes_at_delivery: std::sync::Arc<std::sync::atomic::AtomicI32>,
}

#[async_trait::async_trait]
impl Module for OrderProbe {
    fn name(&self) -> &str {
        "orderprobe"
    }

    fn init(&self, ctx: &Context) -> anyhow::Result<()> {
        use std::sync::atomic::Ordering;
        let refreshes = self.refreshes.clone();
        ctx.invalidation().register(self.channel, "orderprobe", move || {
            let refreshes = refreshes.clone();
            async move {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                refreshes.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });

        let et = bus::define::<serde_json::Value>(
            self.topic,
            1,
            bus::HistoryPolicy::MinRetention { days: 7 },
        );
        let refreshes = self.refreshes.clone();
        let at_delivery = self.refreshes_at_delivery.clone();
        ctx.bus().on_tx(
            bus::SubscriptionSpec {
                id: self.sub_id,
                start: bus::StartPosition::Genesis,
            },
            &et,
            move |_delivery, _v: serde_json::Value| {
                let refreshes = refreshes.clone();
                let at_delivery = at_delivery.clone();
                Box::pin(async move {
                    at_delivery
                        .store(refreshes.load(Ordering::SeqCst) as i32, Ordering::SeqCst);
                    Ok(())
                })
            },
        );
        Ok(())
    }

    async fn start(&self, ctx: &Context) -> anyhow::Result<()> {
        // Module start runs BEFORE both planes start: the probe event is already
        // in the log when the first delivery pass begins.
        let pool = ctx.db().expect("db-backed test").clone();
        let et = bus::define::<serde_json::Value>(
            self.topic,
            1,
            bus::HistoryPolicy::MinRetention { days: 7 },
        );
        let mut tx = pool.begin().await?;
        ctx.bus()
            .emit_tx(
                bus::AnyTx::new(&mut *tx),
                &et,
                &serde_json::json!({ "probe": self.topic }),
            )
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn durable_delivery_starts_only_after_invalidation_first_refresh() {
    use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
    use std::sync::Arc;

    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let pool = match tokio::time::timeout(
        std::time::Duration::from_secs(3),
        sqlx::PgPool::connect(&dsn),
    )
    .await
    {
        Ok(Ok(p)) => p,
        _ => {
            eprintln!("SKIP: postgres unreachable at {dsn} — startup-order test skipped");
            return;
        }
    };

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let pid = std::process::id();
    let topic = leak(format!("app.startorder.{pid}-{nanos}"));
    let sub_id = leak(format!("app.startorder-sub.{pid}-{nanos}"));
    let channel = leak(format!("app_startorder_{pid}_{nanos}"));

    let refreshes = Arc::new(AtomicU32::new(0));
    let at_delivery = Arc::new(AtomicI32::new(-1));

    let mut cfg =
        Config::from_values(Some(dsn.clone()), None, None, None, None, None, None, None, None, None);
    cfg.listen_addr = format!("127.0.0.1:{}", free_port());

    let probe = OrderProbe {
        topic,
        sub_id,
        channel,
        refreshes: refreshes.clone(),
        refreshes_at_delivery: at_delivery.clone(),
    };
    let server = tokio::spawn(run(cfg, vec![Box::new(probe)], None, None));

    // First refresh sleeps 2s, delivery polls at a 1s floor and frontier
    // eligibility can lag behind unrelated transactions — poll generously.
    let mut delivered = false;
    for _ in 0..300 {
        if at_delivery.load(Ordering::SeqCst) >= 0 {
            delivered = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    server.abort();
    assert!(delivered, "the probe event was never delivered");
    assert!(
        at_delivery.load(Ordering::SeqCst) >= 1,
        "durable delivery ran before the invalidation plane's first refresh completed"
    );

    let _ = sqlx::query("DELETE FROM asyncevents.events WHERE topic = $1")
        .bind(topic)
        .execute(&pool)
        .await;
    let _ = sqlx::query("DELETE FROM asyncevents.subscriptions WHERE subscription_id = $1")
        .bind(sub_id)
        .execute(&pool)
        .await;
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

#[test]
fn retention_readiness_message_preserves_plane_threshold_precision() {
    assert_eq!(
        retention_stall_message(std::time::Duration::from_millis(1500)),
        "asyncevents retention sweep has not succeeded in >1.5s"
    );
}
