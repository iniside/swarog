//! The drift gate for the one authority: nothing outside this crate may decide that a
//! database test is skippable. Executed by this crate's own test suite, so it runs in
//! every `cargo test --workspace` (and therefore in verifyctl's blocking `test` stage).

use std::path::{Path, PathBuf};

/// Workspace source trees scanned. `weles/` is deliberately absent: it is zero-sharing
/// by hard rule and may never import this crate, so a rule demanding it route through
/// `testdb` would be unfollowable. It hosts no live-Postgres test today (its `pgfloor`
/// tests are pure) — a known, recorded gap, not an oversight.
pub const SCANNED_DIRS: [&str; 6] = ["core", "modules", "api", "cmd", "tools", "demos"];

/// This crate's own source, which necessarily names every marker it bans.
const SELF_DIR: &str = "tools/testdb";

/// The hand-copied skip line the 21 naive helpers all printed. The message is banned
/// outright: the decision it announces is not this file's to make.
const SKIP_LINE_MARKER: &str = "postgres unreachable";

/// A pool helper that can answer "no pool" — the shape of every naive copy. Legal only
/// in a file that gets its pool from the authority, i.e. a wrapper that adds a schema
/// migration on top of [`AUTHORITY_USE`].
const FALLIBLE_POOL_MARKERS: [&str; 2] = ["-> Option<PgPool>", "-> Option<(PgPool"];

/// The one spelling of the opt-out.
const OPT_OUT_MARKER: &str = "TESTDB_ALLOW_SKIP";

/// The marker proving a file (or the workspace) routes through the authority.
const AUTHORITY_USE: &str = "testdb::test_pool";

#[derive(Debug, PartialEq, Eq)]
pub struct Finding {
    pub path: String,
    pub line: usize,
    pub what: String,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{} — {}", self.path, self.line, self.what)
    }
}

/// The pure rule, applied to one file's relative path + contents.
pub fn file_findings(rel_path: &str, contents: &str) -> Vec<Finding> {
    let normalized = rel_path.replace('\\', "/");
    if normalized.starts_with(SELF_DIR) {
        return Vec::new();
    }
    let routes_through_authority = contents.contains(AUTHORITY_USE);
    let mut findings = Vec::new();
    for (number, line) in contents.lines().enumerate() {
        // Prose may discuss the markers; only code can print a skip or read the env var.
        let is_comment = line.trim_start().starts_with("//");
        let mut push = |what: &str| {
            findings.push(Finding {
                path: normalized.clone(),
                line: number + 1,
                what: what.to_string(),
            });
        };
        if !is_comment && line.to_ascii_lowercase().contains(SKIP_LINE_MARKER) {
            push(
                "a hand-rolled postgres-unreachable skip line — the skip decision belongs to \
                 `testdb::test_pool`, which fails the run unless TESTDB_ALLOW_SKIP is on",
            );
        }
        if !routes_through_authority
            && FALLIBLE_POOL_MARKERS.iter().any(|m| line.contains(m))
        {
            push(
                "a pool helper that can answer `None` in a file that never calls \
                 `testdb::test_pool` — a second skip authority",
            );
        }
        if !is_comment && line.contains(OPT_OUT_MARKER) {
            push("a second spelling of the skip opt-out — `testdb::SKIP_ENV` is the only one");
        }
    }
    findings
}

/// Scans the workspace. A missing scan dir and an unused authority are violations too —
/// a gate that can silently match zero targets is not a gate.
pub fn workspace_findings(root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut authority_seen = false;
    for dir in SCANNED_DIRS {
        let base = root.join(dir);
        if !base.is_dir() {
            findings.push(Finding {
                path: dir.to_string(),
                line: 0,
                what: "scan directory is missing — the gate would scan nothing".to_string(),
            });
            continue;
        }
        for file in rust_sources(&base) {
            let rel = file
                .strip_prefix(root)
                .unwrap_or(&file)
                .to_string_lossy()
                .replace('\\', "/");
            let Ok(contents) = std::fs::read_to_string(&file) else {
                findings.push(Finding {
                    path: rel,
                    line: 0,
                    what: "unreadable source file — the gate cannot judge it".to_string(),
                });
                continue;
            };
            if contents.contains(AUTHORITY_USE) {
                authority_seen = true;
            }
            findings.extend(file_findings(&rel, &contents));
        }
    }
    if !authority_seen {
        findings.push(Finding {
            path: "<workspace>".to_string(),
            line: 0,
            what: format!(
                "no scanned source names `{AUTHORITY_USE}` — the shared skip authority is \
                 unused, so these rules are vacuous"
            ),
        });
    }
    findings
}

fn rust_sources(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            out.extend(rust_sources(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out
}

/// The workspace root, derived from this crate's manifest dir (`<root>/tools/testdb`).
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("tools/testdb sits two levels under the workspace root")
        .to_path_buf()
}
