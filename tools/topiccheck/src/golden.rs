//! `contract-golden` — the VALUE-level contract baseline (remediation round 3, Step 6c).
//!
//! `cargo-public-api` diffs contract-crate SIGNATURES, but it structurally cannot see
//! the runtime VALUES built in function bodies / static initializers: the topic string,
//! version, and history policy inside each `bus::define(...)`, and the HTTP
//! verb/path/auth/success-status/retry-mode inside each generated `Operation`. A silent
//! edit to any of those ships a breaking wire change with a clean public-api diff.
//!
//! This module renders those values into stable sorted lines, diffs them against the
//! COMMITTED golden at `docs/reference/contract-golden/contracts.txt`, and fails on any
//! difference (removed = BREAKING, added = ADDITIVE — same semantics as the public-api
//! baseline). Re-bless intentional changes via `./verify.sh --bless-contract-golden`
//! (or `-BlessContractGolden`), i.e. `cargo run -p topiccheck -- contract-golden --bless`.
//!
//! ## Sources
//! - **Events:** [`crate::defined_topics`] — the canonical `bus::define` list, already
//!   compile-time-coupled to every events crate.
//! - **RPC (http-bound):** each generated `<snake>_rpc::route_bindings()` — impl-free
//!   and carrying the IDENTICAL `opsapi::Operation` values that `operations()` (and
//!   therefore the gateway route table) uses, so no provider impl and no DB are needed.
//!   Wire-only traits (no `#[http]`) yield zero bindings; they stay listed so a newly
//!   HTTP-bound method on them lands in the golden automatically.
//! - **RPC (wire retry semantics):** each generated `<snake>_rpc::wire_ops()` — the
//!   `opsapi::WireOp` (method + `RetryMode`) for EVERY method, http-bound AND
//!   wire-only. This closes the former blind spot (Step 6c): a wire-only method's
//!   `#[retry_safe]` was compiled only into the client and surfaced no data value, so
//!   a silent flip was guarded by review alone; it is now a golden line.
//!
//! ## Self-check (house rule: hand-maintained lists must self-verify)
//! [`rpc_modules`] is a hand-list of generated rpc modules. Before diffing, it is
//! checked against the real source of truth — every `#[rpc]` trait found under
//! `api/*/api/src/lib.rs` — and the run dies with a per-entry fix if the two drift.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use bus::HistoryPolicy;

/// Header written at the top of the golden file; `#`-prefixed lines are ignored when
/// comparing, so the header never participates in the diff.
const GOLDEN_HEADER: &str = "\
# contract-golden -- VALUE-level event + rpc contract baseline (Step 6c).
# Lines: `event topic=<t> version=<n> history=<policy>` from every bus::define,
# `rpc module=<crate::mod> method=<m> verb=<V> path=<p> auth=<a> success=<n> retry=<r>`
# from every generated route_bindings(), and
# `wire module=<crate::mod> method=<m> retry=<r>` from every generated wire_ops()
# (EVERY method, http-bound and wire-only). ANY diff fails the blocking contract-golden
# verify stage: a removed line is BREAKING, an added line is ADDITIVE.
# Regenerate intentionally via ./verify.sh --bless-contract-golden (verify.ps1
# -BlessContractGolden), i.e. `cargo run -p topiccheck -- contract-golden --bless`.";

/// Repo-relative location of the committed golden (mirrors
/// `docs/reference/public-api-baseline/`).
const GOLDEN_REL: &str = "docs/reference/contract-golden/contracts.txt";

/// The workspace root, derived from this crate's manifest dir (`tools/topiccheck`), so
/// the check finds the committed golden from both `cargo run` and `cargo test`.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The generated rpc modules, each referenced DIRECTLY (so a renamed/removed trait
/// breaks this tool at compile time, the `defined_topics` idiom) and labeled with its
/// `crate::module` path for the golden lines and the filesystem self-check. Each entry
/// carries both `route_bindings()` (http-bound `Operation`s) and `wire_ops()` (every
/// method's `RetryMode`, incl. wire-only) from the SAME module — one hand-list, one
/// self-check, both value sources.
#[allow(clippy::type_complexity)]
fn rpc_modules() -> Vec<(&'static str, Vec<opsapi::RouteBinding>, Vec<opsapi::WireOp>)> {
    vec![
        (
            "accountsapi::auth_rpc",
            accountsapi::auth_rpc::route_bindings(),
            accountsapi::auth_rpc::wire_ops(),
        ),
        (
            "accountsapi::sessions_rpc",
            accountsapi::sessions_rpc::route_bindings(),
            accountsapi::sessions_rpc::wire_ops(),
        ),
        (
            "adminapi::admin_data_rpc",
            adminapi::admin_data_rpc::route_bindings(),
            adminapi::admin_data_rpc::wire_ops(),
        ),
        (
            "apikeysapi::keys_rpc",
            apikeysapi::keys_rpc::route_bindings(),
            apikeysapi::keys_rpc::wire_ops(),
        ),
        (
            "charactersapi::ownership_rpc",
            charactersapi::ownership_rpc::route_bindings(),
            charactersapi::ownership_rpc::wire_ops(),
        ),
        (
            "charactersapi::player_rpc",
            charactersapi::player_rpc::route_bindings(),
            charactersapi::player_rpc::wire_ops(),
        ),
        (
            "configapi::config_snapshot_rpc",
            configapi::config_snapshot_rpc::route_bindings(),
            configapi::config_snapshot_rpc::wire_ops(),
        ),
        (
            "inventoryapi::holdings_rpc",
            inventoryapi::holdings_rpc::route_bindings(),
            inventoryapi::holdings_rpc::wire_ops(),
        ),
        (
            "leaderboardapi::leaderboard_rpc",
            leaderboardapi::leaderboard_rpc::route_bindings(),
            leaderboardapi::leaderboard_rpc::wire_ops(),
        ),
        (
            "matchapi::match_rpc",
            matchapi::match_rpc::route_bindings(),
            matchapi::match_rpc::wire_ops(),
        ),
        (
            "ratingapi::mmr_reader_rpc",
            ratingapi::mmr_reader_rpc::route_bindings(),
            ratingapi::mmr_reader_rpc::wire_ops(),
        ),
    ]
}

/// `OwnerOf` -> `owner_of` — the module-name derivation `rpc_macro::to_snake` applies,
/// duplicated here for the filesystem self-check (4 lines, stable by contract: the
/// generated module name IS part of every api crate's public surface).
fn to_snake(pascal: &str) -> String {
    let mut out = String::new();
    for (i, c) in pascal.chars().enumerate() {
        if c.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The source of truth for the [`rpc_modules`] hand-list: every `#[rpc]`-annotated
/// trait under `api/*/api/src/lib.rs`, rendered as `"<crate>::<snake>_rpc"` labels.
fn rpc_modules_from_fs() -> anyhow::Result<BTreeSet<String>> {
    let api_root = workspace_root().join("api");
    let mut expected = BTreeSet::new();
    for entry in std::fs::read_dir(&api_root)? {
        let dir = entry?.path();
        let cargo = dir.join("api").join("Cargo.toml");
        let lib = dir.join("api").join("src").join("lib.rs");
        if !cargo.is_file() || !lib.is_file() {
            continue;
        }
        let crate_name = std::fs::read_to_string(&cargo)?
            .lines()
            .find_map(|l| {
                l.strip_prefix("name = \"").and_then(|r| r.strip_suffix('"')).map(String::from)
            })
            .ok_or_else(|| anyhow::anyhow!("no `name = \"..\"` in {}", cargo.display()))?
            .replace('-', "_");
        let src = std::fs::read_to_string(&lib)?;
        let lines: Vec<&str> = src.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !line.trim_start().starts_with("#[rpc(") {
                continue;
            }
            // The trait item follows the attribute (possibly after more attributes).
            let trait_name = lines[i + 1..].iter().find_map(|l| {
                let t = l.trim_start();
                t.strip_prefix("pub trait ")
                    .map(|r| r.split(|c: char| !c.is_alphanumeric()).next().unwrap_or("").to_string())
            });
            match trait_name {
                Some(t) if !t.is_empty() => {
                    expected.insert(format!("{crate_name}::{}_rpc", to_snake(&t)));
                }
                _ => anyhow::bail!(
                    "{}:{}: found `#[rpc(` with no following `pub trait`",
                    lib.display(),
                    i + 1
                ),
            }
        }
    }
    Ok(expected)
}

/// Dies (per house rule) if the [`rpc_modules`] hand-list drifts from the `#[rpc]`
/// traits actually present under `api/*/api/`, with a per-entry fix.
fn self_check_rpc_list(listed: &[&'static str]) -> anyhow::Result<()> {
    let expected = rpc_modules_from_fs()?;
    let listed: BTreeSet<String> = listed.iter().map(|s| s.to_string()).collect();
    let mut drift = Vec::new();
    for m in expected.difference(&listed) {
        drift.push(format!(
            "MISSING from rpc_modules(): {m} -- add `(\"{m}\", {m}::route_bindings())` \
             to tools/topiccheck/src/golden.rs"
        ));
    }
    for m in listed.difference(&expected) {
        drift.push(format!(
            "STALE in rpc_modules(): {m} -- no matching #[rpc] trait under api/*/api; \
             remove it from tools/topiccheck/src/golden.rs"
        ));
    }
    if drift.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "contract-golden: rpc-module hand-list drifted from api/*/api (fix before \
             any diff runs):\n  {}",
            drift.join("\n  ")
        )
    }
}

/// Renders the LIVE contract values as the golden's sorted line set.
pub fn live_lines() -> anyhow::Result<BTreeSet<String>> {
    let mut lines = BTreeSet::new();
    for c in crate::defined_topics() {
        let history = match c.history {
            HistoryPolicy::MinRetention { days } => format!("min-retention:{days}d"),
            HistoryPolicy::KeepForever => "keep-forever".to_string(),
        };
        lines.insert(format!(
            "event topic={} version={} history={history}",
            c.topic, c.version
        ));
    }
    let modules = rpc_modules();
    self_check_rpc_list(&modules.iter().map(|(l, _, _)| *l).collect::<Vec<_>>())?;
    for (label, bindings, wire_ops) in modules {
        for rb in bindings {
            let op = rb.operation;
            lines.insert(format!(
                "rpc module={label} method={} verb={} path={} auth={:?} success={} retry={:?}",
                op.method, op.verb, op.path, op.auth, op.success, op.retry_mode
            ));
        }
        for w in wire_ops {
            lines.insert(format!(
                "wire module={label} method={} retry={:?}",
                w.method, w.retry_mode
            ));
        }
    }
    Ok(lines)
}

/// The committed golden's line set (comments/blank lines stripped). `Ok(None)` when the
/// file does not exist yet (first run before any bless).
fn committed_lines() -> anyhow::Result<Option<BTreeSet<String>>> {
    let path = workspace_root().join(GOLDEN_REL);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)?;
    Ok(Some(
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(String::from)
            .collect(),
    ))
}

/// Diffs live vs committed. Returns the findings (empty = clean); a missing golden is
/// itself a finding pointing at the bless flow.
pub fn check() -> anyhow::Result<Vec<String>> {
    let live = live_lines()?;
    let Some(committed) = committed_lines()? else {
        return Ok(vec![format!(
            "MISSING golden {GOLDEN_REL} -- run `cargo run -p topiccheck -- contract-golden --bless`"
        )]);
    };
    let mut findings = Vec::new();
    for l in committed.difference(&live) {
        findings.push(format!("REMOVED (BREAKING): {l}"));
    }
    for l in live.difference(&committed) {
        findings.push(format!("ADDED (additive): {l}"));
    }
    Ok(findings)
}

/// Writes the golden from the live values (the `--bless` flow).
pub fn bless() -> anyhow::Result<PathBuf> {
    let path = workspace_root().join(GOLDEN_REL);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let lines = live_lines()?;
    let mut out = String::from(GOLDEN_HEADER);
    out.push('\n');
    for l in &lines {
        out.push('\n');
        out.push_str(l);
    }
    out.push('\n');
    std::fs::write(&path, out)?;
    Ok(path)
}

pub fn render_to(path: &std::path::Path) -> anyhow::Result<PathBuf> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let lines = live_lines()?;
    let mut out = String::from(GOLDEN_HEADER);
    out.push('\n');
    for line in lines {
        out.push('\n');
        out.push_str(&line);
    }
    out.push('\n');
    std::fs::write(path, out)?;
    Ok(path.to_path_buf())
}

/// Entry point for `topiccheck contract-golden [--bless]`: bless writes the golden;
/// the default run diffs and exits non-zero on any finding.
pub fn run(bless_flag: bool) -> anyhow::Result<()> {
    if bless_flag {
        let path = bless()?;
        println!("contract-golden: blessed {}", path.display());
        return Ok(());
    }
    let findings = check()?;
    if findings.is_empty() {
        println!(
            "contract-golden: OK -- live define()/operations() values match {GOLDEN_REL}"
        );
        Ok(())
    } else {
        eprintln!("contract-golden: FAIL -- live contract values differ from {GOLDEN_REL}:");
        for f in &findings {
            eprintln!("  {f}");
        }
        eprintln!(
            "  (a changed value shows as one REMOVED + one ADDED line; if intentional, \
             re-bless via ./verify.sh --bless-contract-golden)"
        );
        std::process::exit(1);
    }
}

#[cfg(test)]
#[path = "golden_tests.rs"]
mod tests;
