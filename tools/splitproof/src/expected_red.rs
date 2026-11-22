//! The splitproof checks known to be red, read from `tools/splitproof/expected-red.txt`.
//!
//! Each line of the file is `id | reason`: `id` is a check name's leading bracket text
//! (`[WL6m] ...` has id `WL6m`) and `reason` says why the check is red today. Blank lines
//! and `#` comment lines are skipped; any other line that is not `id | reason` fails the run
//! before the fleet boots. At the end of a run every failed check is sorted into expected (its
//! id is listed), new (its id is not listed) or stale (a listed id with no failed check), and
//! the run fails on any new or stale entry, so the file cannot drift from the checks.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{bail, Context, Result};

/// The listed check ids, each with the reason it is red.
pub(crate) type ExpectedRed = BTreeMap<String, String>;

/// Where the list lives, relative to the workspace root.
pub(crate) const EXPECTED_RED_PATH: &str = "tools/splitproof/expected-red.txt";

/// Read and parse the list under `root`.
pub(crate) fn load(root: &Path) -> Result<ExpectedRed> {
    let path = root.join(EXPECTED_RED_PATH);
    let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    parse(&text).with_context(|| format!("parse {}", path.display()))
}

/// Parse `id | reason` lines. An id must be one word with no brackets, a reason must not be
/// empty, and an id may be listed once.
pub(crate) fn parse(text: &str) -> Result<ExpectedRed> {
    let mut out = ExpectedRed::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((id, reason)) = line.split_once('|') else {
            bail!("line {}: expected `id | reason`, got {line:?}", n + 1);
        };
        let (id, reason) = (id.trim(), reason.trim());
        if id.is_empty() || id.contains(char::is_whitespace) || id.contains(['[', ']']) {
            bail!("line {}: bad check id {id:?}", n + 1);
        }
        if reason.is_empty() {
            bail!("line {}: {id} has no reason", n + 1);
        }
        if out.insert(id.to_string(), reason.to_string()).is_some() {
            bail!("line {}: {id} is listed twice", n + 1);
        }
    }
    Ok(out)
}

/// The id of a check name: the text of its leading `[...]`, `None` when it has none.
pub(crate) fn check_id(name: &str) -> Option<&str> {
    let rest = name.strip_prefix('[')?;
    rest.find(']').map(|end| &rest[..end])
}

/// A run's failed checks sorted against the list.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Sorted {
    /// Failed checks whose id is listed, each with its listed reason.
    pub(crate) expected: Vec<(String, String)>,
    /// Failed checks whose id is not listed (or that have no id).
    pub(crate) new: Vec<String>,
    /// Listed ids no failed check carries, each with its listed reason.
    pub(crate) stale: Vec<(String, String)>,
}

impl Sorted {
    /// What fails the run: every new failure and every stale entry.
    pub(crate) fn blocking(&self) -> usize {
        self.new.len() + self.stale.len()
    }
}

/// Sort `failed` check names against `expected`. One listed id covers every failed check
/// carrying it; it is stale only when none does.
pub(crate) fn sort(expected: &ExpectedRed, failed: &[String]) -> Sorted {
    let mut out = Sorted::default();
    let mut seen = BTreeSet::new();
    for name in failed {
        match check_id(name).and_then(|id| expected.get_key_value(id)) {
            Some((id, reason)) => {
                seen.insert(id.as_str());
                out.expected.push((name.clone(), reason.clone()));
            }
            None => out.new.push(name.clone()),
        }
    }
    out.stale = expected
        .iter()
        .filter(|(id, _)| !seen.contains(id.as_str()))
        .map(|(id, reason)| (id.clone(), reason.clone()))
        .collect();
    out
}

#[cfg(test)]
#[path = "expected_red_tests.rs"]
mod tests;
