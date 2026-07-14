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
    if control("sleep-decoy") {
        std::thread::sleep(Duration::from_secs(30));
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|arg| arg == "public-api") {
        if let Some(index) = args.iter().position(|arg| arg == "-p") {
            let root = std::env::current_dir().expect("fixture cwd");
            let snapshot = std::fs::read_to_string(
                root.join("docs/reference/public-api-baseline")
                    .join(format!("{}.txt", args[index + 1])),
            )
            .expect("public-api fixture snapshot");
            for line in snapshot
                .lines()
                .filter(|line| !line.starts_with("# cargo-public-api"))
            {
                println!("{line}");
            }
        } else {
            println!("cargo-public-api 0.52.0");
        }
    }
    if let Some(index) = args.iter().position(|arg| arg == "--out") {
        let out = PathBuf::from(&args[index + 1]);
        let cwd = std::env::current_dir().expect("fixture cwd");
        if args.iter().any(|arg| arg == "opscatalog-gen") {
            // opscatalog-gen's `--out` is a FILE (opscatalog/src/generated.rs), unlike
            // csharp-client-gen's `--out` DIRECTORY. Writing it as a file (mirroring the
            // committed artifact) makes the freshness diff match -> PASS; treating it as a
            // dir (copy_tree) would create `generated.rs` as a directory, and the stage's
            // `std::fs::read` of a directory fails "Access is denied (os error 5)".
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent).expect("create fake opscatalog out dir");
            }
            let committed = cwd.join("opscatalog/src/generated.rs");
            let bytes = std::fs::read(&committed).unwrap_or_default();
            std::fs::write(&out, bytes).expect("write fake opscatalog generated.rs");
        } else {
            copy_tree(&cwd.join("clients/csharp/Generated"), &out);
        }
    }
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
    if control("advisory-fail")
        && args.iter().any(|arg| arg == "public-api")
        && args.iter().any(|arg| arg == "-p")
    {
        return ExitCode::FAILURE;
    }
    if control("slow-fail")
        && args.iter().any(|arg| arg == "mutants")
        && !args.iter().any(|arg| arg == "--version")
    {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn copy_tree(source: &std::path::Path, target: &std::path::Path) {
    std::fs::create_dir_all(target).expect("create fake generated tree");
    for entry in std::fs::read_dir(source).expect("read generated tree") {
        let path = entry.expect("generated entry").path();
        if path.is_file() {
            std::fs::copy(&path, target.join(path.file_name().unwrap()))
                .expect("copy generated file");
        }
    }
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
    record(&format!(
        "splitproof skip-build {}",
        std::env::var("SPLITPROOF_SKIP_BUILD").unwrap_or_else(|_| "missing".into())
    ));
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
