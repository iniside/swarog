use super::*;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// `CARGO_TARGET_DIR`/`DATABASE_URL` are process-global env vars — tests that
/// touch them must not interleave with each other (or with any other test in
/// this binary that reads them), matching the mutex pattern in
/// `tests/platform.rs::sequential`.
fn env_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// RAII restore of a single env var across a test that mutates it.
struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        EnvVarGuard { key, previous }
    }

    fn unset(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        std::env::remove_var(key);
        EnvVarGuard { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

fn temp_dir(name: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "weles-prep-{}-{}-{name}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("create test temp dir");
    dir
}

#[test]
fn discover_defaults_target_debug_under_root_target() {
    let _guard = env_guard();
    let _unset = EnvVarGuard::unset("CARGO_TARGET_DIR");

    let root = temp_dir("discover-default");
    let layout = Layout::discover(root.clone()).expect("discover layout");

    assert_eq!(layout.root, root);
    assert_eq!(layout.run_dir, root.join("run").join("weles"));
    assert!(layout.run_dir.is_dir(), "run_dir must be created");
    assert_eq!(layout.target_debug, root.join("target").join("debug"));
}

#[test]
fn discover_honors_absolute_cargo_target_dir() {
    let _guard = env_guard();
    let target_root = temp_dir("discover-abs-target");
    let _set = EnvVarGuard::set("CARGO_TARGET_DIR", target_root.to_str().unwrap());

    let root = temp_dir("discover-abs-root");
    let layout = Layout::discover(root).expect("discover layout");

    assert_eq!(layout.target_debug, target_root.join("debug"));
}

#[test]
fn discover_honors_relative_cargo_target_dir() {
    let _guard = env_guard();
    let _set = EnvVarGuard::set("CARGO_TARGET_DIR", "my-target");

    let root = temp_dir("discover-rel-root");
    let layout = Layout::discover(root.clone()).expect("discover layout");

    assert_eq!(layout.target_debug, root.join("my-target").join("debug"));
}

#[test]
fn binary_appends_platform_exe_suffix() {
    let layout = Layout {
        root: PathBuf::from("/root"),
        run_dir: PathBuf::from("/root/run/weles"),
        target_debug: PathBuf::from("/root/target/debug"),
    };
    let expected = layout
        .target_debug
        .join(format!("edgeca{}", std::env::consts::EXE_SUFFIX));
    assert_eq!(layout.binary("edgeca"), expected);
}

#[test]
fn database_url_defaults_to_dev_dsn() {
    let _guard = env_guard();
    let _unset = EnvVarGuard::unset("DATABASE_URL");

    assert_eq!(
        database_url(),
        "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable"
    );
}

#[test]
fn database_url_honors_env_override() {
    let _guard = env_guard();
    let _set = EnvVarGuard::set("DATABASE_URL", "postgres://custom/db");

    assert_eq!(database_url(), "postgres://custom/db");
}

#[test]
fn resolve_on_path_finds_cargo() {
    // cargo is guaranteed present on PATH in this dev/CI environment (this
    // very test binary was built by it).
    let resolved = resolve_on_path("cargo").expect("cargo must resolve on PATH");
    assert!(resolved.is_file(), "resolved cargo path must exist: {}", resolved.display());
}

#[test]
fn resolve_on_path_errors_on_nonsense_name() {
    let result = resolve_on_path("definitely-not-a-real-executable-name-xyz123");
    assert!(result.is_err(), "a bogus executable name must not resolve");
}

#[cfg(windows)]
#[test]
fn filtered_env_dedupes_case_variant_allowlist_entries_on_windows() {
    let _guard = env_guard();
    // Windows env is case-insensitive-preserving: HTTP_PROXY and http_proxy
    // name the SAME variable, and both allowlist spellings would resolve to
    // it — only one spelling may survive into the child env block.
    let _set = EnvVarGuard::set("HTTP_PROXY", "http://proxy.example:3128");
    let env = filtered_env(&["HTTP_PROXY", "http_proxy"]);
    assert_eq!(
        env.len(),
        1,
        "case-variant allowlist entries must dedupe to one env pair, got: {env:?}"
    );
}

#[cfg(not(windows))]
#[test]
fn filtered_env_keeps_distinct_case_variants_on_unix() {
    let _guard = env_guard();
    // Exact-case lookup: the two spellings are genuinely different variables
    // and BOTH must pass through (deduping here would drop a real var).
    let _upper = EnvVarGuard::set("HTTP_PROXY", "http://upper.example:3128");
    std::env::set_var("http_proxy", "http://lower.example:3128");
    let env = filtered_env(&["HTTP_PROXY", "http_proxy"]);
    std::env::remove_var("http_proxy");
    assert_eq!(env.len(), 2, "unix case variants are distinct vars, got: {env:?}");
}

#[test]
fn mint_ca_skips_spawn_when_both_files_already_exist() {
    let root = temp_dir("mint-ca-skip-root");
    let run_dir = root.join("run").join("weles");
    std::fs::create_dir_all(&run_dir).expect("create run_dir");
    let cert = run_dir.join("edge-ca.crt");
    let key = run_dir.join("edge-ca.key");
    std::fs::write(&cert, b"fake cert").expect("write fake cert");
    std::fs::write(&key, b"fake key").expect("write fake key");

    let layout = Layout {
        root: root.clone(),
        run_dir: run_dir.clone(),
        // Deliberately points at a nonexistent binary: if mint_ca attempted
        // to spawn despite the skip-if-exists branch, the spawn would fail
        // loudly (and this test would catch it as an Err), so the ONLY way
        // this passes is via the early skip.
        target_debug: root.join("no-such-target").join("debug"),
    };

    let paths = mint_ca(&layout).expect("mint_ca should skip and succeed");
    assert_eq!(paths.cert, cert);
    assert_eq!(paths.key, key);

    // No spawn attempted ⇒ no log files written.
    assert!(!run_dir.join("edgeca.out.log").exists());
    assert!(!run_dir.join("edgeca.err.log").exists());
}
