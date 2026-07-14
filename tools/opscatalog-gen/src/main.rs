//! `opscatalog-gen` — regenerates `opscatalog/src/generated.rs` (the `pub const
//! OPERATIONS: &[OpInfo]` table) from every `api/*/api` crate's generated
//! `<snake>_rpc::route_bindings()`.
//!
//! ## Authority
//! `route_bindings()` returns ONLY the `#[http]`-bound operations of a `#[rpc]` trait
//! (wire-only methods yield an empty vec), carrying the IDENTICAL impl-free
//! `opsapi::Operation` the gateway route table uses. Collecting them across every rpc
//! module and rendering `(method, verb, path, auth)` is thus a faithful, drift-free
//! projection of the served op surface — no hand-maintained method list.
//!
//! ## Didn't-forget self-check (house rule)
//! [`rpc_modules`] is the one hand-list this tool carries: the generated rpc modules it
//! calls `route_bindings()` on. Before emitting, it is diffed against the real source of
//! truth — every `#[rpc]` trait under `api/*/api/src/lib.rs` — and the run DIES with a
//! per-entry fix on any drift, so a newly-added provider can't silently be omitted from
//! the catalog (which the freshness gate could not catch, since it and the commit run the
//! SAME generator).
//!
//! ## Output & freshness
//! Deterministic: operations sorted by `method`, no timestamps. Default target is the
//! committed `opscatalog/src/generated.rs` (the re-bless: `cargo run -p opscatalog-gen`);
//! `--out <path>` writes elsewhere (the `codegen-freshness` stage regenerates to a temp
//! and diffs against the commit).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _, Result};

fn main() -> Result<()> {
    let out = parse_out_arg()?.unwrap_or_else(|| workspace_root().join("opscatalog/src/generated.rs"));
    let rendered = render()?;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    std::fs::write(&out, &rendered).with_context(|| format!("write {}", out.display()))?;
    println!("opscatalog-gen: wrote {}", out.display());
    Ok(())
}

/// `--out <path>` (the only flag). Absent = the committed default target.
fn parse_out_arg() -> Result<Option<PathBuf>> {
    let mut args = std::env::args().skip(1);
    let mut out = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = Some(PathBuf::from(args.next().context("--out needs a path")?)),
            other => bail!("unknown argument {other:?} (only --out <path> is accepted)"),
        }
    }
    Ok(out)
}

/// The workspace root, from this crate's compile-time manifest dir (`tools/opscatalog-gen`
/// → up two). The tool is always run in-repo (like topiccheck / csharp-client-gen).
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Every generated rpc module, referenced DIRECTLY (so a renamed/removed trait breaks this
/// tool at compile time) with its `crate::module` label for the filesystem self-check.
/// The hand-list mirrors topiccheck's `rpc_modules()`; wire-only modules (empty
/// `route_bindings()`) are LISTED so a newly `#[http]`-bound method lands automatically and
/// the self-check's completeness holds.
fn rpc_modules() -> Vec<(&'static str, Vec<opsapi::RouteBinding>)> {
    vec![
        ("accountsapi::auth_rpc", accountsapi::auth_rpc::route_bindings()),
        ("accountsapi::sessions_rpc", accountsapi::sessions_rpc::route_bindings()),
        ("adminapi::admin_data_rpc", adminapi::admin_data_rpc::route_bindings()),
        ("adminapi::admin_submit_rpc", adminapi::admin_submit_rpc::route_bindings()),
        ("apikeysapi::keys_rpc", apikeysapi::keys_rpc::route_bindings()),
        ("charactersapi::ownership_rpc", charactersapi::ownership_rpc::route_bindings()),
        ("charactersapi::player_rpc", charactersapi::player_rpc::route_bindings()),
        ("configapi::config_snapshot_rpc", configapi::config_snapshot_rpc::route_bindings()),
        ("inventoryapi::holdings_rpc", inventoryapi::holdings_rpc::route_bindings()),
        ("leaderboardapi::leaderboard_rpc", leaderboardapi::leaderboard_rpc::route_bindings()),
        ("matchapi::match_rpc", matchapi::match_rpc::route_bindings()),
        ("ratingapi::mmr_reader_rpc", ratingapi::mmr_reader_rpc::route_bindings()),
    ]
}

/// One catalog row, in the shape `opscatalog::OpInfo` renders as.
struct Row {
    method: String,
    verb: String,
    path: String,
    auth: &'static str,
}

/// `opsapi::AuthReq` → the wire string `OpInfo::auth` carries (matches csharp-client-gen).
fn auth_str(a: opsapi::AuthReq) -> &'static str {
    match a {
        opsapi::AuthReq::None => "none",
        opsapi::AuthReq::Player => "player",
    }
}

/// Collects every `#[http]`-bound operation, sorted by method (deterministic). Runs the
/// self-check FIRST so a missing provider fails loudly before any output.
fn collect_rows() -> Result<Vec<Row>> {
    let modules = rpc_modules();
    self_check_rpc_list(&modules.iter().map(|(l, _)| *l).collect::<Vec<_>>())?;
    let mut rows: Vec<Row> = Vec::new();
    for (_, bindings) in modules {
        for rb in bindings {
            let op = rb.operation;
            rows.push(Row {
                method: op.method,
                verb: op.verb,
                path: op.path,
                auth: auth_str(op.auth),
            });
        }
    }
    rows.sort_by(|a, b| a.method.cmp(&b.method));
    Ok(rows)
}

/// Renders the full `generated.rs` (header + sorted array). Byte-stable for the diff.
fn render() -> Result<String> {
    let rows = collect_rows()?;
    let mut out = String::new();
    out.push_str(
        "// @generated by tools/opscatalog-gen -- DO NOT EDIT BY HAND.\n\
         // Regenerate: cargo run -p opscatalog-gen (freshness-gated by verifyctl codegen-freshness).\n\
         // Source: every #[http]-bound operation's route_bindings() across api/*/api, sorted by method.\n\
         pub const OPERATIONS: &[OpInfo] = &[\n",
    );
    for r in &rows {
        out.push_str(&format!(
            "    OpInfo {{ method: {:?}, verb: {:?}, path: {:?}, auth: {:?} }},\n",
            r.method, r.verb, r.path, r.auth
        ));
    }
    out.push_str("];\n");
    Ok(out)
}

// ---------------------------------------------------------------------------
// Didn't-forget self-check (ported from topiccheck's contract-golden)
// ---------------------------------------------------------------------------

/// `OwnerOf` → `owner_of` — the module-name derivation `rpc_macro::to_snake` applies.
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

/// Every `#[rpc]`-annotated trait under `api/*/api/src/lib.rs`, as `"<crate>::<snake>_rpc"`
/// labels — the source of truth the [`rpc_modules`] hand-list must match.
fn rpc_modules_from_fs() -> Result<BTreeSet<String>> {
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
            let trait_name = lines[i + 1..].iter().find_map(|l| {
                let t = l.trim_start();
                t.strip_prefix("pub trait ")
                    .map(|r| r.split(|c: char| !c.is_alphanumeric()).next().unwrap_or("").to_string())
            });
            match trait_name {
                Some(t) if !t.is_empty() => {
                    expected.insert(format!("{crate_name}::{}_rpc", to_snake(&t)));
                }
                _ => bail!("{}:{}: found `#[rpc(` with no following `pub trait`", lib.display(), i + 1),
            }
        }
    }
    Ok(expected)
}

/// Dies if [`rpc_modules`] drifts from the `#[rpc]` traits present under `api/*/api`.
fn self_check_rpc_list(listed: &[&'static str]) -> Result<()> {
    let expected = rpc_modules_from_fs()?;
    let listed: BTreeSet<String> = listed.iter().map(|s| s.to_string()).collect();
    let mut drift = Vec::new();
    for m in expected.difference(&listed) {
        drift.push(format!(
            "MISSING from rpc_modules(): {m} -- add `(\"{m}\", {m}::route_bindings())` to \
             tools/opscatalog-gen/src/main.rs"
        ));
    }
    for m in listed.difference(&expected) {
        drift.push(format!(
            "STALE in rpc_modules(): {m} -- no matching #[rpc] trait under api/*/api; remove it"
        ));
    }
    if drift.is_empty() {
        Ok(())
    } else {
        bail!("opscatalog-gen: rpc-module hand-list drifted from api/*/api:\n  {}", drift.join("\n  "))
    }
}
