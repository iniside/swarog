//! `eventctl` — the durable-event-log operator CLI (plan Step 5). Inspect
//! subscriptions and lag, and drive the recovery verbs against the shared
//! `asyncevents` schema over `DATABASE_URL`. All mutating logic lives in
//! `asyncevents::admin`; this main is a hand-rolled arg dispatcher (the `tools/` style —
//! no clap) that prints the before/after transition every mutating verb returns, so
//! a checkpoint is never advanced silently. Retirement is permanent: retry, pause,
//! resume and skip refuse a retired subscription. `seed-mockup` is the one dev fixture
//! verb: it writes the admin mockup's paused and lagging rows ([`eventctl::seed`]).
//!
//!   eventctl list
//!   eventctl lag
//!   eventctl retry   <subscription-id>
//!   eventctl pause   <subscription-id>
//!   eventctl resume  <subscription-id>
//!   eventctl skip    <subscription-id> --reason "<text>"
//!   eventctl retire  <subscription-id>
//!   eventctl bump-generation
//!   eventctl seed-mockup

use std::process::ExitCode;

use anyhow::{anyhow, Result};
use asyncevents::admin::{
    bump_generation, info, pause, resume, retire, retry, skip, snapshot, Retired,
    SkipResult, StateSnapshot,
};
use eventctl::dsn;
use eventctl::seed::{seed_mockup, LAGGING_ID, PAUSED_ID};
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
  eventctl retire  <id>                      mark retired (permanent; excluded from GC floor)
  eventctl bump-generation                   fence the current log generation
  eventctl seed-mockup                       dev only: reset the admin mockup's paused
                                             and lagging rows (seed.mockup-paused.v1,
                                             seed.mockup-lagging.v1) on the busiest topic

Retry, pause, resume and skip refuse a retired subscription.

seed-mockup needs at least two events on the busiest topic (run demoseed first) and
resets both rows from any state, retired included. The lagging row reads the topic
from its first event; the paused row's poison event is the topic's middle event, so
its lag is below the lagging row's. No process consumes them. They hold the topic's
retention floor until retired or the database is wiped.

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
        "retry" => {
            let moved = retry(&pool, &arg_id(&args)?).await.map_err(refused("retry"))?;
            print_transition("retry", moved)
        }
        "pause" => {
            let moved = pause(&pool, &arg_id(&args)?).await.map_err(refused("pause"))?;
            print_transition("pause", moved)
        }
        "resume" => {
            let moved = resume(&pool, &arg_id(&args)?).await.map_err(refused("resume"))?;
            print_transition("resume", moved)
        }
        "retire" => print_transition("retire", retire(&pool, &arg_id(&args)?).await?),
        "skip" => cmd_skip(&pool, &args).await,
        "bump-generation" => {
            let (before, after) = bump_generation(&pool).await?;
            println!("bump-generation: generation {before} -> {after}");
            Ok(())
        }
        "seed-mockup" => cmd_seed_mockup(&pool).await,
        other => Err(anyhow!("eventctl: unknown command {other:?}\n\n{USAGE}")),
    }
}

/// Names a [`Retired`] refusal as the verb's refusal; other errors pass through.
fn refused(verb: &'static str) -> impl FnOnce(anyhow::Error) -> anyhow::Error {
    move |err| match err.downcast_ref::<Retired>() {
        Some(_) => err.context(format!("eventctl: {verb} refused; retirement is permanent")),
        None => err,
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
    (before, after): (StateSnapshot, StateSnapshot),
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
        "{:<40} {:<10} {:<24} {:<9} {:>6}  {:<26} LAST ERROR",
        "SUBSCRIPTION", "STATE", "CURSOR (gen/xid/tie)", "FAILURES", "LAG", "PAUSED SINCE"
    );
    for s in &subs {
        println!(
            "{:<40} {:<10} {:<24} {:<9} {:>6}  {:<26} {}",
            s.id,
            s.state,
            s.cursor,
            s.consecutive_failures,
            s.lag_events,
            s.paused_since.as_deref().unwrap_or(""),
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
    let result = skip(pool, &id, &reason, None).await.map_err(refused("skip"))?;
    let SkipResult::Skipped(outcome) = result else {
        // `expected: None` turns every nothing-to-skip case into an error instead.
        return Err(anyhow!("eventctl: skip of {id:?} moved nothing"));
    };
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

async fn cmd_seed_mockup(pool: &PgPool) -> Result<()> {
    let seeded = seed_mockup(pool).await?;
    println!(
        "seed-mockup: topic {} v{} ({} events)",
        seeded.topic, seeded.version, seeded.events
    );
    for id in [PAUSED_ID, LAGGING_ID] {
        println!("  {id}: {}", snapshot(pool, id).await?.describe());
    }
    Ok(())
}

/// `--flag value` lookup in the hand-rolled arg vec.
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1).cloned())
}
