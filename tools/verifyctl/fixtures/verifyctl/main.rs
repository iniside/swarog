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
    if stem.eq_ignore_ascii_case("cargo-audit") {
        return audit();
    }
    cargo(executable)
}

fn cargo(executable: PathBuf) -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    record(&format!("cargo {}", args.join(" ")));
    if control("sleep-build") && args.join(" ").contains("build --workspace") {
        record("sleeping");
        std::thread::sleep(Duration::from_secs(30));
    }
    if args.first().map(String::as_str) == Some("install") {
        if control("install-fail") {
            return ExitCode::FAILURE;
        }
        let bin = executable.parent().expect("fake bin directory");
        let target = bin.join(format!("cargo-audit{}", std::env::consts::EXE_SUFFIX));
        std::fs::copy(executable, target).expect("install fake cargo-audit");
        return ExitCode::SUCCESS;
    }
    if control("route-fail") && args.join(" ").contains("routecheck") {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn audit() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    record(&format!("cargo-audit {}", args.join(" ")));
    if args != ["audit", "--ignore", "RUSTSEC-2023-0071"] {
        record("cargo-audit argv mismatch");
        return ExitCode::FAILURE;
    }
    if control("audit-network-fail") {
        record("audit network failure");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
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
    ExitCode::SUCCESS
}

fn control(name: &str) -> bool {
    std::env::var("RUSTFLAGS")
        .ok()
        .is_some_and(|value| value.split(',').any(|item| item == name))
}

fn record(message: &str) {
    let Some(target) = std::env::var_os("CARGO_TARGET_DIR") else {
        return;
    };
    let path = PathBuf::from(target)
        .parent()
        .expect("target parent")
        .join("record.log");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open record");
    writeln!(file, "{message}").expect("write record");
    if std::env::var_os("VERIFYCTL_POISON").is_some() {
        writeln!(file, "POISON LEAKED").expect("write poison marker");
    }
}
