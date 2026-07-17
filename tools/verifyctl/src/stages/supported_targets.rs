//! `supported-targets` — a BLOCKING verify stage that typechecks the two
//! rollout-tooling crates against every platform this repo PROMISES compiles.
//!
//! This exists because there is **no CI** (`README.md`: "No CI: `cargo run -p
//! verifyctl -- --fast` is the safety net"). That is exactly why weles's
//! non-Linux control-endpoint fallback rotted to the wrong arity (an E0061 that
//! nothing compiled) and why the macOS port was a rescue rather than a routine
//! build: no stage ever cross-checked a target the dev box does not run on. A
//! verifyctl stage is the ONLY tripwire mechanism this repo has, so this is it —
//! `cargo check --target <t>` typechecks a target's `cfg` arms WITHOUT linking,
//! so E0061-class rot is caught from Windows or Linux without owning a Mac.
//!
//! Modeled on `weles_async_island`: BLOCKING, cargo-driven, builds no fleet,
//! touches no Postgres, and carries the two house habits — an always-on positive
//! control (below) and no green-on-broken-tooling (a missing target is a FAIL,
//! never a SKIP — the `b78444f` cargo-audit scar).
//!
//! ## Scope: `processctl` + `weles` ONLY — NOT `--workspace`
//!
//! `cargo check --target` RUNS build scripts, and `devctl`/`verifyctl`/`edgeca`
//! pull `ring` (via `edge` → `quinn`), whose C/asm build needs a per-target
//! cross-cc a normal box does not have. `processctl` and `weles` are ring-free in
//! their NORMAL deps (`cargo tree -p <c> -e normal` → 0 ring) — and they are
//! precisely the two crates that rotted. **devctl is a NAMED, DELIBERATE gap:**
//! its `ring` edge makes it uncross-checkable from a box without an Apple
//! cross-toolchain, so it is excluded rather than made to green-fail. If devctl
//! ever sheds `ring`, add it here.
//!
//! ## No `--all-targets` — another named gap
//!
//! `processctl` is ring-free only in NORMAL deps; its `[dev-dependencies]`
//! (`asyncevents`/`invalidation`/`scheduler`, the anti-drift mirrors) pull `ring`
//! back in. `cargo check --target` WITHOUT `--all-targets` does not build
//! dev-deps. The cost, stated rather than hidden: **test code is not typechecked
//! cross-target**, so rot inside a `cfg`-gated test module (e.g.
//! `control_tests.rs`) escapes this stage. Named gap, not silence.

use crate::{model::Outcome, runner::Context};
use anyhow::{Context as _, Result};
use std::collections::BTreeSet;

/// The platforms this repo PROMISES compile — the single source of truth,
/// mirrored by `rust-toolchain.toml`'s `targets` (a test below pins them equal,
/// so the declarative provisioning and the checked promise can never drift).
///
/// `x86_64-pc-windows-gnu` is used rather than `-msvc` deliberately: the `-gnu`
/// target cross-checks WITHOUT an MSVC linker, so this stage runs from a Mac or
/// Linux box. The real Windows dev box is `-msvc`; a `cargo check` (typeck, no
/// link) of the `-gnu` triple exercises the same `cfg(windows)` arms, which is
/// the rot surface this stage guards.
pub const SUPPORTED_TARGETS: &[&str] = &[
    "aarch64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-gnu",
];

/// The crates cross-checked. See the module doc for why only these two.
const CHECKED_CRATES: &[&str] = &["processctl", "weles"];

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    let mut findings: Vec<String> = Vec::new();
    for &target in SUPPORTED_TARGETS {
        findings.extend(check_target(ctx, target)?);
    }

    if findings.is_empty() {
        return Ok(Outcome::Pass);
    }
    eprintln!(
        "verifyctl: supported-targets violations ({} finding(s)):",
        findings.len()
    );
    for finding in &findings {
        eprintln!("  {finding}");
        ctx.note(finding)?;
    }
    Ok(Outcome::Fail)
}

/// `cargo check -p processctl -p weles --target <t> --message-format=json`,
/// scored two ways that must BOTH hold:
///
/// 1. cargo exits zero — the crates typecheck for `t`. A non-zero exit is the
///    primary rot-catch (the E0061 the stage exists for) and is a FAIL, not a
///    SKIP: `rust-toolchain.toml` provisions a missing target automatically, so
///    if the check still fails, the promise is genuinely broken.
/// 2. the positive control (below) confirms a target-specific artifact actually
///    materialized — so a silently-ignored `--target`, a fully-cached no-op, or a
///    drift in cargo's JSON shape cannot leave the stage green while it checked
///    nothing (the always-on house habit, mirroring `weles_async_island:84`).
fn check_target(ctx: &mut Context<'_>, target: &str) -> Result<Vec<String>> {
    let label = format!("check-{target}");
    let outcome = ctx.cargo(
        &label,
        &[
            "check",
            "-p",
            "processctl",
            "-p",
            "weles",
            "--target",
            target,
            "--message-format=json",
        ],
    )?;
    let log = ctx.stage_log(&label, "out");

    if outcome != Outcome::Pass {
        return Ok(vec![format!(
            "`cargo check -p processctl -p weles --target {target}` did NOT pass \
             ({outcome}) — a promised platform does not compile (see {}). This is the \
             E0061-class rot this stage exists to catch. A missing rustup target is \
             auto-provisioned via rust-toolchain.toml; if it still fails, that is a \
             FAIL, never a skip.",
            log.display()
        )]);
    }

    let json = std::fs::read_to_string(&log)
        .with_context(|| format!("read cargo check json output {}", log.display()))?;
    Ok(positive_control_findings(&json, target))
}

/// The always-on positive control: which of [`CHECKED_CRATES`] failed to produce
/// a compiler artifact built FOR `target`. Empty means the run genuinely checked
/// both crates against the requested triple.
fn positive_control_findings(json: &str, target: &str) -> Vec<String> {
    let built = target_specific_artifacts(json, target);
    CHECKED_CRATES
        .iter()
        .filter(|crate_name| !built.contains(**crate_name))
        .map(|crate_name| {
            format!(
                "supported-targets positive control: `{crate_name}` produced no compiler \
                 artifact under a `/{target}/` path for `cargo check --target {target}`. \
                 Either --target was silently ignored (the artifact landed in the host \
                 target dir), the crate did not actually compile for that target, or \
                 cargo's --message-format=json shape changed — any of which would leave \
                 this stage green while checking nothing."
            )
        })
        .collect()
}

/// The set of [`CHECKED_CRATES`] whose `compiler-artifact` message carries at
/// least one output filename under a `/<target>/` path component.
///
/// The triple in the artifact path is the load-bearing signal: `cargo check
/// --target X` writes to `target/X/...` even when `X` is the host, so an artifact
/// under `/X/` PROVES the crate was compiled for `X` and not the host. A
/// `fresh: true` (fully-cached) artifact still carries the same path, so a cached
/// tree passes while STILL asserting a real target-specific artifact — verified
/// against live cargo output.
fn target_specific_artifacts(json: &str, target: &str) -> BTreeSet<String> {
    let mut built = BTreeSet::new();
    for line in json.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
            continue;
        }
        let Some(name) = value
            .get("target")
            .and_then(|t| t.get("name"))
            .and_then(|n| n.as_str())
        else {
            continue;
        };
        if !CHECKED_CRATES.contains(&name) {
            continue;
        }
        let target_specific = value
            .get("filenames")
            .and_then(|f| f.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .any(|filename| contains_target_path(filename, target));
        if target_specific {
            built.insert(name.to_string());
        }
    }
    built
}

/// Whether `filename` contains `target` as a path component, tolerant of the
/// path separator (a Windows host reports `\`, a unix host reports `/`).
fn contains_target_path(filename: &str, target: &str) -> bool {
    filename.replace('\\', "/").contains(&format!("/{target}/"))
}

#[cfg(test)]
#[path = "supported_targets_tests.rs"]
mod supported_targets_tests;
