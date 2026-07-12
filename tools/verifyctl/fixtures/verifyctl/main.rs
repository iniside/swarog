use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

fn main() -> ExitCode {
    if let Some(code) = processctl::dispatch_guardian_from_current_exe() {
        return code;
    }
    let executable = std::env::current_exe().expect("fixture executable");
    let stem = executable
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if stem.eq_ignore_ascii_case("splitproof") {
        return splitproof();
    }
    cargo(executable)
}

fn cargo(executable: PathBuf) -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    record(&format!("cargo {}", args.join(" ")));
    if matches_env("FAKE_SLEEP_MATCH", &args) {
        record("sleeping");
        std::thread::sleep(Duration::from_secs(30));
    }
    if args.first().map(String::as_str) == Some("install") {
        if std::env::var_os("FAKE_INSTALL_FAIL").is_some() {
            return ExitCode::FAILURE;
        }
        let bin = PathBuf::from(std::env::var_os("FAKE_BIN_DIR").expect("FAKE_BIN_DIR"));
        let target = bin.join(format!("cargo-audit{}", std::env::consts::EXE_SUFFIX));
        std::fs::copy(executable, target).expect("install fake cargo-audit");
        return ExitCode::SUCCESS;
    }
    if args.first().map(String::as_str) == Some("audit")
        && std::env::var_os("FAKE_AUDIT_FAIL").is_some()
    {
        record("audit network failure");
        return ExitCode::FAILURE;
    }
    if matches_env("FAKE_FAIL_MATCH", &args) {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn splitproof() -> ExitCode {
    match processctl::BorrowedLease::consume_inherited_if_present("splitproof") {
        Ok(Some(lease)) => record(&format!("splitproof borrowed {}", lease.run_id())),
        Ok(None) => {
            record("splitproof lease error: missing lease");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            record(&format!("splitproof lease error: {error}"));
            return ExitCode::FAILURE;
        }
    }
    if std::env::var_os("FAKE_SPLITPROOF_FAIL").is_some() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn matches_env(name: &str, args: &[String]) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|needle| args.join(" ").contains(&needle))
}

fn record(message: &str) {
    let Some(path) = std::env::var_os("FAKE_RECORD") else {
        return;
    };
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open record");
    writeln!(file, "{message}").expect("write record");
}
