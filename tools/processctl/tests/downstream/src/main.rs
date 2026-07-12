use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use processctl::{
    OutputDestination, OwnedChild, ProcessGroupPolicy, ShutdownOutcome, ShutdownPolicy, SpawnSpec,
};

fn main() -> ExitCode {
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
        Some("self-test") => self_test(),
        Some("child") => child(args.collect()),
        Some("crash-supervisor") => crash_supervisor(args.collect()),
        _ => Err("expected self-test, child, or crash-supervisor".into()),
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
                result < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EBADF)
            });
            std::fs::write(ready, if clean { "closed" } else { "open" })?;
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

    crash_cleanup_test(&dir)?;
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
    OwnedChild::spawn(SpawnSpec {
        label: executable.display().to_string(),
        executable: executable.to_path_buf(),
        args: args
            .into_iter()
            .map(|arg| arg.as_ref().to_owned())
            .collect(),
        env: BTreeMap::new(),
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
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            return Err("timed out waiting for child exit".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
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
    let path = std::env::temp_dir().join(format!("processctl-downstream-{}", std::process::id()));
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
