use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use admindiff::{crawl, parse_args, repo_root, run_diff, run_mock, target_dir, Command, USAGE};

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match parse_args(&args) {
        Ok(Command::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(c) => c,
        Err(err) => {
            eprintln!("{err:#}");
            return ExitCode::FAILURE;
        }
    };
    let repo = repo_root();
    let result = match command {
        Command::Help => unreachable!("handled above"),
        Command::Crawl { base_url, user, allow_errors, allow_empty } => {
            if let Err(err) = admindiff::validated_mockup(&repo) {
                eprintln!("admindiff: {err:#}");
                return ExitCode::FAILURE;
            }
            let run = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default();
            let cfg = crawl::Config {
                base_url,
                username: user,
                password: std::env::var(admindiff::PASSWORD_ENV).unwrap_or_else(|_| admindiff::DEFAULT_PASSWORD.to_string()),
                allow_errors,
                allow_empty,
                out: target_dir(&repo).join(format!("crawl-{run}")),
            };
            crawl::run(&cfg).await
        }
        Command::Mock { page } => run_mock(&repo, &page).map(|()| true),
        Command::Diff { page, portal, text } => run_diff(&repo, &page, &portal, text),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("admindiff: {err:#}");
            ExitCode::FAILURE
        }
    }
}
