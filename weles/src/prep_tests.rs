use super::*;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// `DATABASE_URL`/proxy vars are process-global env vars — tests that touch
/// them must not interleave with each other (or with any other test in this
/// binary that reads them), matching the mutex pattern in
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
fn discover_fixes_bin_dir_at_root_deploy() {
    // No env var influences bin_dir: it is ALWAYS <root>/deploy (config-as-code,
    // no CARGO_TARGET_DIR, no debug/release heuristic). Set CARGO_TARGET_DIR to
    // prove it is ignored.
    let _guard = env_guard();
    let _set = EnvVarGuard::set("CARGO_TARGET_DIR", "should-be-ignored");

    let root = temp_dir("discover-default");
    let layout = Layout::discover(root.clone()).expect("discover layout");

    assert_eq!(layout.root, root);
    assert_eq!(layout.run_dir, root.join("run").join("weles"));
    assert!(layout.run_dir.is_dir(), "run_dir must be created");
    assert_eq!(layout.bin_dir, root.join("deploy"));
}

#[test]
fn binary_appends_platform_exe_suffix_under_deploy() {
    let layout = Layout {
        root: PathBuf::from("/root"),
        run_dir: PathBuf::from("/root/run/weles"),
        bin_dir: PathBuf::from("/root/deploy"),
    };
    let expected = layout
        .bin_dir
        .join(format!("edgeca{}", std::env::consts::EXE_SUFFIX));
    assert_eq!(layout.binary("edgeca"), expected);
}

/// Builds a `Layout` whose `bin_dir` is a real temp dir (created), so
/// validate/deploy tests can stage fake exes in it.
fn temp_layout(tag: &str) -> Layout {
    let root = temp_dir(tag);
    let run_dir = root.join("run").join("weles");
    std::fs::create_dir_all(&run_dir).expect("create run_dir");
    let bin_dir = root.join("deploy");
    std::fs::create_dir_all(&bin_dir).expect("create bin_dir");
    Layout { root, run_dir, bin_dir }
}

fn stage_fake(layout: &Layout, pkg: &str) {
    std::fs::write(layout.binary(pkg), b"fake exe").expect("stage fake binary");
}

#[test]
fn validate_binaries_ok_when_all_present() {
    let layout = temp_layout("validate-ok");
    let packages = ["edgeca", "adminctl", "server"];
    for pkg in packages {
        stage_fake(&layout, pkg);
    }
    validate_binaries(&layout, &packages).expect("all staged ⇒ Ok");
}

#[test]
fn validate_binaries_lists_every_missing_binary() {
    let layout = temp_layout("validate-missing");
    // Stage only one of three; the other two must BOTH be listed.
    stage_fake(&layout, "edgeca");
    let packages = ["edgeca", "adminctl", "server"];

    let error = validate_binaries(&layout, &packages).expect_err("missing ⇒ Err");
    let message = format!("{error:#}");
    assert!(
        message.contains(&layout.binary("adminctl").display().to_string()),
        "must list the missing adminctl path: {message}"
    );
    assert!(
        message.contains(&layout.binary("server").display().to_string()),
        "must list the missing server path: {message}"
    );
    assert!(
        !message.contains(&layout.binary("edgeca").display().to_string()),
        "the present binary must NOT be listed: {message}"
    );
    assert!(
        message.contains("weles deploy"),
        "must hint at `weles deploy`: {message}"
    );
}

#[test]
fn deploy_copies_every_package_and_overwrites() {
    let layout = temp_layout("deploy-copy");
    let src = temp_dir("deploy-src");
    // Stage EVERY deployable package in the source so deploy succeeds fully.
    for pkg in deploy_packages() {
        let file = format!("{pkg}{}", std::env::consts::EXE_SUFFIX);
        std::fs::write(src.join(&file), b"v1 source").expect("write source binary");
    }
    // Pre-existing stale artifact in deploy/ to prove overwrite-is-redeploy.
    std::fs::write(layout.binary("edgeca"), b"stale v0").expect("write stale");

    deploy(&layout, &src).expect("deploy should copy every package");

    for pkg in deploy_packages() {
        let dst = layout.binary(pkg);
        assert!(dst.is_file(), "{} must be staged", dst.display());
        assert_eq!(
            std::fs::read(&dst).expect("read staged"),
            b"v1 source",
            "{} must be overwritten with the source bytes",
            dst.display()
        );
    }
}

#[test]
fn deploy_rejects_the_deploy_dir_as_its_own_source() {
    // `weles deploy deploy` (src == bin_dir): on Unix fs::copy truncates the
    // destination before reading the SAME inode, zeroing every staged binary
    // while reporting success. The canonicalize guard must reject this before
    // any file is touched, on both platforms.
    let layout = temp_layout("deploy-self");
    stage_fake(&layout, "edgeca");
    let original = std::fs::read(layout.binary("edgeca")).expect("read staged");

    let error = deploy(&layout, &layout.bin_dir).expect_err("self-deploy must be rejected");
    let message = format!("{error:#}");
    assert!(
        message.contains("IS the deploy dir"),
        "must name the self-copy condition: {message}"
    );
    assert_eq!(
        std::fs::read(layout.binary("edgeca")).expect("read staged after"),
        original,
        "staged bytes must be untouched by a rejected self-deploy"
    );
}

#[test]
fn deploy_enumerates_every_failed_copy_and_stages_the_rest() {
    // Cross-platform copy-failure injection: a DIRECTORY squatting on a
    // destination file path makes fs::copy fail on every platform. The loop
    // must continue past the failure and the final error must enumerate it
    // while the other files got staged.
    let layout = temp_layout("deploy-copyfail");
    let src = temp_dir("deploy-copyfail-src");
    for pkg in deploy_packages() {
        let file = format!("{pkg}{}", std::env::consts::EXE_SUFFIX);
        std::fs::write(src.join(&file), b"payload").expect("write source binary");
    }
    let blocked = layout.binary("edgeca");
    std::fs::create_dir_all(&blocked).expect("squat a directory on the destination path");

    let error = deploy(&layout, &src).expect_err("a failed copy must fail the deploy");
    let message = format!("{error:#}");
    assert!(
        message.contains(&blocked.display().to_string()),
        "must enumerate the failed destination: {message}"
    );
    assert!(
        message.contains("copy failed"),
        "must label the failure class: {message}"
    );
    // Every OTHER package must still have been staged despite the mid-loop failure.
    for pkg in deploy_packages() {
        if pkg == "edgeca" {
            continue;
        }
        let dst = layout.binary(pkg);
        assert!(dst.is_file(), "{} must be staged past the failure", dst.display());
        assert_eq!(std::fs::read(&dst).expect("read staged"), b"payload");
    }
}

#[test]
fn deploy_lists_every_missing_source_and_keeps_copied_files() {
    let layout = temp_layout("deploy-missing");
    let src = temp_dir("deploy-src-missing");
    // Stage all but two packages; both omissions must be listed, and the
    // packages that WERE present must remain staged (no rollback).
    let all = deploy_packages();
    let omit = ["edgeca", "adminctl"];
    for pkg in &all {
        if omit.contains(pkg) {
            continue;
        }
        let file = format!("{pkg}{}", std::env::consts::EXE_SUFFIX);
        std::fs::write(src.join(&file), b"present").expect("write source binary");
    }

    let error = deploy(&layout, &src).expect_err("missing sources ⇒ Err");
    let message = format!("{error:#}");
    for pkg in omit {
        let missing_src = src.join(format!("{pkg}{}", std::env::consts::EXE_SUFFIX));
        assert!(
            message.contains(&missing_src.display().to_string()),
            "must list missing source {}: {message}",
            missing_src.display()
        );
    }
    // A package that WAS present must remain staged despite the overall error.
    let present = all.iter().find(|p| !omit.contains(p)).expect("some present");
    assert!(
        layout.binary(present).is_file(),
        "already-copied {present} must remain staged after the error"
    );
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
        // Deliberately points at a nonexistent deploy dir: if mint_ca attempted
        // to spawn despite the skip-if-exists branch, the spawn would fail
        // loudly (and this test would catch it as an Err), so the ONLY way
        // this passes is via the early skip.
        bin_dir: root.join("no-such-deploy"),
    };

    let paths = mint_ca(&layout).expect("mint_ca should skip and succeed");
    assert_eq!(paths.cert, cert);
    assert_eq!(paths.key, key);

    // No spawn attempted ⇒ no log files written.
    assert!(!run_dir.join("edgeca.out.log").exists());
    assert!(!run_dir.join("edgeca.err.log").exists());
}
