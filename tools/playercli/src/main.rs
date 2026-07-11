//! `playercli` drives the QUIC player front (`edge::PlayerServer`, fronted by
//! `gateway-svc`'s or the monolith's `PLAYER_EDGE_ADDR`) for the split-proof script.
//! It loads the trust anchor via `edge::DevCA::load_cert_only` — the CA CERTIFICATE
//! only, never the signing key: the exact material a real player distribution holds
//! (`DevCA::load` demands both because an internal peer must mint leaves; a player
//! must not be able to) — dials `edge::PlayerClient`, fires ONE call, and prints the
//! raw response payload bytes to stdout.
//!
//!   playercli --addr 127.0.0.1:9100 --ca run/edge-ca.crt [--token dev-alice] \
//!       [--api-key dev-key-client] characters.create '{"name":"hero","class":""}'
//!
//! Exit code is the PINNED grammar from the QUIC player-front plan: 0 iff the
//! transport call succeeded (`PlayerClient::call` returned `Ok` — transport `ok:true`)
//! AND the payload JSON's `status` field is exactly `"Ok"`; 1 otherwise — including a
//! transport `Err` (framing/handshake/ALPN fault) AND a transport-`Ok` domain failure
//! (e.g. `{"status":"Unauthorized",...}`, which the pinned error grammar delivers as
//! `ok:true`). Testing `ok` alone would wrongly call an auth failure a success.

use std::process::ExitCode;

use edge::{DevCA, PlayerClient};

fn usage() -> &'static str {
    "playercli --addr 127.0.0.1:9100 --ca <path> [--token <t>] [--api-key <k>] [--repeat <n>] [--interval-ms <ms>] [--pause-before-last-ms <ms>] <method> [json-payload]"
}

struct Args {
    addr: String,
    ca: String,
    token: Option<String>,
    api_key: Option<String>,
    method: String,
    payload: String,
    repeat: usize,
    interval_ms: u64,
    pause_before_last_ms: u64,
}

/// Hand-rolled flag parsing (mirrors `tools/edgeca`'s style): known `--flag value`
/// pairs, then the first two bare tokens are `<method>` and `[json-payload]`.
fn parse_args() -> Result<Args, String> {
    let mut addr = "127.0.0.1:9100".to_string();
    let mut ca = String::new();
    let mut token: Option<String> = None;
    let mut api_key: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut repeat = 1usize;
    let mut interval_ms = 0u64;
    let mut pause_before_last_ms = 0u64;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Err(String::new()), // empty message ⇒ plain usage, not an error
            "--addr" | "-addr" => {
                addr = args.next().ok_or("playercli: --addr requires a value")?
            }
            "--ca" | "-ca" => ca = args.next().ok_or("playercli: --ca requires a value")?,
            "--token" | "-token" => {
                token = Some(args.next().ok_or("playercli: --token requires a value")?)
            }
            "--api-key" | "-api-key" => {
                api_key = Some(args.next().ok_or("playercli: --api-key requires a value")?)
            }
            "--repeat" => repeat = args.next().ok_or("playercli: --repeat requires a value")?
                .parse().map_err(|_| "playercli: --repeat must be a positive integer")?,
            "--interval-ms" => interval_ms = args.next().ok_or("playercli: --interval-ms requires a value")?
                .parse().map_err(|_| "playercli: --interval-ms must be an integer")?,
            "--pause-before-last-ms" => pause_before_last_ms = args.next().ok_or("playercli: --pause-before-last-ms requires a value")?
                .parse().map_err(|_| "playercli: --pause-before-last-ms must be an integer")?,
            other => positional.push(other.to_string()),
        }
    }

    if ca.is_empty() {
        return Err("playercli: --ca is required".to_string());
    }
    let Some(method) = positional.first().cloned() else {
        return Err("playercli: a <method> argument is required".to_string());
    };
    let payload = positional.get(1).cloned().unwrap_or_else(|| "null".to_string());

    if repeat == 0 { return Err("playercli: --repeat must be positive".to_string()); }
    Ok(Args { addr, ca, token, api_key, method, payload, repeat, interval_ms, pause_before_last_ms })
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("{msg}");
            }
            eprintln!("usage: {}", usage());
            return ExitCode::from(2);
        }
    };

    let socket_addr = match args.addr.parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("playercli: bad --addr {:?}: {e}", args.addr);
            return ExitCode::from(2);
        }
    };

    let trust = match DevCA::load_cert_only(&args.ca) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("playercli: load CA cert {:?}: {e}", args.ca);
            return ExitCode::FAILURE;
        }
    };

    let client = match PlayerClient::dial(socket_addr, &trust).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("playercli: dial {}: {e}", args.addr);
            return ExitCode::FAILURE;
        }
    };

    // `PlayerClient::call` already unwraps the transport envelope: `Err` here is a
    // TRANSPORT fault (the peer's `ok:false`); a domain outcome — auth failure
    // included — arrives as `Ok(bytes)` per the pinned error grammar.
    let mut all_ok = true;
    for index in 0..args.repeat {
        if index + 1 == args.repeat && args.pause_before_last_ms != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(args.pause_before_last_ms)).await;
        }
        match client.call(&args.method, args.token.as_deref(), args.api_key.as_deref(), args.payload.as_bytes()).await {
            Ok(resp) => {
                println!("{}", String::from_utf8_lossy(&resp));
                all_ok &= serde_json::from_slice::<serde_json::Value>(&resp).ok()
                    .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(|s| s == "Ok"))
                    .unwrap_or(false);
            }
            Err(e) => {
                eprintln!("playercli: call {}: {e}", args.method);
                all_ok = false;
            }
        }
        if index + 1 < args.repeat && args.interval_ms != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(args.interval_ms)).await;
        }
    }

    if all_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
