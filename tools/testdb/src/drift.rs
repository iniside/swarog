//! The drift gate for the one authority: nothing outside this crate may decide that a
//! database test is skippable. Executed by this crate's own test suite, so it runs in
//! every `cargo test --workspace` (and therefore in verifyctl's blocking `test` stage).
//!
//! The rules are per-CRATE and per-SHAPE rather than one workspace-wide substring sweep:
//! a single crate reverting must fail on its own, and a reworded message must not buy an
//! exemption. What they do not claim: a hand-rolled skip that neither connects eagerly in
//! a test file, nor prints, nor uses a let-else is out of reach of a text scan — the
//! per-crate routing rule (R1) is the structural floor under that residue.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Workspace source trees scanned. `weles/` is deliberately absent: it is zero-sharing
/// by hard rule and may never import this crate, so a rule demanding it route through
/// `testdb` would be unfollowable. It hosts no live-Postgres test today (its `pgfloor`
/// tests are pure) — a known, recorded gap, not an oversight.
pub const SCANNED_DIRS: [&str; 6] = ["core", "modules", "api", "cmd", "tools", "demos"];

/// This crate's own source, which necessarily names every marker it bans.
const SELF_DIR: &str = "tools/testdb";

/// Known boundary, measured rather than assumed: a helper that returns `Option` and
/// prints NOTHING — `PgPool::connect(..).await.ok()` — inside a crate that routes through
/// the authority in some other test file is NOT caught. The rule that would catch it
/// flags every eager connect in a routing crate's test files, and legitimate ones exist
/// (`modules/scheduler`'s second replica pool). So the floor here is: a skip that
/// ANNOUNCES itself is caught wherever it lives, and a crate that never routes at all is
/// caught whether it announces or not. A silent second authority inside a routing crate
/// is the residue, and it is the shape a reviewer must still look for by hand.
/// An eager Postgres connect. `connect_lazy` is excluded: it opens no socket, so it can
/// neither fail nor skip.
const EAGER_CONNECT: [&str; 3] = ["PgPool::connect(", "PgConnection::connect(", "PgPoolOptions"];

/// The sanctioned source of a test's pool.
const AUTHORITY_CALL: &str = "testdb::test_pool";

/// The one spelling of the opt-out (`testdb::SKIP_ENV`); the literal belongs to no one else.
const OPT_OUT_MARKER: &str = "TESTDB_ALLOW_SKIP";

/// Words that make a printed "skip" a DATABASE skip. Without one of these, a print about
/// skipping is some other degradation (a fixture cleanup with no runtime, say).
const DATABASE_WORDS: [&str; 6] = ["postgres", "database", "db ", "cluster", "dsn", "pool"];

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

/// Is this a test source file? Everything under a `tests` directory plus `tests.rs` /
/// `*_tests.rs`, which is where every live-DB test in this workspace lives.
pub fn is_test_file(rel_path: &str) -> bool {
    let normalized = rel_path.replace('\\', "/");
    let mut parts = normalized.split('/').collect::<Vec<_>>();
    let Some(name) = parts.pop() else {
        return false;
    };
    name == "tests.rs" || name.ends_with("_tests.rs") || parts.contains(&"tests")
}

/// The per-file rules, applied to one test file's relative path + contents.
pub fn file_findings(rel_path: &str, contents: &str) -> Vec<Finding> {
    let normalized = rel_path.replace('\\', "/");
    if normalized.starts_with(SELF_DIR) {
        return Vec::new();
    }
    let mut findings = Vec::new();
    let lines: Vec<&str> = contents.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let mut push = |what: String| {
            findings.push(Finding {
                path: normalized.clone(),
                line: index + 1,
                what,
            });
        };
        let is_comment = line.trim_start().starts_with("//");
        if !is_comment && line.contains(OPT_OUT_MARKER) {
            push("a second spelling of the skip opt-out — `testdb::SKIP_ENV` is the only \
                  one".to_string());
        }
        if !is_test_file(&normalized) || is_comment {
            continue;
        }
        // A connect whose failure binds to an `else` arm: the naive helper's shape,
        // whatever its message says. `test_pool()` is the sanctioned skip.
        if line.contains("let Ok(") || line.contains("let Some(") {
            let window = lines[index..lines.len().min(index + 4)].join(" ");
            let statement = window.split(';').next().unwrap_or(&window);
            let connects = EAGER_CONNECT.iter().any(|marker| statement.contains(marker));
            if connects && statement.contains("else") && !statement.contains("test_pool(") {
                push(
                    "a connect whose failure diverges instead of failing the test — a \
                     connect a test needs is `.expect(...)`, and the one skippable pool \
                     comes from `testdb::test_pool`"
                        .to_string(),
                );
            }
        }
        if let Some(literal) = printed_literal(line) {
            let lowered = literal.to_ascii_lowercase();
            if lowered.contains("skip") && DATABASE_WORDS.iter().any(|w| lowered.contains(w)) {
                push(format!(
                    "a hand-rolled database-skip line ({literal:?}) — the skip decision \
                     belongs to `testdb::test_pool`, which fails the run unless \
                     TESTDB_ALLOW_SKIP is on"
                ));
            }
        }
    }
    findings
}

/// The string literal a `println!`/`eprintln!` on this line prints, if any.
fn printed_literal(line: &str) -> Option<&str> {
    let call = line.find("println!")?;
    let rest = &line[call..];
    let open = rest.find('"')? + 1;
    let close = rest[open..].find('"')? + open;
    Some(&rest[open..close])
}

/// The per-crate rules. `dev_deps_testdb` and the file set come from the crate itself, so
/// one crate reverting fails on its own rather than hiding behind the other twenty.
pub fn crate_findings(
    crate_dir: &str,
    dev_deps_testdb: bool,
    test_files: &[(String, String)],
) -> Vec<Finding> {
    let normalized = crate_dir.replace('\\', "/");
    if normalized.starts_with(SELF_DIR) {
        return Vec::new();
    }
    let mut findings = Vec::new();
    let connects: BTreeSet<&str> = test_files
        .iter()
        .filter(|(_, body)| {
            body.lines().any(|line| {
                !line.trim_start().starts_with("//")
                    && EAGER_CONNECT.iter().any(|marker| line.contains(marker))
            })
        })
        .map(|(path, _)| path.as_str())
        .collect();
    let routes = test_files
        .iter()
        .any(|(_, body)| body.contains(AUTHORITY_CALL));

    if !connects.is_empty() && !dev_deps_testdb {
        findings.push(Finding {
            path: format!("{normalized}/Cargo.toml"),
            line: 0,
            what: format!(
                "connects to Postgres in {} but does not dev-depend on `testdb` — its \
                 pool decision is its own",
                connects.iter().copied().collect::<Vec<_>>().join(", ")
            ),
        });
    }
    if !connects.is_empty() && !routes {
        findings.push(Finding {
            path: normalized.clone(),
            line: 0,
            what: format!(
                "connects to Postgres in {} but no test file names `{AUTHORITY_CALL}` — \
                 this crate has its own skip authority",
                connects.iter().copied().collect::<Vec<_>>().join(", ")
            ),
        });
    }
    if dev_deps_testdb && !routes {
        findings.push(Finding {
            path: format!("{normalized}/Cargo.toml"),
            line: 0,
            what: format!(
                "dev-depends on `testdb` but no test file calls `{AUTHORITY_CALL}` — the \
                 authority was bypassed, or the dependency is stale"
            ),
        });
    }
    findings
}

/// Scans the workspace crate by crate. A missing scan dir is a violation too — a gate
/// that can silently scan nothing is not a gate.
pub fn workspace_findings(root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    for dir in SCANNED_DIRS {
        let base = root.join(dir);
        if !base.is_dir() {
            findings.push(Finding {
                path: dir.to_string(),
                line: 0,
                what: "scan directory is missing — the gate would scan nothing".to_string(),
            });
        }
    }
    for manifest in manifests(root) {
        let crate_dir = manifest.parent().expect("a manifest has a directory");
        let rel_dir = relative(root, crate_dir);
        let Ok(manifest_text) = std::fs::read_to_string(&manifest) else {
            findings.push(Finding {
                path: rel_dir,
                line: 0,
                what: "unreadable manifest — the gate cannot judge this crate".to_string(),
            });
            continue;
        };
        let mut test_files = Vec::new();
        for file in rust_sources(crate_dir) {
            let rel = relative(root, &file);
            if !is_test_file(&rel) {
                continue;
            }
            match std::fs::read_to_string(&file) {
                Ok(body) => {
                    findings.extend(file_findings(&rel, &body));
                    test_files.push((rel, body));
                }
                Err(_) => findings.push(Finding {
                    path: rel,
                    line: 0,
                    what: "unreadable source file — the gate cannot judge it".to_string(),
                }),
            }
        }
        findings.extend(crate_findings(
            &rel_dir,
            dev_deps_testdb(&manifest_text),
            &test_files,
        ));
    }
    findings
}

/// Does this manifest list `testdb` under `[dev-dependencies]`? A NORMAL dependency is a
/// different relationship (verifyctl reads the opt-out to refuse a pointless run) and is
/// not a claim about this crate's own DB tests.
pub fn dev_deps_testdb(manifest: &str) -> bool {
    let mut in_dev = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_dev = trimmed == "[dev-dependencies]";
            continue;
        }
        if in_dev && (trimmed.starts_with("testdb ") || trimmed.starts_with("testdb.")) {
            return true;
        }
    }
    false
}

fn manifests(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in SCANNED_DIRS {
        collect_manifests(&root.join(dir), &mut out);
    }
    out.sort();
    out
}

fn collect_manifests(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            collect_manifests(&path, out);
        } else if path.file_name().is_some_and(|n| n == "Cargo.toml") {
            out.push(path);
        }
    }
}

fn rust_sources(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let skip = path
                .file_name()
                .is_some_and(|n| n == "target" || n == "fixtures");
            // A nested crate's sources belong to that crate's own pass.
            if skip || path.join("Cargo.toml").is_file() {
                continue;
            }
            out.extend(rust_sources(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The workspace root, derived from this crate's manifest dir (`<root>/tools/testdb`).
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("tools/testdb sits two levels under the workspace root")
        .to_path_buf()
}
