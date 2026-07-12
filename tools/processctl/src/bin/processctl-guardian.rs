#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    let code = processctl::run_guardian();
    std::process::ExitCode::from(u8::try_from(code).unwrap_or(1))
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("processctl-guardian is supported only on Linux");
    std::process::ExitCode::FAILURE
}
