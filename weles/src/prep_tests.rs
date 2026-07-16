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
    // Stage a generation so discover can pin it (discover requires deploy/current).
    std::fs::create_dir_all(root.join("deploy").join("gen-1")).expect("gen-1");
    std::fs::write(root.join("deploy").join("current"), "gen-1").expect("current");
    let layout = Layout::discover(root.clone()).expect("discover layout");

    assert_eq!(layout.root, root);
    assert_eq!(layout.run_dir, root.join("run").join("weles"));
    assert!(layout.run_dir.is_dir(), "run_dir must be created");
    assert_eq!(layout.bin_dir, root.join("deploy"));
    assert_eq!(
        layout.active_bin_dir,
        root.join("deploy").join("gen-1"),
        "discover pins the generation named by deploy/current"
    );
}

#[test]
fn binary_resolves_against_the_pinned_generation_dir() {
    // binary() resolves against active_bin_dir (the pinned generation), NOT the
    // deploy root — so the fleet spawns one coherent generation.
    let layout = Layout {
        root: PathBuf::from("/root"),
        run_dir: PathBuf::from("/root/run/weles"),
        bin_dir: PathBuf::from("/root/deploy"),
        active_bin_dir: PathBuf::from("/root/deploy/gen-2"),
    };
    let expected = layout
        .active_bin_dir
        .join(format!("edgeca{}", std::env::consts::EXE_SUFFIX));
    assert_eq!(layout.binary("edgeca"), expected);
    assert_ne!(
        layout.binary("edgeca"),
        layout.bin_dir.join(format!("edgeca{}", std::env::consts::EXE_SUFFIX)),
        "must NOT resolve against the deploy root"
    );
}

/// Builds a `Layout` whose `bin_dir` is a real temp dir (created), with
/// `active_bin_dir` pinned to the deploy root, so validate tests can stage fake
/// exes where `binary()` resolves them.
fn temp_layout(tag: &str) -> Layout {
    let root = temp_dir(tag);
    let run_dir = root.join("run").join("weles");
    std::fs::create_dir_all(&run_dir).expect("create run_dir");
    let bin_dir = root.join("deploy");
    std::fs::create_dir_all(&bin_dir).expect("create bin_dir");
    Layout {
        root,
        run_dir,
        active_bin_dir: bin_dir.clone(),
        bin_dir,
    }
}

/// A deploy-path layout for `root` (no pinned generation required — deploy
/// stages a fresh one). `active_bin_dir` is the inert placeholder `deploy/`.
fn deploy_layout(root: &Path) -> Layout {
    Layout::discover_for_deploy(root.to_path_buf()).expect("discover_for_deploy")
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

/// Stages every deployable package in `src` with the given bytes so a deploy
/// succeeds fully.
fn stage_full_source(src: &Path, bytes: &[u8]) {
    for pkg in deploy_packages() {
        let file = format!("{pkg}{}", std::env::consts::EXE_SUFFIX);
        std::fs::write(src.join(&file), bytes).expect("write source binary");
    }
}

#[test]
fn deploy_stages_gen_1_and_flips_current() {
    let root = temp_dir("deploy-gen1");
    let layout = deploy_layout(&root);
    let src = temp_dir("deploy-gen1-src");
    stage_full_source(&src, b"v1 source");

    deploy(&layout, &src).expect("first deploy stages gen-1");

    // current names gen-1, and the pinned Layout resolves gen-1 binaries.
    assert_eq!(
        std::fs::read_to_string(root.join("deploy").join("current"))
            .expect("read current")
            .trim(),
        "gen-1"
    );
    let up = Layout::discover(root.clone()).expect("discover pins gen-1");
    assert_eq!(up.active_bin_dir, root.join("deploy").join("gen-1"));
    for pkg in deploy_packages() {
        let dst = up.binary(pkg);
        assert!(dst.is_file(), "{} must be staged in gen-1", dst.display());
        assert_eq!(std::fs::read(&dst).expect("read staged"), b"v1 source");
    }
    // A manifest.json exists recording the generation.
    assert!(root.join("deploy").join("gen-1").join("manifest.json").is_file());
}

#[test]
fn second_deploy_creates_gen_2_and_repoints_current() {
    let root = temp_dir("deploy-gen2");
    let layout = deploy_layout(&root);
    let src = temp_dir("deploy-gen2-src");
    stage_full_source(&src, b"payload");

    deploy(&layout, &src).expect("gen-1");
    deploy(&layout, &src).expect("gen-2");

    assert!(root.join("deploy").join("gen-1").is_dir());
    assert!(root.join("deploy").join("gen-2").is_dir());
    assert_eq!(
        std::fs::read_to_string(root.join("deploy").join("current"))
            .expect("read current")
            .trim(),
        "gen-2"
    );
}

#[test]
fn three_deploys_retain_only_the_two_newest_generations() {
    let root = temp_dir("deploy-retain");
    let layout = deploy_layout(&root);
    let src = temp_dir("deploy-retain-src");
    stage_full_source(&src, b"payload");

    deploy(&layout, &src).expect("gen-1");
    deploy(&layout, &src).expect("gen-2");
    deploy(&layout, &src).expect("gen-3");

    let deploy_dir = root.join("deploy");
    assert!(!deploy_dir.join("gen-1").exists(), "gen-1 must be pruned");
    assert!(deploy_dir.join("gen-2").is_dir(), "gen-2 (previous) is kept");
    assert!(deploy_dir.join("gen-3").is_dir(), "gen-3 (current) is kept");
    assert_eq!(
        std::fs::read_to_string(deploy_dir.join("current"))
            .expect("read current")
            .trim(),
        "gen-3"
    );
}

#[test]
fn abandoned_partial_is_pruned_while_the_previous_good_generation_survives() {
    // Scenario B: an abandoned partial bumps the counter, so a position-based
    // "keep current-1" would keep the useless abandoned gen and delete the real
    // previous good one. Keying retention off the PRE-FLIP current closes it.
    let root = temp_dir("deploy-abandoned");
    let layout = deploy_layout(&root);
    let good = temp_dir("deploy-abandoned-good");
    stage_full_source(&good, b"good");
    deploy(&layout, &good).expect("gen-1 good");

    // Partial-fail gen-2 (omit one pkg): bails before flip, current stays gen-1,
    // gen-2 dir is abandoned (no manifest).
    let broken = temp_dir("deploy-abandoned-broken");
    for pkg in deploy_packages() {
        if pkg == "adminctl" {
            continue;
        }
        let file = format!("{pkg}{}", std::env::consts::EXE_SUFFIX);
        std::fs::write(broken.join(&file), b"broken").expect("write source binary");
    }
    deploy(&layout, &broken).expect_err("gen-2 partial fails");

    // gen-3 good: retention protects pre-flip current (gen-1), NOT gen-3-1=gen-2.
    deploy(&layout, &good).expect("gen-3 good");

    let deploy_dir = root.join("deploy");
    assert!(deploy_dir.join("gen-1").is_dir(), "previous good gen-1 must survive");
    assert!(!deploy_dir.join("gen-2").exists(), "abandoned partial gen-2 must be pruned");
    assert!(deploy_dir.join("gen-3").is_dir(), "current gen-3 is kept");
    // gen-1 is the good previous generation with a real manifest.
    assert!(deploy_dir.join("gen-1").join("manifest.json").is_file());
}

#[test]
fn deploy_rejects_the_deploy_dir_as_its_own_source() {
    // `weles deploy deploy` (src == deploy root) is refused before any file is
    // touched, on both platforms.
    let root = temp_dir("deploy-self");
    let layout = deploy_layout(&root);
    std::fs::create_dir_all(&layout.bin_dir).expect("create deploy dir");

    let error = deploy(&layout, &layout.bin_dir).expect_err("self-deploy must be rejected");
    let message = format!("{error:#}");
    assert!(
        message.contains("IS the deploy dir"),
        "must name the self-copy condition: {message}"
    );
}

#[test]
fn partial_fail_missing_source_abandons_gen_and_keeps_current() {
    // The at-risk branch (was "no rollback"): a second deploy with a missing
    // source must NOT flip `current` — it still names the good previous
    // generation, and a fresh discover resolves the OLD gen.
    let root = temp_dir("deploy-partial");
    let layout = deploy_layout(&root);
    let good = temp_dir("deploy-partial-good");
    stage_full_source(&good, b"good v1");
    deploy(&layout, &good).expect("gen-1 succeeds");

    // Second source omits one package ⇒ deploy bails.
    let broken = temp_dir("deploy-partial-broken");
    for pkg in deploy_packages() {
        if pkg == "adminctl" {
            continue;
        }
        let file = format!("{pkg}{}", std::env::consts::EXE_SUFFIX);
        std::fs::write(broken.join(&file), b"broken v2").expect("write source binary");
    }
    let error = deploy(&layout, &broken).expect_err("missing source ⇒ Err");
    let message = format!("{error:#}");
    assert!(
        message.contains("adminctl"),
        "must enumerate the missing source: {message}"
    );
    assert!(
        message.contains("current` unchanged"),
        "must state current was not flipped: {message}"
    );

    // current still names gen-1; a fresh discover resolves gen-1's bytes.
    assert_eq!(
        std::fs::read_to_string(root.join("deploy").join("current"))
            .expect("read current")
            .trim(),
        "gen-1",
        "a partial deploy must not repoint current"
    );
    let up = Layout::discover(root.clone()).expect("discover pins the OLD gen");
    assert_eq!(up.active_bin_dir, root.join("deploy").join("gen-1"));
    assert_eq!(
        std::fs::read(up.binary("edgeca")).expect("read pinned"),
        b"good v1",
        "the live fleet's binary source is the untouched previous generation"
    );
}

#[test]
fn discover_without_current_reports_nothing_deployed() {
    let root = temp_dir("deploy-fresh");
    std::fs::create_dir_all(root.join("deploy")).expect("create empty deploy dir");
    let error = Layout::discover(root).expect_err("no current ⇒ Err");
    let message = format!("{error:#}");
    assert!(
        message.contains("nothing deployed") && message.contains("weles deploy"),
        "must be the clear operator-facing error: {message}"
    );
}

#[test]
fn parse_and_next_generation_ignore_non_gen_entries() {
    assert_eq!(parse_generation(std::ffi::OsStr::new("gen-7")), Some(7));
    assert_eq!(parse_generation(std::ffi::OsStr::new("current")), None);
    assert_eq!(parse_generation(std::ffi::OsStr::new("current.tmp")), None);
    assert_eq!(parse_generation(std::ffi::OsStr::new("gen-")), None);

    let root = temp_dir("nextgen");
    let deploy = root.join("deploy");
    std::fs::create_dir_all(deploy.join("gen-1")).expect("gen-1");
    std::fs::create_dir_all(deploy.join("gen-4")).expect("gen-4");
    std::fs::write(deploy.join("current"), "gen-4").expect("current");
    assert_eq!(next_generation(&deploy).expect("next"), 5);
}

#[test]
fn generations_to_prune_keeps_exactly_the_protected_set() {
    // Everything not protected is stale; membership (by number), not position.
    assert_eq!(generations_to_prune(&[1, 2, 3], &[2, 3]), vec![1]);
    // A live pin on gen-1 far behind current protects it even though 2 is also
    // protected and 1 < current-1 — the position rule would have deleted it.
    assert_eq!(generations_to_prune(&[1, 2, 3, 4], &[1, 3, 4]), vec![2]);
    // Nothing protected ⇒ everything prunes; everything protected ⇒ nothing.
    assert_eq!(generations_to_prune(&[1, 2], &[]), vec![1, 2]);
    assert!(generations_to_prune(&[1, 2], &[1, 2]).is_empty());
}

#[test]
fn copy_and_hash_errors_when_source_is_a_directory() {
    // A directory source fails the copy on both platforms (Windows: open fails;
    // Unix: the read fails with EISDIR) — the copy-failure path returns Err.
    let root = temp_dir("copyhash-err");
    let src = root.join("a-directory");
    std::fs::create_dir_all(&src).expect("create dir source");
    let dst = root.join("dst.bin");
    assert!(copy_and_hash(&src, &dst).is_err(), "dir source must fail the copy");
}

#[test]
fn prune_tolerates_an_undeletable_generation_and_removes_the_rest() {
    // Delete must be TOLERANT (close "overwrite live exe" without opening
    // "delete live exe"). A `gen-1` that is a FILE (not a dir) makes
    // remove_dir_all fail on both platforms — the prune must log-and-skip it
    // and still remove the other stale generation, never erroring.
    let root = temp_dir("prune-tolerant");
    let deploy = root.join("deploy");
    std::fs::create_dir_all(&deploy).expect("deploy");
    // gen-1 is an (undeletable-by-remove_dir_all) FILE; gen-2 a real dir.
    std::fs::write(deploy.join("gen-1"), b"squat").expect("gen-1 file");
    std::fs::create_dir_all(deploy.join("gen-2")).expect("gen-2 dir");
    std::fs::create_dir_all(deploy.join("gen-4")).expect("gen-4 dir (current)");

    // Protect gen-4 only ⇒ gen-1 and gen-2 are both stale; gen-1 is undeletable
    // (a file), gen-2 removes cleanly.
    let removed = prune_stale_generations(&deploy, &[4]);

    assert!(deploy.join("gen-1").exists(), "undeletable gen-1 is skipped, not fatal");
    assert!(!deploy.join("gen-2").exists(), "gen-2 must be pruned");
    assert!(deploy.join("gen-4").exists(), "protected generation is kept");
    assert_eq!(removed, vec![deploy.join("gen-2")], "only gen-2 was removed");
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
        active_bin_dir: root.join("no-such-deploy"),
    };

    let paths = mint_ca(&layout).expect("mint_ca should skip and succeed");
    assert_eq!(paths.cert, cert);
    assert_eq!(paths.key, key);

    // No spawn attempted ⇒ no log files written.
    assert!(!run_dir.join("edgeca.out.log").exists());
    assert!(!run_dir.join("edgeca.err.log").exists());
}
