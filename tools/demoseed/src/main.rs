use std::process::ExitCode;

use demoseed::{parse_args, run, Command, USAGE};

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config = match parse_args(&args) {
        Ok(Command::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Command::Seed(config)) => config,
        Err(err) => {
            eprintln!("{err:#}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "demoseed: seeding {} players and {} matches, then {} client scenarios, through {} ({} requests in flight at most)",
        config.players, config.matches, config.clients, config.base_url, config.concurrency
    );
    match run(&config).await {
        Ok(report) => {
            let rendered = report.render(&config.base_url, opscatalog::OPERATIONS);
            print!("{}", rendered.text);
            if rendered.failed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(err) => {
            eprintln!("{err:#}");
            ExitCode::FAILURE
        }
    }
}
