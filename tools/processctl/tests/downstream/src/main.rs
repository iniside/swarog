#[cfg(target_os = "linux")]
mod linux_fixture {
    use std::collections::BTreeMap;
    use std::ffi::{OsStr, OsString};
    use std::os::unix::process::ExitStatusExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode, Stdio};
    use std::time::{Duration, Instant};

    use processctl::{
        BorrowedLease, FleetState, LeaseError, ManagedProcess, OutputDestination, OwnedChild,
        ProcessGroupPolicy, ProcessIdentity, RolloutLock, ShutdownOutcome, ShutdownPolicy,
        SpawnSpec, StartMarker, StateStore,
    };

    pub(super) fn entry() -> ExitCode {
        if let Some(code) = processctl::dispatch_guardian_from_current_exe() {
            return code;
        }
        match run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("downstream fixture: {error}");
                ExitCode::FAILURE
            }
        }
    }

    fn run() -> Result<(), Box<dyn std::error::Error>> {
        let mut args = std::env::args_os();
        let _binary = args.next();
        match args.next().as_deref().and_then(OsStr::to_str) {
            None | Some("self-test") => self_test(),
            Some("child") => child(args.collect()),
            Some("lease-borrower") => lease_borrower(args.collect()),
            Some("crash-supervisor") => crash_supervisor(args.collect()),
            _ => self_test(),
        }
    }

    fn child(args: Vec<OsString>) -> Result<(), Box<dyn std::error::Error>> {
        let mode = args
            .first()
            .and_then(|arg| arg.to_str())
            .ok_or("missing child mode")?;
        let ready = args.get(1).map(PathBuf::from).ok_or("missing ready path")?;
        match mode {
            "exit" => Ok(()),
            "exit-1" => std::process::exit(1),
            "signal-kill" => {
                unsafe { libc::raise(libc::SIGKILL) };
                unreachable!()
            }
            "term-exit-190" => {
                unsafe extern "C" fn exit_190(_: i32) {
                    unsafe { libc::_exit(190) };
                }
                unsafe { libc::signal(libc::SIGTERM, exit_190 as *const () as usize) };
                std::fs::write(ready, std::process::id().to_string())?;
                std::thread::sleep(Duration::from_secs(60));
                Ok(())
            }
            "sleep" => {
                std::fs::write(ready, std::process::id().to_string())?;
                std::thread::sleep(Duration::from_secs(60));
                Ok(())
            }
            "ignore" => {
                ignore_term();
                std::fs::write(ready, std::process::id().to_string())?;
                std::thread::sleep(Duration::from_secs(60));
                Ok(())
            }
            "escaped" => {
                if unsafe { libc::setsid() } < 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                ignore_term();
                std::fs::write(ready, std::process::id().to_string())?;
                std::thread::sleep(Duration::from_secs(60));
                Ok(())
            }
            "tree-escaped" | "root-graceful-descendant" => {
                if mode == "tree-escaped" {
                    ignore_term();
                }
                let descendant_ready = ready.with_extension("descendant");
                let descendant = Command::new(std::env::current_exe()?)
                    .args([OsStr::new("child"), OsStr::new("escaped")])
                    .arg(&descendant_ready)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()?;
                wait_file(&descendant_ready)?;
                std::fs::write(
                    ready,
                    format!("{}\n{}", std::process::id(), descendant.id()),
                )?;
                std::mem::forget(descendant);
                std::thread::sleep(Duration::from_secs(60));
                Ok(())
            }
            "fd-check" => {
                let clean = (3..64).all(|fd| {
                    let result = unsafe { libc::fcntl(fd, libc::F_GETFD) };
                    result < 0
                        && std::io::Error::last_os_error().raw_os_error() == Some(libc::EBADF)
                });
                std::fs::write(ready, if clean { "closed" } else { "open" })?;
                Ok(())
            }
            "stdin-check" => {
                let unavailable = unsafe { libc::fcntl(0, libc::F_GETFD) } < 0
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::EBADF);
                let replaced_with_null = std::fs::read_link("/proc/self/fd/0")
                    .is_ok_and(|target| target == Path::new("/dev/null"));
                std::fs::write(
                    ready,
                    if unavailable || replaced_with_null {
                        "closed"
                    } else {
                        "leaked"
                    },
                )?;
                Ok(())
            }
            "argv-zero" => {
                // Report the argv[0] this child was exec'd with. The guardian must
                // preserve the ORIGINAL (symlink) path here, not the canonicalized
                // real binary, so a `cargo -> rustup` shim dispatches on basename
                // `cargo`. See argv_zero_symlink_test.
                let argv0 = std::env::args_os()
                    .next()
                    .ok_or("child was exec'd without argv[0]")?;
                std::fs::write(ready, argv0.to_string_lossy().as_bytes())?;
                Ok(())
            }
            _ => Err(format!("unknown child mode {mode}").into()),
        }
    }

    fn self_test() -> Result<(), Box<dyn std::error::Error>> {
        let dir = test_dir()?;
        let sibling_guardian = std::env::current_exe()?.with_file_name("processctl-guardian");
        if sibling_guardian.exists() {
            return Err(format!(
                "unexpected sibling guardian: {}",
                sibling_guardian.display()
            )
            .into());
        }

        let ready = dir.join("force-tree.ready");
        let mut tree = spawn("tree-escaped", &ready)?;
        wait_file(&ready)?;
        let tree_pids = read_pids(&ready)?;
        let outcome = tree.shutdown(policy(Duration::from_millis(100)))?;
        if !matches!(outcome, ShutdownOutcome::Forced(_)) {
            return Err("ignored graceful signal did not force owned tree".into());
        }
        wait_dead(tree_pids[0])?;
        wait_dead(tree_pids[1])?;

        let ready = dir.join("graceful-descendant.ready");
        let mut root = spawn("root-graceful-descendant", &ready)?;
        wait_file(&ready)?;
        let root_pids = read_pids(&ready)?;
        let outcome = root.shutdown(policy(Duration::from_secs(3)))?;
        if !matches!(outcome, ShutdownOutcome::Forced(_)) {
            return Err("forced descendant cleanup was reported as wholly graceful".into());
        }
        wait_dead(root_pids[0])?;
        wait_dead(root_pids[1])?;

        let ready = dir.join("fds.ready");
        let mut fds = spawn("fd-check", &ready)?;
        wait_file(&ready)?;
        if std::fs::read_to_string(&ready)? != "closed" {
            return Err("guardian pipe descriptor leaked into target".into());
        }
        wait_exit(&mut fds)?;

        let link = dir.join("consumer-link");
        std::os::unix::fs::symlink(std::env::current_exe()?, &link)?;
        let ready = dir.join("symlink.ready");
        let mut linked = spawn_executable(&link, ["child", "ignore", path_str(&ready)?])?;
        wait_file(&ready)?;
        if linked.identity().executable != std::fs::canonicalize(std::env::current_exe()?)? {
            return Err("symlink identity is not the actual executable".into());
        }
        std::fs::remove_file(&link)?;
        let _ = linked.shutdown(policy(Duration::from_millis(100)))?;

        let script = dir.join("target-script");
        std::fs::write(&script, "#!/bin/sh\ntrap '' TERM\n/bin/sleep 60\n")?;
        set_executable(&script)?;
        let mut scripted = spawn_executable(&script, std::iter::empty::<&str>())?;
        if scripted.identity().executable != std::fs::canonicalize("/bin/sh")? {
            return Err(format!(
                "shebang identity {:?} is not the actual interpreter",
                scripted.identity().executable
            )
            .into());
        }
        std::fs::write(&script, "#!/bin/false\n")?;
        let _ = scripted.shutdown(policy(Duration::from_millis(100)))?;

        let ready = dir.join("exit-unused");
        let mut short = spawn("exit", &ready)?;
        wait_exit(&mut short)?;

        let ready = dir.join("exit-190.ready");
        let mut exit_190 = spawn("term-exit-190", &ready)?;
        wait_file(&ready)?;
        match exit_190.shutdown(policy(Duration::from_secs(3)))? {
            ShutdownOutcome::Graceful(status) if status.code() == Some(190) => {}
            other => {
                return Err(
                    format!("target exit 190 collided with guardian status: {other:?}").into(),
                )
            }
        }

        let mut exit_1 = spawn("exit-1", &dir.join("exit-1-unused"))?;
        if wait_status(&mut exit_1)?.code() != Some(1) {
            return Err("target exit 1 was not preserved".into());
        }

        let mut signalled = spawn("signal-kill", &dir.join("signal-unused"))?;
        if wait_status(&mut signalled)?.signal() != Some(libc::SIGKILL) {
            return Err("target signal wait status was not preserved".into());
        }

        argv_zero_symlink_test(&dir)?;
        crash_cleanup_test(&dir)?;
        checkpoint_rollback_test(&dir)?;
        stale_state_identity_test(&dir)?;
        oversized_credential_delivery_test(&dir)?;
        inherited_lease_test(&dir)?;
        println!("processctl downstream fixture: PASS");
        Ok(())
    }

    fn crash_supervisor(args: Vec<OsString>) -> Result<(), Box<dyn std::error::Error>> {
        let supervisor_ready = args
            .first()
            .map(PathBuf::from)
            .ok_or("missing supervisor ready")?;
        let tree_ready = args.get(1).map(PathBuf::from).ok_or("missing tree ready")?;
        let owned = spawn("tree-escaped", &tree_ready)?;
        wait_file(&tree_ready)?;
        std::fs::write(supervisor_ready, owned.identity().pid.to_string())?;
        std::mem::forget(owned);
        std::process::abort();
    }

    fn argv_zero_symlink_test(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
        // Reproduces the `cargo -> rustup` shim case: a symlink whose canonicalized
        // target differs from its own path. The guardian must exec the child with
        // argv[0] == the symlink path (so the rustup proxy dispatches on basename
        // `cargo`), NOT the canonicalized real binary. Pre-fix the guardian ran
        // `Command::new(canonicalize(symlink))` with no arg0, so the child observed
        // the resolved binary path and a rustup shim would misdispatch.
        let cargo_link = dir.join("cargo");
        std::os::unix::fs::symlink(std::env::current_exe()?, &cargo_link)?;
        let ready = dir.join("argv-zero.ready");
        let mut child =
            spawn_executable(&cargo_link, ["child", "argv-zero", path_str(&ready)?])?;
        wait_file(&ready)?;
        let observed = std::fs::read_to_string(&ready)?;
        let expected = path_str(&cargo_link)?;
        if observed != expected {
            wait_exit(&mut child)?;
            return Err(format!(
                "guardian did not preserve symlink argv[0]: child saw {observed:?}, expected {expected:?}"
            )
            .into());
        }
        // The exact failing branch: pre-fix argv[0] equalled the canonicalized target.
        let resolved = std::fs::canonicalize(&cargo_link)?;
        if resolved.as_path() == Path::new(&observed) {
            wait_exit(&mut child)?;
            return Err(format!(
                "child argv[0] {observed:?} was the canonicalized target {resolved:?}; a cargo->rustup shim would misdispatch"
            )
            .into());
        }
        wait_exit(&mut child)?;
        std::fs::remove_file(&cargo_link)?;
        Ok(())
    }

    fn crash_cleanup_test(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let supervisor_ready = dir.join("supervisor.ready");
        let tree_ready = dir.join("crash-tree.ready");
        let decoy_ready = dir.join("decoy.ready");
        let mut decoy = Command::new(std::env::current_exe()?)
            .args(["child", "sleep"])
            .arg(&decoy_ready)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        wait_file(&decoy_ready)?;
        let mut supervisor = Command::new(std::env::current_exe()?)
            .arg("crash-supervisor")
            .arg(&supervisor_ready)
            .arg(&tree_ready)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        wait_file(&supervisor_ready)?;
        let pids = read_pids(&tree_ready)?;
        if supervisor.wait()?.success() {
            return Err("crash supervisor unexpectedly succeeded".into());
        }
        wait_dead(pids[0])?;
        wait_dead(pids[1])?;
        if !process_alive(decoy.id()) {
            return Err("unrelated decoy was killed".into());
        }
        let _ = decoy.kill();
        let _ = decoy.wait();
        Ok(())
    }

    fn checkpoint_rollback_test(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let first_ready = dir.join("checkpoint-first.ready");
        let second_ready = dir.join("checkpoint-second.ready");
        let first = spawn("ignore", &first_ready)?;
        let second = spawn("ignore", &second_ready)?;
        wait_file(&first_ready)?;
        wait_file(&second_ready)?;
        let pids = [first.identity().pid, second.identity().pid];
        let mut started = vec![first, second];
        let state = FleetState::new("fixture-rollback", "split")?;
        let store = StateStore::new(dir.join("missing-parent").join("fleet.json"));
        let error = store
            .checkpoint_or_rollback(&state, &mut started, policy(Duration::from_millis(100)))
            .unwrap_err();
        if !error.cleanup_failures.is_empty() {
            return Err(format!("checkpoint rollback failed: {error}").into());
        }
        wait_dead(pids[0])?;
        wait_dead(pids[1])?;
        Ok(())
    }

    fn stale_state_identity_test(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let ready = dir.join("stale-decoy.ready");
        let mut decoy = Command::new(std::env::current_exe()?)
            .args(["child", "sleep"])
            .arg(&ready)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        wait_file(&ready)?;
        let mut state = FleetState::new("stale-run", "split")?;
        state.push_process(ManagedProcess::new(
            "stale-decoy",
            ProcessIdentity {
                pid: decoy.id(),
                executable: PathBuf::from("definitely-not-the-decoy"),
                started: StartMarker(0),
            },
            PathBuf::from("stale.out"),
            PathBuf::from("stale.err"),
        )?);
        let store = StateStore::new(dir.join("stale-state.json"));
        store.write_atomic(&state)?;
        let _loaded = store.load()?.ok_or("stale state disappeared")?;
        if !process_alive(decoy.id()) {
            return Err("loading stale identity signalled an unrelated decoy".into());
        }
        let _ = decoy.kill();
        let _ = decoy.wait();
        Ok(())
    }

    fn inherited_lease_test(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let mut owner =
            RolloutLock::acquire(dir.join("rollout.lock"), "fixture-borrow", "splitproof")?;
        let ready = dir.join("borrower.ready");
        let error_log = ready.with_extension("err");
        assert!(matches!(
            owner.spawn_borrower(borrower_spec(&ready)?, "wrong-role"),
            Err(LeaseError::WrongRole { .. })
        ));
        let mut borrower = owner.spawn_borrower(borrower_spec(&ready)?, "splitproof")?;
        assert!(matches!(
            RolloutLock::acquire(dir.join("rollout.lock"), "competing-run", "splitproof"),
            Err(LeaseError::AlreadyOwned)
        ));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() {
            if let Some(status) = borrower.try_wait()? {
                let detail = std::fs::read_to_string(&error_log).unwrap_or_default();
                return Err(format!(
                    "borrower exited before consuming the inherited lease ({status}): {detail}"
                )
                .into());
            }
            if Instant::now() >= deadline {
                let detail = std::fs::read_to_string(&error_log).unwrap_or_default();
                return Err(format!("timed out waiting for borrower: {detail}").into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if std::fs::read_to_string(&ready)? != "borrowed-ok"
            || !wait_borrowed_status(&mut borrower)?.success()
        {
            return Err("borrower did not consume the inherited lease".into());
        }
        drop(borrower);
        assert!(
            std::fs::read_dir(dir)?.any(|entry| entry.is_ok_and(|entry| entry
                .path()
                .extension()
                .is_some_and(|value| value == "borrowed")))
        );
        assert!(matches!(
            owner.spawn_borrower(borrower_spec(&ready)?, "splitproof"),
            Err(LeaseError::BorrowerAlreadyIssued)
        ));
        drop(owner);
        assert!(
            !std::fs::read_dir(dir)?.any(|entry| entry.is_ok_and(|entry| entry
                .path()
                .extension()
                .is_some_and(|value| value == "borrowed")))
        );
        Ok(())
    }

    fn oversized_credential_delivery_test(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let mut long_dir = dir.join("long-credential");
        for index in 0..19 {
            long_dir = long_dir.join(format!("{index:02}-{}", "x".repeat(190)));
        }
        std::fs::create_dir_all(&long_dir)?;
        let lock_path = long_dir.join("rollout.lock");
        assert!(lock_path.as_os_str().len() > 3650);
        let mut owner = RolloutLock::acquire(&lock_path, "fixture-borrow", "splitproof")?;
        let ignored_ready = dir.join("oversized-ignored.ready");
        let ignored_spec = SpawnSpec {
            label: "credential-non-reader".into(),
            executable: std::env::current_exe()?,
            args: vec![
                OsString::from("child"),
                OsString::from("ignore"),
                ignored_ready.as_os_str().to_owned(),
            ],
            env: BTreeMap::new(),
            cwd: std::env::current_dir()?,
            stdout: OutputDestination::Null,
            stderr: OutputDestination::Null,
            process_group: ProcessGroupPolicy::Owned,
        };
        match owner.spawn_borrower(ignored_spec, "splitproof") {
            Err(LeaseError::CredentialDeliveryTimeout(_)) => {}
            Err(error) => {
                return Err(format!("unexpected oversized delivery error: {error}").into())
            }
            Ok(_) => return Err("oversized credential fit in the bounded pipe".into()),
        }

        let ready = dir.join("oversized-borrower.ready");
        let mut borrower = owner.spawn_borrower(borrower_spec(&ready)?, "splitproof")?;
        wait_file(&ready)?;
        if std::fs::read_to_string(&ready)? != "borrowed-ok"
            || !wait_borrowed_status(&mut borrower)?.success()
        {
            return Err("oversized credential retry did not complete".into());
        }
        Ok(())
    }

    fn wait_borrowed_status(
        child: &mut processctl::BorrowedChild<'_>,
    ) -> Result<std::process::ExitStatus, processctl::LeaseError> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(processctl::LeaseError::InvalidField(
                    "timed out waiting for borrowed child exit".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn borrower_spec(ready: &Path) -> Result<SpawnSpec, Box<dyn std::error::Error>> {
        let mut env = BTreeMap::new();
        if let Some(path) = std::env::var_os("PATH") {
            env.insert(OsString::from("PATH"), path);
        }
        Ok(SpawnSpec {
            label: "lease-borrower".into(),
            executable: std::env::current_exe()?,
            args: vec![
                OsString::from("lease-borrower"),
                ready.as_os_str().to_owned(),
            ],
            env,
            cwd: std::env::current_dir()?,
            stdout: OutputDestination::Null,
            stderr: OutputDestination::File(ready.with_extension("err")),
            process_group: ProcessGroupPolicy::Owned,
        })
    }

    fn lease_borrower(args: Vec<OsString>) -> Result<(), Box<dyn std::error::Error>> {
        let lease = BorrowedLease::consume_inherited_if_present("splitproof")?
            .ok_or("borrower marker was not detected")?;
        if lease.run_id() != "fixture-borrow"
            || BorrowedLease::consume_inherited("splitproof").is_ok()
        {
            return Err("borrower credential was wrong or consumable twice".into());
        }
        let ready = args
            .first()
            .map(PathBuf::from)
            .ok_or("missing borrower ready")?;
        let child_ready = ready.with_extension("child");
        let grandchild_ready = ready.with_extension("grandchild");
        let mut child = Command::new(std::env::current_exe()?)
            .args(["child", "stdin-check"])
            .arg(&child_ready)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let mut grandchild = Command::new(std::env::current_exe()?)
            .args(["child", "stdin-check"])
            .arg(&grandchild_ready)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        wait_file(&child_ready)?;
        wait_file(&grandchild_ready)?;
        if !child.wait()?.success()
            || !grandchild.wait()?.success()
            || std::fs::read_to_string(child_ready)? != "closed"
            || std::fs::read_to_string(grandchild_ready)? != "closed"
        {
            return Err("credential handle leaked to a fake service or grandchild".into());
        }
        if !Command::new("cargo")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success()
        {
            return Err("cargo child failed after credential consumption".into());
        }
        std::fs::write(ready, "borrowed-ok")?;
        Ok(())
    }

    fn spawn(mode: &str, ready: &Path) -> Result<OwnedChild, processctl::ProcessError> {
        spawn_executable(
            &std::env::current_exe().expect("current executable"),
            ["child", mode, path_str(ready).expect("UTF-8 fixture path")],
        )
    }

    fn spawn_executable<I, S>(
        executable: &Path,
        args: I,
    ) -> Result<OwnedChild, processctl::ProcessError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut env = BTreeMap::new();
        if let Some(path) = std::env::var_os("PATH") {
            env.insert(OsString::from("PATH"), path);
        }
        OwnedChild::spawn(SpawnSpec {
            label: executable.display().to_string(),
            executable: executable.to_path_buf(),
            args: args
                .into_iter()
                .map(|arg| arg.as_ref().to_owned())
                .collect(),
            env,
            cwd: std::env::current_dir().expect("current directory"),
            stdout: OutputDestination::Null,
            stderr: OutputDestination::Null,
            process_group: ProcessGroupPolicy::Owned,
        })
    }

    fn policy(graceful_timeout: Duration) -> ShutdownPolicy {
        ShutdownPolicy {
            graceful_timeout,
            force_timeout: Duration::from_secs(5),
        }
    }

    fn wait_exit(child: &mut OwnedChild) -> Result<(), Box<dyn std::error::Error>> {
        wait_status(child).map(|_| ())
    }

    fn wait_status(
        child: &mut OwnedChild,
    ) -> Result<std::process::ExitStatus, Box<dyn std::error::Error>> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err("timed out waiting for child exit".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_file(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            if Instant::now() >= deadline {
                return Err(format!("timed out waiting for {}", path.display()).into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    fn read_pids(path: &Path) -> Result<Vec<u32>, Box<dyn std::error::Error>> {
        Ok(std::fs::read_to_string(path)?
            .lines()
            .map(str::parse)
            .collect::<Result<_, _>>()?)
    }

    fn wait_dead(pid: u32) -> Result<(), Box<dyn std::error::Error>> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while process_alive(pid) {
            if Instant::now() >= deadline {
                return Err(format!("pid {pid} stayed alive").into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    fn process_alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    fn ignore_term() {
        unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) };
    }

    fn test_dir() -> std::io::Result<PathBuf> {
        let path =
            std::env::temp_dir().join(format!("processctl-downstream-{}", std::process::id()));
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        std::fs::create_dir_all(&path)?;
        Ok(path)
    }

    fn path_str(path: &Path) -> Result<&str, Box<dyn std::error::Error>> {
        path.to_str()
            .ok_or_else(|| "fixture path is not UTF-8".into())
    }

    fn set_executable(path: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions)
    }
}

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux_fixture::entry()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::SUCCESS
}
