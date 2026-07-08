//! `playercli` drives the QUIC player front (`edge::PlayerServer`, fronted by
//! `gateway-svc`'s or the monolith's `PLAYER_EDGE_ADDR`) for the split-proof script.
//! It loads the trust anchor via `edge::DevCA::load_cert_only` — the CA CERTIFICATE
//! only, never the signing key: the exact material a real player distribution holds
//! (`DevCA::load` demands both because an internal peer must mint leaves; a player
//! must not be able to) — dials `edge::PlayerClient`, fires ONE call, and prints the
//! raw response payload bytes to stdout.
//!
//!   playercli --addr 127.0.0.1:9100 --ca run/edge-ca.crt [--token dev-alice] \
//!       characters.create '{"name":"hero","class":""}'
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
    "playercli --addr 127.0.0.1:9100 --ca <path> [--token <t>] <method> [json-payload]"
}

struct Args {
    addr: String,
    ca: String,
    token: Option<String>,
    method: String,
    payload: String,
}

/// Hand-rolled flag parsing (mirrors `tools/edgeca`'s style): known `--flag value`
/// pairs, then the first two bare tokens are `<method>` and `[json-payload]`.
fn parse_args() -> Result<Args, String> {
    let mut addr = "127.0.0.1:9100".to_string();
    let mut ca = String::new();
    let mut token: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();

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

    Ok(Args { addr, ca, token, method, payload })
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
    let resp = match client.call(&args.method, args.token.as_deref(), args.payload.as_bytes()).await {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("playercli: call {}: {e}", args.method);
            return ExitCode::FAILURE;
        }
    };

    let body = String::from_utf8_lossy(&resp);
    println!("{body}");

    let status_ok = serde_json::from_slice::<serde_json::Value>(&resp)
        .ok()
        .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(|s| s == "Ok"))
        .unwrap_or(false);

    if status_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
