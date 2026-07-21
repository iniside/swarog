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

    /// Removes `key` for the test's duration, restoring the prior value on drop.
    /// Used to prove the cwd-walk branch of `resolve_root` fires only when
    /// `WELES_ROOT` is absent.
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

/// RAII restore of the process-global current directory across a test that
/// changes it. Like `WELES_ROOT`, cwd is process-global, so a test that sets it
/// must hold [`env_guard`] and restore on drop.
struct CwdGuard {
    previous: PathBuf,
}

impl CwdGuard {
    fn set(dir: &Path) -> Self {
        let previous = std::env::current_dir().expect("read current dir");
        std::env::set_current_dir(dir).expect("set current dir");
        CwdGuard { previous }
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.previous);
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
    // Stage a generation so discover can pin it (discover requires deploy/current
    // AND a valid deployed fleet.toml, which it reads+validates once at pin time).
    std::fs::create_dir_all(root.join("deploy").join("gen-1")).expect("gen-1");
    std::fs::write(root.join("deploy").join("current"), "gen-1").expect("current");
    std::fs::write(
        root.join("deploy").join("gen-1").join("fleet.toml"),
        "[[service]]\nname = \"server\"\npkg = \"server\"\nhttp_port = 8080\n",
    )
    .expect("stage a minimal fleet.toml");
    // discover now verifies the pinned generation against its manifest, so a
    // manually-staged generation needs a matching manifest.json.
    write_manifest_for(&root.join("deploy").join("gen-1"));
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
    let layout = Layout::for_test(
        PathBuf::from("/root"),
        PathBuf::from("/root/run/weles"),
        PathBuf::from("/root/deploy"),
        PathBuf::from("/root/deploy/gen-2"),
    );
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
    Layout::for_test(root, run_dir, bin_dir.clone(), bin_dir)
}

/// A deploy-path layout for `root` (no pinned generation required — deploy
/// stages a fresh one). `active_bin_dir` is the inert placeholder `deploy/`.
fn deploy_layout(root: &Path) -> Layout {
    Layout::discover_for_deploy(root.to_path_buf()).expect("discover_for_deploy")
}

fn stage_fake(layout: &Layout, pkg: &str) {
    std::fs::write(layout.binary(pkg), b"fake exe").expect("stage fake binary");
}

/// The committed split fixture, loaded (its `[[service]]` pkgs ∪ `[[prepare]]`
/// runs are the deploy set). These deploy mechanics tests exercise
/// staging/retention, so the concrete fleet only has to be a real, validating
/// one — the shipped split fixture is the obvious choice.
fn split_fleet() -> crate::fleet_toml::Fleet {
    crate::test_fixtures::load_split_fixture()
}

fn split_fleet_path() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fleet.split.toml")
}

/// `deploy` with the fleet arg fixed to the committed split fixture (the file
/// `deploy` stamps into the generation and `up` reads back).
fn deploy_fx(layout: &Layout, src: &Path) -> Result<()> {
    deploy(layout, src, &split_fleet_path())
}

#[test]
fn validate_binaries_ok_when_all_present() {
    let layout = temp_layout("validate-ok");
    let packages = ["edgeca", "adminctl", "server"].map(String::from);
    for pkg in &packages {
        stage_fake(&layout, pkg);
    }
    validate_binaries(&layout, &packages).expect("all staged ⇒ Ok");
}

#[test]
fn validate_binaries_lists_every_missing_binary() {
    let layout = temp_layout("validate-missing");
    // Stage only one of three; the other two must BOTH be listed.
    stage_fake(&layout, "edgeca");
    let packages = ["edgeca", "adminctl", "server"].map(String::from);

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
    for pkg in deploy_packages(&split_fleet()) {
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

    deploy_fx(&layout, &src).expect("first deploy stages gen-1");

    // current names gen-1, and the pinned Layout resolves gen-1 binaries.
    assert_eq!(
        std::fs::read_to_string(root.join("deploy").join("current"))
            .expect("read current")
            .trim(),
        "gen-1"
    );
    let up = Layout::discover(root.clone()).expect("discover pins gen-1");
    assert_eq!(up.active_bin_dir, root.join("deploy").join("gen-1"));
    for pkg in deploy_packages(&split_fleet()) {
        let dst = up.binary(&pkg);
        assert!(dst.is_file(), "{} must be staged in gen-1", dst.display());
        assert_eq!(std::fs::read(&dst).expect("read staged"), b"v1 source");
    }
    // A manifest.json exists recording the generation.
    assert!(root.join("deploy").join("gen-1").join("manifest.json").is_file());
}

#[test]
fn deploy_records_history_in_the_master_store() {
    let root = temp_dir("deploy-history");
    let layout = deploy_layout(&root);
    let src = temp_dir("deploy-history-src");
    stage_full_source(&src, b"v1 source");

    deploy_fx(&layout, &src).expect("first deploy stages gen-1");

    // The flip is recorded in the durable store, readable back by generation.
    let store = crate::store::Store::open(&layout.run_dir.join("state.db"))
        .expect("open master store");
    let record = store
        .deploy_record("gen-1")
        .expect("read deploy history")
        .expect("gen-1 was recorded on the successful flip");
    assert_eq!(record.generation, "gen-1");
    assert_eq!(
        record.sha_root.len(),
        64,
        "sha_root must be a hex SHA-256: {}",
        record.sha_root
    );
    assert!(
        record.sha_root.chars().all(|c| c.is_ascii_hexdigit()),
        "sha_root must be hex: {}",
        record.sha_root
    );
    assert!(record.deployed_unix > 0, "deployed_unix must be a real timestamp");

    // A second deploy of the SAME source bytes + fleet records gen-2 with the
    // SAME sha_root (the root hash reflects staged content, deterministically).
    deploy_fx(&layout, &src).expect("second deploy stages gen-2");
    let gen2 = store
        .deploy_record("gen-2")
        .expect("read gen-2 history")
        .expect("gen-2 recorded");
    assert_eq!(gen2.generation, "gen-2");
    assert_eq!(
        gen2.sha_root, record.sha_root,
        "identical staged bytes ⇒ identical root sha"
    );
}

#[test]
fn second_deploy_creates_gen_2_and_repoints_current() {
    let root = temp_dir("deploy-gen2");
    let layout = deploy_layout(&root);
    let src = temp_dir("deploy-gen2-src");
    stage_full_source(&src, b"payload");

    deploy_fx(&layout, &src).expect("gen-1");
    deploy_fx(&layout, &src).expect("gen-2");

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

    deploy_fx(&layout, &src).expect("gen-1");
    deploy_fx(&layout, &src).expect("gen-2");
    deploy_fx(&layout, &src).expect("gen-3");

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
    deploy_fx(&layout, &good).expect("gen-1 good");

    // Partial-fail gen-2 (omit one pkg): bails before flip, current stays gen-1,
    // gen-2 dir is abandoned (no manifest).
    let broken = temp_dir("deploy-abandoned-broken");
    for pkg in deploy_packages(&split_fleet()) {
        if pkg == "adminctl" {
            continue;
        }
        let file = format!("{pkg}{}", std::env::consts::EXE_SUFFIX);
        std::fs::write(broken.join(&file), b"broken").expect("write source binary");
    }
    deploy_fx(&layout, &broken).expect_err("gen-2 partial fails");

    // gen-3 good: retention protects pre-flip current (gen-1), NOT gen-3-1=gen-2.
    deploy_fx(&layout, &good).expect("gen-3 good");

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

    let error = deploy_fx(&layout, &layout.bin_dir).expect_err("self-deploy must be rejected");
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
    deploy_fx(&layout, &good).expect("gen-1 succeeds");

    // Second source omits one package ⇒ deploy bails.
    let broken = temp_dir("deploy-partial-broken");
    for pkg in deploy_packages(&split_fleet()) {
        if pkg == "adminctl" {
            continue;
        }
        let file = format!("{pkg}{}", std::env::consts::EXE_SUFFIX);
        std::fs::write(broken.join(&file), b"broken v2").expect("write source binary");
    }
    let error = deploy_fx(&layout, &broken).expect_err("missing source ⇒ Err");
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

#[cfg(unix)]
#[test]
fn copy_and_hash_mirrors_the_source_executable_bit_on_unix() {
    // `File::create` gives dst the default 0644 mode, dropping any source +x
    // bit — the deployed binary would then be un-exec'able (`Permission
    // denied (os error 13)`) when weles later tries to spawn it. Proves the
    // fix: an executable source stays executable at the destination.
    use std::os::unix::fs::PermissionsExt;

    let root = temp_dir("copyhash-exec-bit");
    let src = root.join("fake-exe");
    std::fs::write(&src, b"#!/bin/sh\necho hi\n").expect("write source binary");
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755))
        .expect("chmod source +x");
    let dst = root.join("staged-exe");

    copy_and_hash(&src, &dst).expect("copy_and_hash succeeds");

    let dst_mode = std::fs::metadata(&dst).expect("stat dst").permissions().mode();
    assert!(
        dst_mode & 0o111 != 0,
        "staged binary must be executable, got mode {dst_mode:o}"
    );
}

#[cfg(unix)]
#[test]
fn deploy_stages_binaries_executable_on_unix() {
    // The end-to-end regression: a real `weles deploy` run must leave every
    // staged binary in the new generation executable, not just byte-correct
    // — this is what let weles exec them at all on darwin/linux.
    use std::os::unix::fs::PermissionsExt;

    let root = temp_dir("deploy-exec-bit");
    let layout = deploy_layout(&root);
    let src = temp_dir("deploy-exec-bit-src");
    for pkg in deploy_packages(&split_fleet()) {
        let file = format!("{pkg}{}", std::env::consts::EXE_SUFFIX);
        let path = src.join(&file);
        std::fs::write(&path, b"#!/bin/sh\necho hi\n").expect("write source binary");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod source +x");
    }

    deploy_fx(&layout, &src).expect("deploy succeeds");

    let up = Layout::discover(root.clone()).expect("discover pins gen-1");
    for pkg in deploy_packages(&split_fleet()) {
        let dst = up.binary(&pkg);
        let mode = std::fs::metadata(&dst).expect("stat staged").permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "{} must be executable after deploy, got mode {mode:o}",
            dst.display()
        );
    }
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
    // (a file), gen-2 removes cleanly. No state.json under run_dir ⇒ no live pin.
    let run_dir = root.join("run");
    std::fs::create_dir_all(&run_dir).expect("run_dir");
    let removed = prune_stale_generations(&deploy, &run_dir, &[4]);

    assert!(deploy.join("gen-1").exists(), "undeletable gen-1 is skipped, not fatal");
    assert!(!deploy.join("gen-2").exists(), "gen-2 must be pruned");
    assert!(deploy.join("gen-4").exists(), "protected generation is kept");
    assert_eq!(removed, vec![deploy.join("gen-2")], "only gen-2 was removed");
}

/// Writes a minimal but VALID `state.json` recording a live, non-terminal
/// supervisor (this test process's own pid + a fresh `started_unix`, so
/// `supervisor_alive` sees it live and S5's pid-reuse start-time check passes)
/// pinning `gen-<pinned>`. Returns the run_dir it wrote under.
fn write_live_pin_state(run_dir: &std::path::Path, pinned: u64) {
    std::fs::create_dir_all(run_dir).expect("run_dir");
    let state = crate::state::FleetState {
        run_id: "test-run".to_string(),
        supervisor: crate::state::ProcessIdentity {
            pid: std::process::id(),
            started_unix: crate::control::now_unix(),
        },
        fleet_label: "split".to_string(),
        status: crate::state::FleetStatus::Running,
        control_endpoint: None,
        pinned_generation: Some(format!("gen-{pinned}")),
        services: Vec::new(),
    };
    crate::state::checkpoint(&run_dir.join("state.json"), &state).expect("write state.json");
}

#[test]
fn prune_rechecks_live_pin_and_spares_a_generation_the_snapshot_missed() {
    // THE TOCTOU branch: a concurrent `up` pins gen-3 AFTER `deploy` built its
    // `protected` snapshot but BEFORE the delete loop reaches it. The snapshot
    // (`protected`) does NOT list gen-3, so the pre-fix code (delete everything
    // not in `protected`) would remove the LIVE generation's directory and
    // invalidate the pinned `.exe` path — killing every crash-respawn. The fresh
    // live-pin re-read right before destruction must SPARE gen-3.
    let root = temp_dir("prune-live-pin");
    let deploy = root.join("deploy");
    std::fs::create_dir_all(&deploy).expect("deploy");
    // Put a real binary under gen-3 so we can assert the pinned path still
    // resolves after the prune, not merely that the directory lingers.
    std::fs::create_dir_all(deploy.join("gen-3")).expect("gen-3 dir");
    std::fs::write(deploy.join("gen-3").join("server.exe"), b"live-binary").expect("gen-3 exe");
    std::fs::create_dir_all(deploy.join("gen-2")).expect("gen-2 dir");
    std::fs::create_dir_all(deploy.join("gen-4")).expect("gen-4 dir (current)");

    // Live supervisor (this process) pins gen-3; the retention snapshot missed it.
    let run_dir = root.join("run");
    write_live_pin_state(&run_dir, 3);

    // protected = {4} only. gen-3 is NOT protected — only the fresh live-pin
    // re-read inside prune keeps it. gen-2 is genuinely dead and must be pruned.
    let removed = prune_stale_generations(&deploy, &run_dir, &[4]);

    assert!(
        !removed.contains(&deploy.join("gen-3")),
        "the freshly-pinned live generation must not be in the removed list"
    );
    assert!(
        deploy.join("gen-3").join("server.exe").exists(),
        "the pinned binary path must still resolve after prune (the live-pin branch)"
    );
    assert!(!deploy.join("gen-2").exists(), "the genuinely-dead gen-2 must be pruned");
    assert!(deploy.join("gen-4").exists(), "the protected current generation is kept");
    assert_eq!(removed, vec![deploy.join("gen-2")], "only the dead gen-2 was removed");
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

/// Creates a fresh temp directory tree carrying the repo marker
/// (`Cargo.toml` + `tools/processctl/`) at its top plus a nested subdirectory,
/// returning `(marker_root, nested_subdir)`. Both are canonicalized so they can
/// be compared against `resolve_root`'s output (which is derived from
/// `current_dir`, already canonical on macOS where /var → /private/var).
fn marker_tree(name: &str) -> (PathBuf, PathBuf) {
    let root = temp_dir(name);
    std::fs::write(root.join("Cargo.toml"), b"[package]\nname=\"x\"\n")
        .expect("write marker Cargo.toml");
    std::fs::create_dir_all(root.join("tools").join("processctl"))
        .expect("create tools/processctl marker dir");
    let nested = root.join("nested").join("deep");
    std::fs::create_dir_all(&nested).expect("create nested subdir");
    let root = std::fs::canonicalize(&root).expect("canonicalize marker root");
    let nested = std::fs::canonicalize(&nested).expect("canonicalize nested subdir");
    (root, nested)
}

#[test]
fn resolve_root_flag_wins_over_env_and_cwd() {
    let _guard = env_guard();
    // Even with a conflicting WELES_ROOT set and cwd inside a marker tree, an
    // explicit --root value is returned verbatim: it is the highest authority.
    let _env = EnvVarGuard::set("WELES_ROOT", "/env/should/lose");
    let (_root, nested) = marker_tree("flag-wins");
    let _cwd = CwdGuard::set(&nested);
    let flag = PathBuf::from("/operator/pinned/root");
    assert_eq!(resolve_root(Some(flag.clone())).unwrap(), flag);
}

#[test]
fn resolve_root_env_var_overrides_cwd() {
    let _guard = env_guard();
    // No flag: WELES_ROOT beats the cwd marker-walk.
    let (_root, nested) = marker_tree("env-wins");
    let _cwd = CwdGuard::set(&nested);
    let _env = EnvVarGuard::set("WELES_ROOT", "/env/authored/root");
    assert_eq!(resolve_root(None).unwrap(), PathBuf::from("/env/authored/root"));
}

#[test]
fn resolve_root_walks_cwd_up_to_the_marker() {
    let _guard = env_guard();
    // THE failing-branch pin (Finding 1): from a NESTED subdir with no flag and
    // no WELES_ROOT, resolve_root must return the repo-marker ancestor — NOT the
    // flat nested cwd — so weles's `<root>/run/rollout.lock` path stays identical
    // to devctl/verifyctl and the one-Postgres mutual exclusion holds.
    let _env = EnvVarGuard::unset("WELES_ROOT");
    let (root, nested) = marker_tree("marker-walk");
    let _cwd = CwdGuard::set(&nested);
    let resolved = resolve_root(None).expect("resolve via marker walk");
    assert_eq!(resolved, root, "must return the marker root, not the nested cwd");
    assert_ne!(resolved, nested, "must not stop at the flat nested cwd");
}

#[test]
fn resolve_root_no_marker_no_flag_no_env_errors() {
    let _guard = env_guard();
    // A real off-checkout deploy: a temp dir with no marker above it, no flag, no
    // WELES_ROOT. resolve_root must FAIL CLOSED (never a silent flat-cwd fallback
    // that would mis-locate state/lock/deploy).
    let _env = EnvVarGuard::unset("WELES_ROOT");
    let bare = temp_dir("no-marker");
    let bare = std::fs::canonicalize(&bare).expect("canonicalize bare dir");
    let _cwd = CwdGuard::set(&bare);
    let err = resolve_root(None).expect_err("no marker/flag/env must error");
    let message = format!("{err:#}");
    assert!(
        message.contains("--root") && message.contains("WELES_ROOT"),
        "error must guide the operator to --root/WELES_ROOT: {message}"
    );
}

// ---- A1: sha-verify-on-read + `weles rollback` ---------------------------

/// Writes a `manifest.json` into `gen_dir` covering every regular file present
/// (except `manifest.json` itself), with REAL SHA-256/lengths — so a
/// manually-staged generation passes [`verify_generation`], exactly as one
/// staged by `deploy` would. The generation number is parsed from the dir name.
fn write_manifest_for(gen_dir: &Path) {
    let gen = parse_generation(gen_dir.file_name().expect("gen dir name"))
        .expect("gen-<N> dir name");
    let mut artifacts = Vec::new();
    let mut fleet = None;
    for entry in std::fs::read_dir(gen_dir).expect("read gen dir") {
        let entry = entry.expect("dir entry");
        if !entry.path().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "manifest.json" {
            continue;
        }
        let (sha256, bytes) = hash_file(&entry.path()).expect("hash artifact");
        let artifact = Artifact { pkg: name.clone(), file: name.clone(), sha256, bytes };
        if name == "fleet.toml" {
            fleet = Some(artifact);
        } else {
            artifacts.push(artifact);
        }
    }
    let manifest = GenerationManifest {
        gen,
        artifacts,
        fleet: fleet.expect("a fleet.toml artifact"),
    };
    std::fs::write(
        gen_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("serialize manifest"),
    )
    .expect("write manifest.json");
}

/// Manually stages a COMPLETE, verifiable generation `gen-<n>` under `deploy/`:
/// every deployable package plus the split `fleet.toml`, then a real manifest —
/// so [`verify_generation`] passes exactly as a `deploy`-staged generation would.
/// Used to build a deep history (gen-1..gen-5) directly, which sequential
/// `deploy_fx` cannot: deploy's own tail retention keeps only current + pre-flip,
/// so it never leaves five generations on disk at once.
fn stage_manual_generation(deploy_dir: &Path, n: u64, bytes: &[u8]) {
    let gen_dir = deploy_dir.join(format!("gen-{n}"));
    std::fs::create_dir_all(&gen_dir).expect("create gen dir");
    for pkg in deploy_packages(&split_fleet()) {
        let file = format!("{pkg}{}", std::env::consts::EXE_SUFFIX);
        std::fs::write(gen_dir.join(&file), bytes).expect("write gen binary");
    }
    std::fs::copy(split_fleet_path(), gen_dir.join("fleet.toml")).expect("stamp fleet.toml");
    write_manifest_for(&gen_dir);
}

#[test]
fn verify_generation_fails_on_a_tampered_artifact() {
    // THE dead-hash path now lives: flip a byte in a staged binary AFTER the
    // manifest recorded its digest ⇒ verify_generation must catch the mismatch.
    let root = temp_dir("verify-tamper");
    let layout = deploy_layout(&root);
    let src = temp_dir("verify-tamper-src");
    stage_full_source(&src, b"original artifact bytes");
    deploy_fx(&layout, &src).expect("deploy gen-1");

    let gen_dir = root.join("deploy").join("gen-1");
    verify_generation(&gen_dir).expect("a freshly-staged generation verifies clean");

    let pkg = deploy_packages(&split_fleet())[0].clone();
    let victim = gen_dir.join(format!("{pkg}{}", std::env::consts::EXE_SUFFIX));
    let mut bytes = std::fs::read(&victim).expect("read staged binary");
    bytes[0] ^= 0xff; // same length ⇒ a pure content/digest mismatch
    std::fs::write(&victim, &bytes).expect("tamper the staged binary");

    let error = verify_generation(&gen_dir).expect_err("tampered artifact must fail verification");
    let message = format!("{error:#}");
    assert!(
        message.contains(&pkg) && message.contains("sha256"),
        "must name the offending artifact and the hash mismatch: {message}"
    );
}

#[test]
fn verify_generation_fails_on_a_missing_manifest() {
    // A generation dir with binaries but no manifest.json has nothing to verify
    // against ⇒ fail closed (never silently trust an unrecorded generation).
    let root = temp_dir("verify-nomanifest");
    let gen_dir = root.join("deploy").join("gen-1");
    std::fs::create_dir_all(&gen_dir).expect("gen-1");
    std::fs::write(gen_dir.join("server.exe"), b"binary").expect("stage a binary");

    let error = verify_generation(&gen_dir).expect_err("missing manifest must fail");
    assert!(
        format!("{error:#}").contains("manifest"),
        "error must name the manifest: {error:#}"
    );
}

#[test]
fn discover_verifies_the_pinned_generation_and_rejects_tampering() {
    // The wired authority: Layout::discover runs verify_generation before it
    // hands back a bootable layout, so a tampered pinned generation fails the
    // discover (before the rollout lock is even taken), not as an opaque exec
    // crash later.
    let root = temp_dir("discover-verify");
    let layout = deploy_layout(&root);
    let src = temp_dir("discover-verify-src");
    stage_full_source(&src, b"good bytes");
    deploy_fx(&layout, &src).expect("deploy gen-1");
    Layout::discover(root.clone()).expect("a clean generation discovers fine");

    let pkg = deploy_packages(&split_fleet())[0].clone();
    let victim = root
        .join("deploy")
        .join("gen-1")
        .join(format!("{pkg}{}", std::env::consts::EXE_SUFFIX));
    let mut bytes = std::fs::read(&victim).expect("read staged");
    bytes[0] ^= 0xff;
    std::fs::write(&victim, &bytes).expect("tamper");

    let error = Layout::discover(root).expect_err("discover must reject a tampered generation");
    let message = format!("{error:#}");
    assert!(
        message.contains("integrity") || message.contains(&pkg),
        "discover error must reflect the integrity failure: {message}"
    );
}

#[test]
fn rollback_repoints_current_to_the_predecessor_and_up_boots_it() {
    let root = temp_dir("rollback-predecessor");
    let layout = deploy_layout(&root);
    let v1 = temp_dir("rb-v1");
    stage_full_source(&v1, b"v1 bytes");
    deploy_fx(&layout, &v1).expect("gen-1");
    let v2 = temp_dir("rb-v2");
    stage_full_source(&v2, b"v2 bytes");
    deploy_fx(&layout, &v2).expect("gen-2");

    // current is gen-2; a no-target rollback picks gen-1 (highest good below).
    rollback(&deploy_layout(&root), None).expect("rollback to predecessor");
    assert_eq!(
        std::fs::read_to_string(root.join("deploy").join("current")).unwrap().trim(),
        "gen-1"
    );

    // A subsequent boot pins + verifies gen-1 and resolves the OLDER bytes.
    let up = Layout::discover(root.clone()).expect("discover pins gen-1 after rollback");
    assert_eq!(up.active_bin_dir, root.join("deploy").join("gen-1"));
    let pkg = deploy_packages(&split_fleet())[0].clone();
    assert_eq!(std::fs::read(up.binary(&pkg)).unwrap(), b"v1 bytes");
}

#[test]
fn rollback_accepts_an_explicit_bare_number_target() {
    let root = temp_dir("rollback-explicit");
    let layout = deploy_layout(&root);
    let v1 = temp_dir("rbx-v1");
    stage_full_source(&v1, b"one");
    deploy_fx(&layout, &v1).expect("gen-1");
    let v2 = temp_dir("rbx-v2");
    stage_full_source(&v2, b"two");
    deploy_fx(&layout, &v2).expect("gen-2");

    // Bare "1" normalizes to gen-1 (proves normalize_generation_name).
    rollback(&deploy_layout(&root), Some("1")).expect("explicit bare-number target");
    assert_eq!(
        std::fs::read_to_string(root.join("deploy").join("current")).unwrap().trim(),
        "gen-1"
    );
}

#[test]
fn rollback_refuses_a_target_with_a_corrupt_manifest_and_leaves_current() {
    let root = temp_dir("rollback-corrupt");
    let layout = deploy_layout(&root);
    let v1 = temp_dir("rbc-v1");
    stage_full_source(&v1, b"one");
    deploy_fx(&layout, &v1).expect("gen-1");
    let v2 = temp_dir("rbc-v2");
    stage_full_source(&v2, b"two");
    deploy_fx(&layout, &v2).expect("gen-2");

    // Corrupt gen-1's manifest ⇒ verify_generation fails ⇒ rollback refused.
    std::fs::write(
        root.join("deploy").join("gen-1").join("manifest.json"),
        b"{ not valid json",
    )
    .expect("corrupt gen-1 manifest");

    let error = rollback(&deploy_layout(&root), Some("gen-1"))
        .expect_err("a corrupt-manifest target must be refused");
    assert!(
        format!("{error:#}").contains("gen-1"),
        "error must name the refused target: {error:#}"
    );
    // current was NOT flipped — still gen-2.
    assert_eq!(
        std::fs::read_to_string(root.join("deploy").join("current")).unwrap().trim(),
        "gen-2",
        "a refused rollback must not repoint current"
    );
}

#[test]
fn rollback_predecessor_skips_a_manifestless_generation() {
    let root = temp_dir("rollback-skip");
    let layout = deploy_layout(&root);
    let src = temp_dir("rbs-src");
    stage_full_source(&src, b"bytes");
    deploy_fx(&layout, &src).expect("gen-1");
    deploy_fx(&layout, &src).expect("gen-2");
    deploy_fx(&layout, &src).expect("gen-3");
    // Retention kept gen-2 + gen-3 (gen-1 pruned). Remove gen-2's manifest so it
    // is no longer a valid predecessor (an abandoned-partial shape).
    std::fs::remove_file(root.join("deploy").join("gen-2").join("manifest.json"))
        .expect("remove gen-2 manifest");

    // Predecessor of gen-3 is gen-2 (skipped: manifest-less) and gen-1 is gone ⇒
    // nothing good to roll back to.
    let error = rollback(&deploy_layout(&root), None)
        .expect_err("no good predecessor ⇒ rollback refused");
    assert!(
        format!("{error:#}").contains("nothing to roll back to"),
        "error must say there is nothing to roll back to: {error:#}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("deploy").join("current")).unwrap().trim(),
        "gen-3",
        "a refused predecessor rollback must not repoint current"
    );
}

#[test]
fn rollback_protects_generations_newer_than_the_target_so_roll_forward_survives() {
    // The branch the OLD protected set got wrong: rolling back from gen-5 to
    // gen-2 with NO live `up`. The old code protected {target, pre-flip current,
    // live-pin} = {2, 5}, so the intermediate gen-3/gen-4 (real roll-forward
    // candidates) were PRUNED — destroying roll-forward. The new authority
    // protects EVERY present generation >= target, so gen-3/gen-4 survive and
    // only the strictly-older gen-1 is pruned.
    let root = temp_dir("rollback-rollforward");
    let deploy_dir = root.join("deploy");
    for n in 1..=5 {
        stage_manual_generation(&deploy_dir, n, format!("gen-{n} bytes").as_bytes());
    }
    std::fs::write(deploy_dir.join("current"), "gen-5").expect("current -> gen-5");

    // No state.json ⇒ no live pin: the only thing keeping gen-3/gen-4 is the new
    // >= target retention, not a live supervisor.
    rollback(&deploy_layout(&root), Some("gen-2")).expect("rollback to gen-2");

    assert_eq!(
        std::fs::read_to_string(deploy_dir.join("current")).unwrap().trim(),
        "gen-2",
        "rollback repoints current at the target"
    );
    // Survivors: EXACTLY {gen-2, gen-3, gen-4, gen-5}. gen-3/gen-4 are the
    // intermediates the OLD code deleted; they must now survive (roll-forward
    // preserved). gen-1 (strictly older than the target) is pruned by ordinary
    // tail retention.
    assert!(
        !deploy_dir.join("gen-1").exists(),
        "gen-1 (older than the target) must be pruned"
    );
    for n in 2..=5 {
        assert!(
            deploy_dir.join(format!("gen-{n}")).is_dir(),
            "gen-{n} (>= target) must survive the rollback retention"
        );
    }
}

#[test]
fn deploy_lock_serializes_current_mutators() {
    // The deploy/-scoped mutator lock (DISTINCT from run/rollout.lock) is held
    // exclusively: a second acquire while the first is live fails loudly, and it
    // is re-acquirable once released. flock/LockFileEx contend even between two
    // handles in ONE process, so this same mutual exclusion holds across
    // separate `weles deploy`/`rollback` invocations.
    let root = temp_dir("deploy-lock");
    let deploy = root.join("deploy");
    let first = crate::lock::acquire_deploy(&deploy).expect("first acquire creates + locks");
    let error = crate::lock::acquire_deploy(&deploy)
        .expect_err("a second concurrent mutator must be refused");
    let message = format!("{error:#}");
    assert!(
        message.contains("deploy") && message.contains("rollback"),
        "must name the deploy/rollback mutator contention: {message}"
    );
    drop(first);
    crate::lock::acquire_deploy(&deploy).expect("re-acquire succeeds after release");
}

