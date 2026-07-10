//! `adminctl` — the admin-user operator CLI (plan Step 3). Mint / list / delete the
//! GameOps admin logins in the shared `admin` schema over `DATABASE_URL`. All mutating
//! logic lives in the lib ([`adminctl`]); this main is a hand-rolled arg dispatcher
//! (the `tools/` style — no clap). Passwords are NEVER read from argv: `--password-stdin`
//! reads one trimmed line from stdin, or `ADMINCTL_PASSWORD` supplies it — the root
//! `install.sh`/`install.ps1` wrappers do the no-echo prompting and pipe the answer in.
//!
//!   adminctl create-user <username> [--password-stdin]
//!   adminctl list
//!   adminctl delete <username>

use std::io::Read as _;
use std::process::ExitCode;

use adminctl::{create_user, delete_user, dsn, list_users};
use anyhow::{anyhow, Result};
use sqlx::PgPool;

const USAGE: &str = "\
adminctl — admin-user operator CLI

USAGE:
  adminctl create-user <username> [--password-stdin]   mint or password-reset an admin login
  adminctl list                                         list admin users (username + created_at)
  adminctl delete <username>                            remove an admin login

Password (never via argv):
  --password-stdin       read one trimmed line from stdin
  ADMINCTL_PASSWORD env  used when --password-stdin is absent
  (provide exactly one; an empty password is rejected)

Connection: DATABASE_URL (default local dev DSN).";

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    if matches!(cmd, "" | "-h" | "--help" | "help") {
        println!("{USAGE}");
        return Ok(());
    }

    let pool = PgPool::connect(&dsn())
        .await
        .map_err(|e| anyhow!("adminctl: connect {}: {e}", dsn()))?;

    match cmd {
        "create-user" => cmd_create_user(&pool, &args).await,
        "list" => cmd_list(&pool).await,
        "delete" => cmd_delete(&pool, &args).await,
        other => Err(anyhow!("adminctl: unknown command {other:?}\n\n{USAGE}")),
    }
}

/// The username positional: the second token (`adminctl <cmd> <username>`).
fn arg_username(args: &[String], verb: &str) -> Result<String> {
    args.get(1)
        .filter(|s| !s.starts_with('-'))
        .cloned()
        .ok_or_else(|| anyhow!("adminctl: {verb} requires a <username>"))
}

/// Resolve the password from the two allowed channels: `--password-stdin` (one trimmed
/// line) XOR `ADMINCTL_PASSWORD`. Never from argv. Errors clearly when neither is given.
fn resolve_password(args: &[String]) -> Result<String> {
    let stdin_flag = args.iter().any(|a| a == "--password-stdin");
    let env_pw = std::env::var("ADMINCTL_PASSWORD").ok();
    match (stdin_flag, env_pw) {
        (true, _) => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| anyhow!("adminctl: read password from stdin: {e}"))?;
            // One trimmed line (drop the trailing newline the wrapper pipes in, plus any
            // surrounding whitespace). A blank line is rejected downstream.
            Ok(buf.trim().to_string())
        }
        (false, Some(pw)) => Ok(pw),
        (false, None) => Err(anyhow!(
            "adminctl: no password supplied — pass --password-stdin (one line on stdin) \
             or set ADMINCTL_PASSWORD (use ./install.sh for a no-echo prompt)"
        )),
    }
}

async fn cmd_create_user(pool: &PgPool, args: &[String]) -> Result<()> {
    let username = arg_username(args, "create-user")?;
    let password = resolve_password(args)?;
    let inserted = create_user(pool, &username, &password).await?;
    if inserted {
        println!("created admin user {username:?}");
    } else {
        println!("reset password for existing admin user {username:?}");
    }
    Ok(())
}

async fn cmd_list(pool: &PgPool) -> Result<()> {
    let users = list_users(pool).await?;
    if users.is_empty() {
        println!("(no admin users — run ./install.sh <username> to create one)");
        return Ok(());
    }
    println!("{:<32} CREATED AT", "USERNAME");
    for u in &users {
        println!("{:<32} {}", u.username, u.created_at);
    }
    Ok(())
}

async fn cmd_delete(pool: &PgPool, args: &[String]) -> Result<()> {
    let username = arg_username(args, "delete")?;
    if delete_user(pool, &username).await? {
        println!("deleted admin user {username:?}");
    } else {
        println!("no such admin user {username:?}");
    }
    Ok(())
}
