use std::process::ExitCode;

fn main() -> ExitCode {
    if let Some(code) = processctl::dispatch_guardian_from_current_exe() {
        return code;
    }
    let options = match verifyctl::cli::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("verifyctl: {error:#}");
            eprintln!("{}", verifyctl::cli::USAGE);
            return ExitCode::from(verifyctl::Exit::Orchestration as u8);
        }
    };
    match verifyctl::execute(options) {
        Ok(exit) => ExitCode::from(exit as u8),
        Err(error) => {
            eprintln!("verifyctl: orchestration error: {error:#}");
            ExitCode::from(verifyctl::Exit::Orchestration as u8)
        }
    }
}
