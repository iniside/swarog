//! `eventctl` — the durable-event-log operator CLI (plan Step 5). Inspect
//! subscriptions and lag, and drive the recovery verbs against the shared
//! `asyncevents` schema over `DATABASE_URL`. All mutating logic lives in the lib
//! ([`eventctl`]); this main is a hand-rolled arg dispatcher (the `tools/` style —
//! no clap) that prints the before/after transition every mutating verb returns, so
//! a checkpoint is never advanced silently.
//!
//!   eventctl list
//!   eventctl lag
//!   eventctl retry   <subscription-id>
//!   eventctl pause   <subscription-id>
//!   eventctl resume  <subscription-id>
//!   eventctl skip    <subscription-id> --reason "<text>"
//!   eventctl retire  <subscription-id>
//!   eventctl bump-generation

use std::process::ExitCode;

use anyhow::{anyhow, Result};
use eventctl::{bump_generation, dsn, info, pause, resume, retire, retry, skip};
use sqlx::PgPool;

const USAGE: &str = "\
eventctl — durable event-log operator CLI

USAGE:
  eventctl list                              list subscriptions (state/cursor/failures/lag)
  eventctl lag                               per-subscription event-count + age lag
  eventctl retry   <id>                      clear failures/backoff, keep the cursor
  eventctl pause   <id>                      stop delivery
  eventctl resume  <id>                      resume delivery
  eventctl skip    <id> --reason <text>      step past the CURRENT failing event only
  eventctl retire  <id>                      mark retired (explicit; excluded from GC floor)
  eventctl bump-generation                   fence the current log generation

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
        .map_err(|e| anyhow!("eventctl: connect {}: {e}", dsn()))?;

    match cmd {
        "list" => cmd_list(&pool).await,
        "lag" => cmd_lag(&pool).await,
        "retry" => print_transition("retry", retry(&pool, &arg_id(&args)?).await?),
        "pause" => print_transition("pause", pause(&pool, &arg_id(&args)?).await?),
        "resume" => print_transition("resume", resume(&pool, &arg_id(&args)?).await?),
        "retire" => print_transition("retire", retire(&pool, &arg_id(&args)?).await?),
        "skip" => cmd_skip(&pool, &args).await,
        "bump-generation" => {
            let (before, after) = bump_generation(&pool).await?;
            println!("bump-generation: generation {before} -> {after}");
            Ok(())
        }
        other => Err(anyhow!("eventctl: unknown command {other:?}\n\n{USAGE}")),
    }
}

/// The subscription id positional: the second token (`eventctl <cmd> <id>`).
fn arg_id(args: &[String]) -> Result<String> {
    args.get(1)
        .filter(|s| !s.starts_with('-'))
        .cloned()
        .ok_or_else(|| anyhow!("eventctl: {} requires a <subscription-id>", args[0]))
}

fn print_transition(
    verb: &str,
    (before, after): (eventctl::StateSnapshot, eventctl::StateSnapshot),
) -> Result<()> {
    println!("{verb} {}:", before.id);
    println!("  before: {}", before.describe());
    println!("  after:  {}", after.describe());
    Ok(())
}

async fn cmd_list(pool: &PgPool) -> Result<()> {
    let subs = info(pool).await?;
    if subs.is_empty() {
        println!("(no subscriptions)");
        return Ok(());
    }
    println!(
        "{:<40} {:<10} {:<24} {:<9} {:>6}  LAST ERROR",
        "SUBSCRIPTION", "STATE", "CURSOR (gen/xid/tie)", "FAILURES", "LAG"
    );
    for s in &subs {
        println!(
            "{:<40} {:<10} {:<24} {:<9} {:>6}  {}",
            s.id,
            s.state,
            s.cursor,
            s.consecutive_failures,
            s.lag_events,
            s.last_error.as_deref().unwrap_or("-"),
        );
    }
    Ok(())
}

async fn cmd_lag(pool: &PgPool) -> Result<()> {
    let subs = info(pool).await?;
    if subs.is_empty() {
        println!("(no subscriptions)");
        return Ok(());
    }
    println!(
        "{:<40} {:<24} {:>10}     LAG AGE (s)",
        "SUBSCRIPTION", "TOPIC", "LAG EVENTS"
    );
    for s in &subs {
        println!(
            "{:<40} {:<24} {:>10} {:>14.1}",
            s.id, s.topic, s.lag_events, s.lag_age_seconds
        );
    }
    Ok(())
}

async fn cmd_skip(pool: &PgPool, args: &[String]) -> Result<()> {
    let id = arg_id(args)?;
    let reason = flag_value(args, "--reason")
        .ok_or_else(|| anyhow!("eventctl: skip requires --reason <text>"))?;
    let outcome = skip(pool, &id, &reason).await?;
    // The skipped event's id + payload go to stderr (an audit trail), the transition
    // to stdout — the checkpoint move is always visible.
    eprintln!(
        "eventctl: SKIPPED event {} on {id} (reason: {reason})\n  payload: {}",
        outcome.skipped_event_id, outcome.skipped_payload
    );
    println!("skip {id}:");
    println!("  before: {}", outcome.before.describe());
    println!("  after:  {}", outcome.after.describe());
    Ok(())
}

/// `--flag value` lookup in the hand-rolled arg vec.
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1).cloned())
}
