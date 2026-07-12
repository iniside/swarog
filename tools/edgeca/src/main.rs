//! `edgeca` mints a local dev CA (cert + key PEM) for the edge hop's mutual TLS and
//! writes it to the given paths (port of Go's `tools/edgeca`). The split run scripts
//! invoke it ONCE, then export `EDGE_CA_CERT` / `EDGE_CA_KEY` at those paths to every
//! edge process so each mints its own leaf under one shared trust anchor. A dev
//! convenience only — never part of a shipped binary.
//!
//!   edgeca --cert run/edge-ca.crt --key run/edge-ca.key

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut cert_path = String::new();
    let mut key_path = String::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cert" | "-cert" => cert_path = args.next().unwrap_or_default(),
            "--key" | "-key" => key_path = args.next().unwrap_or_default(),
            other => {
                eprintln!("edgeca: unknown argument {other:?}");
                return ExitCode::from(2);
            }
        }
    }

    if cert_path.is_empty() || key_path.is_empty() {
        eprintln!("edgeca: both --cert and --key are required");
        return ExitCode::from(2);
    }

    if let Err(e) = edgeca::mint_dev_ca(
        std::path::Path::new(&cert_path),
        std::path::Path::new(&key_path),
    ) {
        eprintln!("edgeca: write CA: {e}");
        return ExitCode::FAILURE;
    }
    println!("edgeca: wrote dev CA cert={cert_path} key={key_path}");
    ExitCode::SUCCESS
}
