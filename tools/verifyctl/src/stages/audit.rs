use std::ffi::OsString;

use anyhow::{Context as _, Result};

use crate::model::{Outcome, SkipReason};
use crate::runner::Context;

/// The ignored advisory whose justification is that its crate is lock-only.
const SQLITE_ADVISORY: &str = "RUSTSEC-2026-0459";
const SQLITE_CRATE: &str = "libsqlite3-sys";
const SQLITE_PREMISE_LABEL: &str = "sqlite-premise";
const SQLITE_PREMISE_ARGS: &[&str] = &[
    "tree",
    "-i",
    SQLITE_CRATE,
    "--workspace",
    "--target",
    "all",
    "-e",
    "all",
    "--locked",
];

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    let mut executable = ctx.resolve("cargo-audit");
    if executable.is_none() {
        if !ctx.options.install {
            ctx.note("cargo-audit is missing and installation was explicitly disabled")?;
            return Ok(Outcome::Skip(SkipReason::ExplicitNoInstallMissingTool));
        }
        let installed = match ctx.cargo("audit-install", &["install", "cargo-audit", "--locked"]) {
            Ok(outcome) => outcome,
            Err(error) => {
                ctx.note(&format!(
                    "cargo-audit installation invocation failed: {error:#}"
                ))?;
                return Ok(Outcome::Fail);
            }
        };
        executable = ctx.resolve("cargo-audit");
        if installed != Outcome::Pass || executable.is_none() {
            return Ok(Outcome::Fail);
        }
    }
    if let Some(finding) = sqlite_premise_finding(ctx)? {
        eprintln!("verifyctl: audit: {finding}");
        ctx.note(&finding)?;
        return Ok(Outcome::Fail);
    }
    match ctx.command(
        "audit",
        executable.expect("resolved above"),
        vec![
            OsString::from("audit"),
            // rsa 0.9.10 (Marvin timing side channel): `cargo tree -i rsa --workspace
            // --target all -e normal,build --locked` shows only `tools/splitproof` as a
            // dependent, so sqlx-mysql does not pull it into any build. Its two users
            // (`tools/splitproof` and `modules/accounts` as a dev-dependency) only generate
            // throwaway 2048-bit keys and export n/e plus PKCS#8 PEM for JWKS fixtures; the
            // signing is jsonwebtoken via ring, and production OIDC verification
            // (`modules/accounts/src/oidc.rs`) is public-key only and never touches `rsa`.
            // Revisit if `rsa` is used for decryption or signing, or a patched release lands.
            OsString::from("--ignore"),
            OsString::from("RUSTSEC-2023-0071"),
            // libsqlite3-sys is in Cargo.lock only through sqlx's weak
            // `sqlx-sqlite?/…` feature entries and is never compiled;
            // `sqlite_premise_finding` fails the stage once that stops being
            // true. The workspace is on sqlx 0.8, whose sqlx-sqlite requires libsqlite3-sys
            // ^0.30.1; the latest release, 0.9.0, still caps it at <0.38, and the fix needs
            // >=0.39. Remove this ignore once a released sqlx allows >=0.39.
            OsString::from("--ignore"),
            OsString::from(SQLITE_ADVISORY),
        ],
    ) {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            ctx.note(&format!("cargo-audit invocation failed: {error:#}"))?;
            Ok(Outcome::Fail)
        }
    }
}

/// Runs `cargo tree -i libsqlite3-sys` and returns why the sqlite ignore is no
/// longer justified, or `None` when the crate is in the lock but not compiled.
fn sqlite_premise_finding(ctx: &mut Context<'_>) -> Result<Option<String>> {
    let outcome = match ctx.cargo(SQLITE_PREMISE_LABEL, SQLITE_PREMISE_ARGS) {
        Ok(outcome) => outcome,
        Err(error) => {
            return Ok(Some(format!(
                "cargo tree invocation failed, so the {SQLITE_ADVISORY} ignore cannot be \
                 justified: {error:#}"
            )));
        }
    };
    let stdout = read_log(ctx, "out")?;
    let stderr = read_log(ctx, "err")?;
    Ok(premise_finding(outcome == Outcome::Pass, &stdout, &stderr))
}

/// Classifies `cargo tree -i libsqlite3-sys --locked` as observed: a lock-only
/// crate exits 0 with empty stdout and "nothing to print" on stderr; a
/// compiled crate exits 0 and prints its inverted tree; a crate absent
/// from the lock exits non-zero with "did not match any packages". Anything
/// else is a cargo failure and never reads as "not compiled".
fn premise_finding(passed: bool, stdout: &str, stderr: &str) -> Option<String> {
    if passed {
        if !stdout.trim().is_empty() {
            return Some(format!(
                "{SQLITE_CRATE} is compiled; the {SQLITE_ADVISORY} ignore is no longer justified"
            ));
        }
        if stderr.contains("nothing to print") {
            return None;
        }
        return Some(format!(
            "cargo tree -i {SQLITE_CRATE} printed nothing without its \"nothing to print\" \
             warning, so the {SQLITE_ADVISORY} ignore cannot be justified"
        ));
    }
    if stderr.contains("did not match any packages") {
        return Some(format!("{SQLITE_ADVISORY} ignore is stale; remove it"));
    }
    Some(format!(
        "cargo tree -i {SQLITE_CRATE} failed, so the {SQLITE_ADVISORY} ignore cannot be \
         justified"
    ))
}

fn read_log(ctx: &Context<'_>, stream: &str) -> Result<String> {
    let log = ctx.stage_log(SQLITE_PREMISE_LABEL, stream);
    std::fs::read_to_string(&log)
        .with_context(|| format!("read cargo tree output {}", log.display()))
}

#[cfg(test)]
#[path = "audit_tests.rs"]
mod audit_tests;
