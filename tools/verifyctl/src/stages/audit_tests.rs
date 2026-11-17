//! Tests for `premise_finding`, the classifier of the sqlite ignore's premise
//! check. Each case is pure: it feeds the classifier one shape of `cargo tree
//! -i libsqlite3-sys` output and asserts the class of its finding. The one
//! exception is the real-cargo anchor test, which runs real `cargo tree` in a
//! throwaway workspace. The runner tests in `tests/runner.rs` drive the same
//! shapes through the real stage.

use super::*;

/// Real cargo's stderr for a crate that is in the lock but not in the build
/// graph.
const NOTHING_TO_PRINT: &str = "warning: nothing to print.\n";

/// Real cargo's stderr for a crate that is not in the lock at all.
const NO_MATCH: &str =
    "error: package ID specification `libsqlite3-sys` did not match any packages\n";

/// An inverted tree: the crate is compiled.
const TREE: &str = "libsqlite3-sys v0.30.1\n└── sqlx-sqlite v0.8.6\n";

fn finding_of(passed: bool, stdout: &str, stderr: &str) -> String {
    premise_finding(passed, stdout, stderr)
        .unwrap_or_else(|| panic!("expected a finding for {passed} / {stdout:?} / {stderr:?}"))
}

#[test]
fn a_lock_only_crate_justifies_the_ignore() {
    assert_eq!(premise_finding(true, "", NOTHING_TO_PRINT), None);
    // Whitespace-only stdout is still an empty tree.
    assert_eq!(premise_finding(true, "\n  \n", NOTHING_TO_PRINT), None);
}

#[test]
fn a_compiled_crate_fails_even_beside_the_warning() {
    let finding = finding_of(true, TREE, "");
    assert!(
        finding.starts_with("libsqlite3-sys is compiled;"),
        "{finding}"
    );
    // The tree wins over a "nothing to print" that also appears on stderr.
    let finding = finding_of(true, TREE, NOTHING_TO_PRINT);
    assert!(
        finding.starts_with("libsqlite3-sys is compiled;"),
        "{finding}"
    );
}

#[test]
fn an_empty_tree_without_the_warning_fails() {
    let finding = finding_of(true, "", "");
    assert!(
        finding.contains("printed nothing without its \"nothing to print\" warning"),
        "{finding}"
    );
}

#[test]
fn a_crate_absent_from_the_lock_is_a_stale_ignore() {
    assert_eq!(
        finding_of(false, "", NO_MATCH),
        "RUSTSEC-2026-0459 ignore is stale; remove it"
    );
}

/// Anchors the stale wording on real cargo rather than on the string the fake
/// cargo and `NO_MATCH` share: the stage's own premise argv, with the crate
/// swapped for a name no lock holds, is run in a throwaway workspace and its
/// real exit and output go through `premise_finding`. The workspace is one
/// package with no dependencies, so `--offline --target all` resolves nothing
/// from the registry and the run does not depend on the package cache.
#[test]
fn real_cargo_tree_on_a_crate_absent_from_the_lock_reads_as_stale() {
    const ABSENT_CRATE: &str = "verifyctl-premise-probe-that-no-lock-holds";
    let args: Vec<&str> = SQLITE_PREMISE_ARGS
        .iter()
        .map(|&arg| if arg == SQLITE_CRATE { ABSENT_CRATE } else { arg })
        .chain(["--offline"])
        .collect();
    assert!(args.contains(&ABSENT_CRATE), "the premise argv lost its crate: {args:?}");

    let workspace = RemovedOnDrop(
        super::super::temp_dir(&std::env::temp_dir(), "verifyctl-premise-probe")
            .expect("create the throwaway workspace"),
    );
    std::fs::create_dir_all(workspace.0.join("src")).expect("create src");
    std::fs::write(
        workspace.0.join("Cargo.toml"),
        "[workspace]\n\n[package]\nname = \"premise-probe\"\nversion = \"0.0.0\"\n\
         edition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    std::fs::write(workspace.0.join("src/lib.rs"), "").expect("write src/lib.rs");

    // `--locked` refuses to create a lock, so the lock is generated first, offline.
    let (status, stdout, stderr) =
        run_cargo_bounded(&workspace.0, &["generate-lockfile", "--offline"]);
    assert!(
        status.success(),
        "generate-lockfile: {status}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    let (status, stdout, stderr) = run_cargo_bounded(&workspace.0, &args);
    assert_eq!(
        premise_finding(status.success(), &stdout, &stderr).as_deref(),
        Some("RUSTSEC-2026-0459 ignore is stale; remove it"),
        "{status}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// Owns a temp directory and removes it when dropped, so the directory goes
/// away on every exit path of the test, panics included.
struct RemovedOnDrop(std::path::PathBuf);

impl Drop for RemovedOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs the cargo that runs this test in `dir` and returns its exit and output,
/// draining both pipes on their own threads so a full pipe cannot stall it.
fn run_cargo_bounded(
    dir: &std::path::Path,
    args: &[&str],
) -> (std::process::ExitStatus, String, String) {
    use std::io::Read as _;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut child = Command::new(&cargo)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn {cargo:?}: {error}"));
    let drain = |mut pipe: Box<dyn std::io::Read + Send>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            pipe.read_to_string(&mut text).expect("read cargo output");
            text
        })
    };
    let stdout = drain(Box::new(child.stdout.take().expect("piped stdout")));
    let stderr = drain(Box::new(child.stderr.take().expect("piped stderr")));
    // Hang guard only, with wide headroom: neither command builds anything, so
    // a run that is still going after this long is blocked, not slow.
    let deadline = Instant::now() + Duration::from_secs(300);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll cargo") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "cargo {args:?} did not finish within 300 s (likely blocked on the \
                 package-cache lock)"
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = stdout.join().expect("stdout reader");
    let stderr = stderr.join().expect("stderr reader");
    (status, stdout, stderr)
}

#[test]
fn any_other_cargo_failure_fails_and_is_not_read_as_stale_or_lock_only() {
    // A failing exit is never a pass, even with the lock-only warning on stderr.
    for stderr in [
        "error: the lock file needs to be updated but --locked was passed\n",
        "",
        NOTHING_TO_PRINT,
    ] {
        let finding = finding_of(false, "", stderr);
        assert!(
            finding.starts_with("cargo tree -i libsqlite3-sys failed"),
            "{stderr:?}: {finding}"
        );
    }
}
