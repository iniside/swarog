//! `weles-async-island` — a BLOCKING verify stage guarding the two constraints
//! on weles's tokio island (`weles/src/agentapi.rs`) that weles CANNOT guard
//! from inside itself.
//!
//! Both checks are here for the same structural reason the other `weles-*`
//! stages exist: **a cross-cutting claim about weles cannot live inside weles.**
//! Zero-sharing means weles imports no workspace crate, so from its own test
//! suite weles can see neither `core/app`'s tokio features nor `stages::csharp`'s
//! port. verifyctl sees everything (it already depends on `weles`), and cargo is
//! a verify stage's house tool, so:
//!
//! * no nested cargo — these `cargo tree` calls would otherwise run INSIDE
//!   `cargo test --workspace` (weles is a workspace member), i.e. inside
//!   verifyctl's own blocking `test` stage and its rollout lease. It happens not
//!   to deadlock on cargo 1.96 (the CacheLocker rework scoped the package-cache
//!   lock to the download phase), but that is an implementation detail, not a
//!   contract — pre-1.7x behaviour is precisely what made nested cargo hang, and
//!   a reader enforcing "one cargo at a time" cannot tell the difference from
//!   the code.
//! * no coupling — as a weles unit test, the workspace-wide `process` ban would
//!   fail inside *weles's own suite* the day someone legitimately edits
//!   `core/app`'s feature list. That is a verify concern, and it now fails in a
//!   verify stage.
//!
//! Neither check builds anything or touches Postgres: `cargo tree` resolves from
//! the lockfile.

use crate::{
    model::Outcome,
    runner::Context,
    stages::csharp,
};
use anyhow::{Context as _, Result};

/// Tokio features that must never reach weles's own resolve.
///
/// `process` installs a SIGCHLD handler that reaps children out from under
/// `weles::platform::OwnedProc::try_wait` — the sole authority for
/// `Observed::Exited`, "the process is gone" — which under async is
/// indistinguishable from "connection refused". Arming it turns a Postgres blip
/// into a fleet-wide restart storm. `signal` would fight the raw
/// `libc::signal(SIGINT)` handler in `weles::supervisor`; last writer wins.
const BANNED_FOR_WELES: &[&str] = &["process", "signal"];

/// Tokio features that must never be armed ANYWHERE in the workspace.
///
/// Narrower than [`BANNED_FOR_WELES`] on purpose — see [`run`]'s asymmetry note.
const BANNED_WORKSPACE_WIDE: &[&str] = &["process"];

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    let mut findings: Vec<String> = Vec::new();
    findings.extend(weles_tokio_findings(ctx)?);
    findings.extend(workspace_tokio_findings(ctx)?);
    findings.extend(agent_port_findings());

    if findings.is_empty() {
        return Ok(Outcome::Pass);
    }
    eprintln!(
        "verifyctl: weles async-island violations ({} finding(s)):",
        findings.len()
    );
    for finding in &findings {
        eprintln!("  {finding}");
        ctx.note(finding)?;
    }
    Ok(Outcome::Fail)
}

/// weles's OWN resolve must carry neither banned feature.
///
/// `--target all` so a Windows run still covers the Unix-only SIGCHLD mine.
fn weles_tokio_findings(ctx: &mut Context<'_>) -> Result<Vec<String>> {
    let tree = cargo_tree(
        ctx,
        "tokio-features-weles",
        &["tree", "-e", "features", "-p", "weles", "--target", "all"],
    )?;

    // Fail-proof for this CHECK: if cargo's feature rendering ever changes, the
    // bans below would match nothing and pass forever. `net` is a feature weles
    // demonstrably enables, so its presence proves the pattern shape is live.
    if !tree.contains(r#"tokio feature "net""#) {
        return Ok(vec![
            "cargo tree's feature rendering changed: `tokio feature \"net\"` is absent from \
             weles's own tree, so the process/signal bans are no longer checking anything"
                .to_string(),
        ]);
    }
    Ok(BANNED_FOR_WELES
        .iter()
        .filter(|feature| tree.contains(&feature_edge(feature)))
        .map(|feature| {
            format!(
                "weles's tokio resolved WITH the banned `{feature}` feature — the async island \
                 (weles/src/agentapi.rs) may never own process or signal handling"
            )
        })
        .collect())
}

/// The scope `-p weles` cannot reach: a SIBLING crate arming `process`.
///
/// `cargo tree -p weles` resolves features for weles's own selection, so it
/// would not see it — yet `cargo build --workspace` unifies features across
/// selected packages, which is exactly how the mine gets armed inside the weles
/// binary. Resolver-2 unification is why `weles/Cargo.toml` is not the authority
/// on what weles's tokio actually gets, and why this is a test and not a comment.
///
/// KNOWN, DELIBERATE ASYMMETRY: `signal` is NOT banned here. `core/app` owns it
/// legitimately (`tokio::signal` for graceful shutdown), so a workspace-wide
/// `signal` ban is impossible; it is banned for weles's own resolve only.
fn workspace_tokio_findings(ctx: &mut Context<'_>) -> Result<Vec<String>> {
    let tree = cargo_tree(
        ctx,
        "tokio-features-workspace",
        &["tree", "-e", "features", "--workspace", "--target", "all", "-i", "tokio"],
    )?;

    // Fail-proof AND the asymmetry's positive control in one: core/app's
    // `signal` MUST be visible here. If it is not, either the rendering changed
    // or the inverted tree stopped covering the workspace — and the `process`
    // ban below would be checking nothing.
    if !tree.contains(&feature_edge("signal")) {
        return Ok(vec![
            "expected core/app's `tokio feature \"signal\"` in the workspace-wide inverted \
             tree; its absence means this check can no longer see workspace features, so the \
             `process` ban is not checking anything"
                .to_string(),
        ]);
    }
    Ok(BANNED_WORKSPACE_WIDE
        .iter()
        .filter(|feature| tree.contains(&feature_edge(feature)))
        .map(|feature| {
            format!(
                "a workspace crate enabled tokio's `{feature}` feature. Resolver-2 unifies \
                 features across the build graph, so this arms it inside the weles binary too: \
                 tokio::process reaps children out from under weles::platform's try_wait, the \
                 sole authority for Observed::Exited. Remove it, or move that crate's \
                 subprocess work off tokio::process."
            )
        })
        .collect())
}

/// `weles::manifest::AGENT_PORT` against every port this workspace claims that
/// weles cannot see.
///
/// weles's own suite derives the FLEET's ports from its manifest, but weles can
/// only ever see its own fleet — the one place this port was never going to
/// collide. The C# fixture is the gap that motivated the check: it binds 8099
/// and fails when occupied, so sharing a port means a leftover fixture makes
/// `weles up` die naming the wrong culprit (and vice versa). Not a live race
/// (both hold `run/rollout.lock`) — a diagnosis trap.
fn agent_port_findings() -> Vec<String> {
    let agent = weles::manifest::AGENT_PORT;
    [
        (csharp::HTTP_PORT, "the C# fixture server's HTTP port (stages/csharp.rs)"),
        (csharp::PLAYER_PORT, "the C# fixture server's player-QUIC port (stages/csharp.rs)"),
    ]
    .iter()
    .filter(|(port, _)| *port == agent)
    .map(|(port, what)| {
        format!(
            "weles::manifest::AGENT_PORT ({agent}) collides with {what} ({port}) — pick a port \
             nothing in this repo claims, so a leftover listener never makes either tool report \
             the wrong culprit"
        )
    })
    .collect()
}

/// How `cargo tree -e features` renders a feature edge.
fn feature_edge(feature: &str) -> String {
    format!(r#"tokio feature "{feature}""#)
}

/// Runs `cargo tree` through the runner (so its output is preserved as an
/// ordinary stage log) and returns its stdout.
///
/// A non-zero exit is an ERROR, never an empty-string "no findings": a checker
/// that reports green because its own tooling broke is exactly the failure this
/// repo has a scar from.
fn cargo_tree(ctx: &mut Context<'_>, label: &str, args: &[&str]) -> Result<String> {
    let outcome = ctx.cargo(label, args)?;
    let log = ctx.stage_log(label, "out");
    if outcome != Outcome::Pass {
        anyhow::bail!(
            "cargo tree failed ({outcome:?}) — see {}; the tokio feature bans could not be \
             evaluated, which is a FAILURE, not an absence of findings",
            log.display()
        );
    }
    std::fs::read_to_string(&log).with_context(|| format!("read cargo tree output {}", log.display()))
}

#[cfg(test)]
#[path = "weles_async_island_tests.rs"]
mod weles_async_island_tests;
