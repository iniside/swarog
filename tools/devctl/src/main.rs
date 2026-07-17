use std::process::ExitCode;

use devctl::{cli, supervisor};

fn main() -> ExitCode {
    if let Some(code) = processctl::dispatch_guardian_from_current_exe() {
        return code;
    }
    match cli::parse(std::env::args().skip(1)).and_then(supervisor::execute) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("devctl: {error:#}");
            ExitCode::FAILURE
        }
    }
}
