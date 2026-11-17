//! `tools/admindiff/accept/<page>.txt`: one accepted difference per line, as
//! `<difference> | <reason>`. Blank lines and lines starting with `#` are skipped.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};

/// Accepted difference → reason.
pub fn parse(src: &str) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    let mut problems = Vec::new();
    for (n, line) in src.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((diff, reason)) = line.rsplit_once(" | ") else {
            problems.push(format!("line {}: no \" | <reason>\"", n + 1));
            continue;
        };
        let (diff, reason) = (diff.trim(), reason.trim());
        if reason.is_empty() {
            problems.push(format!("line {}: empty reason", n + 1));
        }
        if out.insert(diff.to_string(), reason.to_string()).is_some() {
            problems.push(format!("line {}: duplicate entry {diff}", n + 1));
        }
    }
    if !problems.is_empty() {
        bail!("{}", problems.join("\n"));
    }
    Ok(out)
}

pub struct Verdict {
    pub accepted: Vec<String>,
    pub unaccepted: Vec<String>,
    /// Entries matching no current difference.
    pub stale: Vec<String>,
}

pub fn judge(diffs: &BTreeSet<String>, accept: &BTreeMap<String, String>) -> Verdict {
    let (accepted, unaccepted) = diffs.iter().cloned().partition(|d| accept.contains_key(d));
    let stale = accept.keys().filter(|k| !diffs.contains(*k)).cloned().collect();
    Verdict { accepted, unaccepted, stale }
}
