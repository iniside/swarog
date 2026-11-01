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
        "demoseed: seeding {} players and {} matches through {}",
        config.players, config.matches, config.base_url
    );
    match run(&config).await {
        Ok(summary) => {
            print!("{}", summary.render(&config.base_url));
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("demoseed: {err:#}");
            ExitCode::FAILURE
        }
    }
}
