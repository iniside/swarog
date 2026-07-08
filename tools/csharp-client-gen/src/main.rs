//! `csharp-client-gen` — the external C# player-client generator (Step 2 = the scraper
//! half). It scrapes the player-reachable API into an internal typed [`model::Manifest`]
//! and can dump it as JSON (`--emit-manifest`), cross-checking the runtime allow-list
//! against the parsed source via two hard gates (see [`scrape`]).
//!
//! The C# emitter (`--out <dir>`) is Step 3 — a clearly-marked stub here.
//!
//! ```text
//! csharp-client-gen --emit-manifest [path]   # dump the manifest as pretty JSON
//! csharp-client-gen --out <dir>              # emit the typed C# client into <dir>
//! ```

mod emit;
mod model;
mod scrape;

use anyhow::{anyhow, Result};

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("csharp-client-gen: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut manifest_path: Option<String> = None;
    let mut out_dir: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--emit-manifest" => {
                // Optional path argument (a non-flag token).
                if let Some(next) = args.get(i + 1) {
                    if !next.starts_with("--") {
                        manifest_path = Some(next.clone());
                        i += 1;
                    }
                }
            }
            "--out" => {
                out_dir = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| anyhow!("--out requires a directory argument"))?,
                );
                i += 1;
            }
            "-h" | "--help" => {
                print_usage();
                return Ok(());
            }
            other => return Err(anyhow!("unknown argument {other:?} (see --help)")),
        }
        i += 1;
    }

    // The scrape runs both gates; a gate failure surfaces as an `Err` → nonzero exit.
    let manifest = scrape::scrape()?;

    if let Some(dir) = out_dir {
        emit::emit(&manifest, std::path::Path::new(&dir))?;
        return Ok(());
    }

    // Default action: dump the manifest as pretty JSON.
    let json = serde_json::to_string_pretty(&manifest)?;
    match manifest_path {
        Some(path) => std::fs::write(&path, json)?,
        None => println!("{json}"),
    }
    Ok(())
}

fn print_usage() {
    eprintln!(
        "csharp-client-gen — external C# player-client generator (Step 2: scraper)\n\
         \n\
         USAGE:\n\
         \x20 csharp-client-gen --emit-manifest [path]   dump the scraped manifest as pretty JSON\n\
         \x20 csharp-client-gen --out <dir>              emit the typed C# client into <dir>\n\
         \n\
         With no arguments, behaves like --emit-manifest to stdout."
    );
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
