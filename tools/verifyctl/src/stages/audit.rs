use std::ffi::OsString;

use anyhow::Result;

use crate::model::{Outcome, SkipReason};
use crate::runner::Context;

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
    match ctx.command(
        "audit",
        executable.expect("resolved above"),
        vec![
            OsString::from("audit"),
            OsString::from("--ignore"),
            OsString::from("RUSTSEC-2023-0071"),
            // libsqlite3-sys 0.30.1 is in Cargo.lock only because sqlx's weak
            // `sqlx-sqlite?/…` feature entries pull optional backends into the
            // lockfile. The workspace enables only the postgres backend, so
            // sqlite is never compiled: `cargo tree -i libsqlite3-sys
            // --workspace --target all -e all` prints nothing. Remove this
            // ignore once a released sqlx allows libsqlite3-sys >=0.39; the
            // sqlx-sqlite of sqlx 0.9.0 still caps it at <0.38.
            OsString::from("--ignore"),
            OsString::from("RUSTSEC-2026-0459"),
        ],
    ) {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            ctx.note(&format!("cargo-audit invocation failed: {error:#}"))?;
            Ok(Outcome::Fail)
        }
    }
}
