use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use processctl::{
    BorrowedLease, LeaseError, OutputDestination, OwnedChild, ProcessGroupPolicy, RolloutLock,
    SpawnSpec,
};

const PRIVATE_MARKER: &str = "--processctl-borrowed-lease-v1";

fn main() -> ExitCode {
    if let Some(exit) = processctl::dispatch_guardian_from_current_exe() {
        return exit;
    }
    let result = match std::env::args_os().nth(1).as_deref().and_then(OsStr::to_str) {
        Some("direct-pipe") => direct_pipe_child(),
        Some("marker-no-pipe") => marker_no_pipe_child(),
        Some("borrower") => borrower_child(),
        _ => self_test(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lease marker fixture: {error}");
            ExitCode::FAILURE
        }
    }
}

fn self_test() -> Result<(), Box<dyn std::error::Error>> {
    let directory = test_directory()?;

    let mut direct = Command::new(std::env::current_exe()?)
        .arg("direct-pipe")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    direct
        .stdin
        .take()
        .ok_or("direct fixture stdin was absent")?
        .write_all(b"ordinary piped input")?;
    if !direct.wait()?.success() {
        return Err("ordinary piped stdin was mistaken for a borrower credential".into());
    }

    let mut marker_only = OwnedChild::spawn(spec(
        "marker-no-pipe",
        vec!["marker-no-pipe".into(), PRIVATE_MARKER.into()],
        &directory,
    ))?;
    if !wait_exit(&mut marker_only)?.success() {
        return Err("marker without credential pipe did not fail closed".into());
    }

    let mut owner = RolloutLock::acquire(
        directory.join("rollout.lock"),
        "marker-fixture",
        ["splitproof"],
    )?;
    let ready = directory.join("borrower.ready");
    let mut borrower = owner.spawn_borrower(
        spec(
            "borrower",
            vec!["borrower".into(), ready.as_os_str().to_owned()],
            &directory,
        ),
        "splitproof",
    )?;
    wait_file(&ready)?;
    if std::fs::read_to_string(&ready)? != "borrowed-ok"
        || !wait_exit_borrowed(&mut borrower)?.success()
    {
        return Err("real borrower did not consume the marked credential".into());
    }
    Ok(())
}

fn direct_pipe_child() -> Result<(), Box<dyn std::error::Error>> {
    if BorrowedLease::consume_inherited_if_present("splitproof")?.is_some() {
        return Err("unmarked pipe yielded a lease".into());
    }
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    if input != b"ordinary piped input" {
        return Err("optional lease check touched ordinary stdin".into());
    }
    Ok(())
}

fn marker_no_pipe_child() -> Result<(), Box<dyn std::error::Error>> {
    match BorrowedLease::consume_inherited_if_present("splitproof") {
        Err(LeaseError::BorrowerMarkerWithoutPipe) => Ok(()),
        _ => Err("marker without pipe was not rejected".into()),
    }
}

fn borrower_child() -> Result<(), Box<dyn std::error::Error>> {
    let lease = BorrowedLease::consume_inherited_if_present("splitproof")?
        .ok_or("marked borrower did not receive a lease")?;
    if lease.run_id() != "marker-fixture" {
        return Err("borrower received the wrong lease".into());
    }
    let ready = std::env::args_os()
        .nth(2)
        .map(PathBuf::from)
        .ok_or("borrower ready path was absent")?;
    std::fs::write(ready, "borrowed-ok")?;
    Ok(())
}

fn spec(label: &str, args: Vec<OsString>, cwd: &Path) -> SpawnSpec {
    let mut env = BTreeMap::new();
    for key in ["PATH", "PATHEXT", "SYSTEMROOT", "WINDIR"] {
        if let Some(value) = std::env::var_os(key) {
            env.insert(OsString::from(key), value);
        }
    }
    SpawnSpec {
        label: label.into(),
        executable: std::env::current_exe().expect("fixture executable"),
        args,
        env,
        cwd: cwd.to_path_buf(),
        stdout: OutputDestination::Null,
        stderr: OutputDestination::Null,
        process_group: ProcessGroupPolicy::Owned,
    }
}

fn wait_exit(child: &mut OwnedChild) -> Result<std::process::ExitStatus, processctl::ProcessError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(processctl::ProcessError::ForceTimeout {
                label: "lease marker fixture".into(),
                timeout: Duration::from_secs(10),
            });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_exit_borrowed(
    child: &mut processctl::BorrowedChild<'_>,
) -> Result<std::process::ExitStatus, LeaseError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(LeaseError::InvalidField("borrower fixture timed out".into()));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_file(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.is_file() {
        if Instant::now() >= deadline {
            return Err("borrower fixture ready file timed out".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn test_directory() -> std::io::Result<PathBuf> {
    let directory =
        std::env::temp_dir().join(format!("processctl-lease-marker-{}", std::process::id()));
    if directory.exists() {
        std::fs::remove_dir_all(&directory)?;
    }
    std::fs::create_dir_all(&directory)?;
    Ok(directory)
}
