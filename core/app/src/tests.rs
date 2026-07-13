use super::*;

#[test]
fn player_request_limit_defaults_and_burst_semantics() {
    let cfg = Config::from_values(None, None, None, None, None, None, None, None, None, None);
    assert_eq!(cfg.player_rate_limit_rps, 20.0);
    assert_eq!(cfg.player_rate_limit_burst, 40);
    assert_eq!(cfg.player_conn_rate_limit_rps, 10.0);
    assert_eq!(cfg.player_conn_rate_limit_burst, 20);
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

#[test]
fn db_pool_max_resolves_default_on_unset_blank_and_garbage() {
    // Unset → sqlx default (no behavior change when the knob is absent).
    assert_eq!(resolve_pool_max(None), DEFAULT_DB_POOL_MAX);
    assert_eq!(resolve_pool_max(Some("   ")), DEFAULT_DB_POOL_MAX);
    // Garbage falls back to the default (the repo's knob convention: never fatal) —
    // NOT an error, so a typo still boots. A negative parses as garbage for u32.
    assert_eq!(resolve_pool_max(Some("ten")), DEFAULT_DB_POOL_MAX);
    assert_eq!(resolve_pool_max(Some("-5")), DEFAULT_DB_POOL_MAX);
    // A valid value is honored verbatim, including one below the floor (so `run` can
    // tell an explicit misconfiguration apart from a typo).
    assert_eq!(resolve_pool_max(Some(" 32 ")), 32);
    assert_eq!(resolve_pool_max(Some("0")), 0);
    assert_eq!(resolve_pool_max(Some("1")), 1);
}

#[test]
fn db_pool_max_below_floor_fails_startup() {
    // The migrate self-deadlock: an explicit 0 or 1 is rejected loudly; the floor and
    // anything above it passes. Garbage (which resolves to the default) also passes.
    assert!(validate_pool_max(0).is_err());
    assert!(validate_pool_max(1).is_err());
    assert!(validate_pool_max(MIN_DB_POOL_MAX).is_ok());
    assert!(validate_pool_max(DEFAULT_DB_POOL_MAX).is_ok());
    assert!(validate_pool_max(resolve_pool_max(Some("ten"))).is_ok());
}

// ============================================================================
// `env_rate_pair` (Step 13 / DEFECT 1): the single decision authority resolving a
// `(rps, burst)` pair together, closing the gap where a bare `RATE_LIMIT_BURST=0`
// alongside an always-on gateway `rps>0` silently mounted a capacity-0 bucket.
//
// Each test below sets env vars under UNIQUE names (never the real
// `RATE_LIMIT_*`/`PLAYER_RATE_LIMIT_*` knobs) so tests can run in parallel with the
// rest of the suite without a cross-test lock: `env_rate_pair` takes its var NAMES
// as parameters, so per-test-unique names give isolation by construction — no
// other test ever reads or writes them, which is exactly why no ENV_LOCK is needed
// here (a test that mutates a REAL, fixed var name — see
// `run_fails_startup_when_invalidation_poll_interval_is_zero` — must serialize
// instead).
// ============================================================================

/// Sets `vars` for the duration of `f`, restoring prior values after — including on
/// panic, so a failing assertion never leaks env state into a sibling test.
fn with_env_vars<const N: usize>(vars: [(&str, Option<&str>); N], f: impl FnOnce()) {
    let prev: Vec<(String, Option<String>)> = vars
        .iter()
        .map(|(k, _)| ((*k).to_string(), std::env::var(k).ok()))
        .collect();
    for (k, v) in vars {
        match v {
            Some(v) => std::env::set_var(k, v),
            None => std::env::remove_var(k),
        }
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    for (k, v) in prev {
        match v {
            Some(v) => std::env::set_var(&k, v),
            None => std::env::remove_var(&k),
        }
    }
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[test]
fn env_rate_pair_reject_explicit_rps_and_zero_burst_fails_naming_both_vars() {
    with_env_vars(
        [
            ("T13_GW_RPS_A", Some("5")),
            ("T13_GW_BURST_A", Some("0")),
        ],
        || {
            let err = env_rate_pair(
                "T13_GW_RPS_A",
                "T13_GW_BURST_A",
                20.0,
                40,
                RateZeroPolicy::Reject,
            )
            .expect_err("positive rps paired with an explicit zero burst must fail");
            let msg = format!("{err:#}");
            assert!(msg.contains("T13_GW_RPS_A"), "message should name the rps var: {msg}");
            assert!(msg.contains("T13_GW_BURST_A"), "message should name the burst var: {msg}");
        },
    );
}

#[test]
fn env_rate_pair_reject_unset_rps_nonzero_default_and_zero_burst_still_fails() {
    // RPS is left UNSET (so it resolves to the surface's nonzero default, e.g. the
    // gateway's always-on 20.0) while only BURST is explicitly zeroed — the exact
    // shape of DEFECT 1 (a lone `RATE_LIMIT_BURST=0` beside an always-on rps).
    with_env_vars([("T13_GW_BURST_B", Some("0"))], || {
        let err = env_rate_pair(
            "T13_GW_RPS_B",
            "T13_GW_BURST_B",
            20.0,
            40,
            RateZeroPolicy::Reject,
        )
        .expect_err("an effective nonzero rps (from the default) with a zero burst must fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("T13_GW_RPS_B"));
        assert!(msg.contains("T13_GW_BURST_B"));
    });
}

#[test]
fn env_rate_pair_reject_lone_rps_zero_warns_and_defaults_unchanged() {
    // SCOPE PRECISION (i): a LONE `rps=0` under `Reject` keeps today's semantics —
    // warn + fall back to the default rps — and is NOT a pair failure as long as
    // burst is left at its own (nonzero) default. This pins that the new pair check
    // did not change the pre-existing single-value policy.
    with_env_vars([("T13_GW_RPS_C", Some("0"))], || {
        let (rps, burst) = env_rate_pair(
            "T13_GW_RPS_C",
            "T13_GW_BURST_C",
            20.0,
            40,
            RateZeroPolicy::Reject,
        )
        .expect("a lone explicit rps=0 must still warn+default, not fail");
        assert_eq!(rps, 20.0, "explicit zero rps under Reject falls back to the default");
        assert_eq!(burst, 40, "burst is untouched and keeps its own default");
    });
}

#[test]
fn env_rate_pair_reject_explicit_rps_zero_and_burst_zero_still_fails() {
    // The error message's advice must not be circular: `rps=0` under `Reject` cannot
    // disable the limiter (it warns and falls back to the surface default, here 20.0),
    // so an operator who sets BOTH knobs to 0 hoping to turn the front's limiter off
    // still ends up with an effective rps>0 + burst==0 — the capacity-0 bucket — and
    // must get the pair failure, not a silently mounted block-all limiter.
    with_env_vars(
        [
            ("T13_GW_RPS_E", Some("0")),
            ("T13_GW_BURST_E", Some("0")),
        ],
        || {
            let err = env_rate_pair(
                "T13_GW_RPS_E",
                "T13_GW_BURST_E",
                20.0,
                40,
                RateZeroPolicy::Reject,
            )
            .expect_err("rps=0 falls back to the nonzero default, so burst=0 must still fail");
            let msg = format!("{err:#}");
            assert!(msg.contains("T13_GW_RPS_E"));
            assert!(msg.contains("T13_GW_BURST_E"));
        },
    );
}

#[test]
fn env_rate_pair_allow_zero_burst_passes_through_as_disable() {
    // The player plane's own semantics: `burst==0` under `Allow` is that plane's
    // "disable this layer" signal and must pass through unchanged, never rejected.
    with_env_vars(
        [
            ("T13_PLAYER_RPS_D", Some("10")),
            ("T13_PLAYER_BURST_D", Some("0")),
        ],
        || {
            let (rps, burst) = env_rate_pair(
                "T13_PLAYER_RPS_D",
                "T13_PLAYER_BURST_D",
                10.0,
                40,
                RateZeroPolicy::Allow,
            )
            .expect("Allow policy is infallible by construction");
            assert_eq!(rps, 10.0);
            assert_eq!(burst, 0, "burst=0 under Allow stays the player plane's disable signal");
        },
    );
}

/// Serializes the tests that mutate REAL (fixed-name) env vars `run()` itself reads —
/// unlike the `env_rate_pair` tests above, whose parameterized per-test-unique var
/// names need no lock.
static RUN_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Restores an env var to its prior state on drop — including on panic, so a failed
/// assertion (or the select! safety-net) never leaks the mutation into a later test.
struct EnvRestore {
    key: &'static str,
    prev: Option<String>,
}

impl EnvRestore {
    fn set(key: &'static str, val: &str) -> EnvRestore {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, val);
        EnvRestore { key, prev }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

/// C20 propagation, proven at the TRUE boot level: `run()` itself — not
/// `InvalidationPlane::new` called in isolation — fails startup on an explicit
/// `INVALIDATION_POLL_INTERVAL_MS=0`, because the plane is constructed inside `run`
/// (DB ⇒ plane) and its `Result` is threaded with `?`. The plane only exists when a
/// pool was opened, so this needs a live Postgres and skips cleanly (the invalidation
/// crate's DB-test pattern) when unreachable. Awaited directly on this task (not
/// `tokio::spawn` — no `Send` proof of `run`'s future required or wanted, same as the
/// other `run()` tests), with a select! deadline as the safety net: a regression that
/// silently swallows the construction error would otherwise boot a full server here
/// and hang the suite.
#[tokio::test(flavor = "multi_thread")]
// The guard MUST span the `run()` await — `run` reads the var mid-flight, so releasing
// earlier would let a parallel mutator race the read. Safe here: `#[tokio::test]`
// drives this future via `block_on` on one thread (no `Send` requirement), so the
// std MutexGuard is never moved across threads and cannot deadlock the runtime's
// worker pool the way the lint guards against.
#[allow(clippy::await_holding_lock)]
async fn run_fails_startup_when_invalidation_poll_interval_is_zero() {
    let dsn = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let probe = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        sqlx::PgPool::connect(&dsn),
    )
    .await;
    if !matches!(probe, Ok(Ok(_))) {
        eprintln!("SKIP: postgres unreachable at {dsn} — run()-level invalidation env test skipped");
        return;
    }

    // This test mutates the REAL var name `run()` reads, so it must serialize.
    let _guard = RUN_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _restore = EnvRestore::set("INVALIDATION_POLL_INTERVAL_MS", "0");

    let mut cfg =
        Config::from_values(Some(dsn), None, None, None, None, None, None, None, None, None);
    cfg.listen_addr = format!("127.0.0.1:{}", free_port());

    tokio::select! {
        res = run(cfg, vec![], None, None) => {
            let err = res
                .expect_err("run() must fail startup when the invalidation poll interval is 0");
            let msg = format!("{err:#}");
            assert!(
                msg.contains("INVALIDATION_POLL_INTERVAL_MS"),
                "startup error should name the var: {msg}"
            );
        }
        _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
            panic!("run() did not fail startup — the invalidation env error was swallowed");
        }
    }
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
/// process signal, so it is DRIVEN via `select!` on this test's own task — never
/// `tokio::spawn`ed, which would demand a generally-`Send` proof of `run`'s giant
/// generator that rustc cannot make and production never needs (every `cmd/*`
/// main just `.await`s it). The `select!` arm dropping `run`'s future when the
/// probe body finishes replaces the old task-`abort()`.)
#[tokio::test(flavor = "multi_thread")]
async fn run_with_tls_files_serves_https() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (cert, key, cert_pem) = mint_localhost_cert("run");
    let port = free_port();

    let mut cfg =
        Config::from_values(None, None, None, None, None, None, None, None, None, None).without_db();
    cfg.listen_addr = format!("127.0.0.1:{port}");
    let cfg = cfg.with_tls(Some(TlsFront::Files { cert, key }));

    tokio::select! {
        res = run(cfg, Vec::new(), None, None) => panic!("server exited early: {res:?}"),
        _ = async {
            let client = tls_client(&cert_pem);
            let resp = get_when_up(&client, &format!("https://127.0.0.1:{port}/healthz")).await;
            assert_eq!(resp.status(), reqwest::StatusCode::OK);
            assert_eq!(resp.text().await.unwrap(), "ok");
        } => {}
    }
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

    // Driven via `select!` on this task, not `tokio::spawn` — see
    // `run_with_tls_files_serves_https` for why (no `Send` proof of `run`'s
    // generator required or wanted; select-drop replaces the old `abort()`).
    tokio::select! {
        res = run(cfg, vec![Box::new(SlowRoutes { slow_ms: 5000 })], None, None) => {
            panic!("server exited early: {res:?}")
        }
        _ = async {
            let client = reqwest::Client::new();

            // A fast request is unaffected by the layer.
            let fast = get_when_up(&client, &format!("http://127.0.0.1:{port}/fast")).await;
            assert_eq!(fast.status(), reqwest::StatusCode::OK);
            assert_eq!(fast.text().await.unwrap(), "fast");

            // The slow handler exceeds the 150ms budget → 408 Request Timeout
            // (deliberately 408, not 504 — see the layer-site comment in `run`).
            let slow = client
                .get(format!("http://127.0.0.1:{port}/slow"))
                .send()
                .await
                .expect("slow request");
            assert_eq!(slow.status(), reqwest::StatusCode::REQUEST_TIMEOUT);
        } => {}
    }
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

    // Driven via `select!` on this task, not `tokio::spawn` — see
    // `run_with_tls_files_serves_https` for why.
    tokio::select! {
        res = run(cfg, vec![Box::new(SlowRoutes { slow_ms: 300 })], None, None) => {
            panic!("server exited early: {res:?}")
        }
        _ = async {
            let client = reqwest::Client::new();

            // With no layer, the 300ms handler completes instead of being timed out.
            let slow = get_when_up(&client, &format!("http://127.0.0.1:{port}/slow")).await;
            assert_eq!(slow.status(), reqwest::StatusCode::OK);
            assert_eq!(slow.text().await.unwrap(), "slow");
        } => {}
    }
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
// Holds `RUN_ENV_LOCK` across the `run()` await: this DB-backed boot READS the real
// `INVALIDATION_*` env vars mid-flight, so it must not overlap the test that mutates
// them (`run_fails_startup_when_invalidation_poll_interval_is_zero`). Safe for the
// same reason as there: `#[tokio::test]` drives this future via `block_on` on one
// thread, so the std MutexGuard never crosses threads.
#[allow(clippy::await_holding_lock)]
async fn durable_delivery_starts_only_after_invalidation_first_refresh() {
    use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
    use std::sync::Arc;

    let _env_guard = RUN_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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
    // Driven via `select!` on this task, not `tokio::spawn` — see
    // `run_with_tls_files_serves_https` for why. The polling arm completing
    // drops `run`'s future (the old `abort()`), and the assertions + DB
    // cleanup below run after the server is gone, exactly as before.
    let delivered = tokio::select! {
        res = run(cfg, vec![Box::new(probe)], None, None) => {
            panic!("server exited early: {res:?}")
        }
        delivered = async {
            // First refresh sleeps 2s, delivery polls at a 1s floor and frontier
            // eligibility can lag behind unrelated transactions — poll generously.
            for _ in 0..300 {
                if at_delivery.load(Ordering::SeqCst) >= 0 {
                    return true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            false
        } => delivered,
    };
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
