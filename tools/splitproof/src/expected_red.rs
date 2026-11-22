//! The splitproof checks known to be red, read from `tools/splitproof/expected-red.txt`.
//!
//! Each line of the file is `id | signature | reason`: `id` is a check name's leading bracket
//! text (`[WL6m] ...` has id `WL6m`), `signature` is text the check's failure detail must
//! contain (`code=404`), and `reason` says why the check is red today. Blank lines and `#`
//! comment lines are skipped; any other line that is not `id | signature | reason` fails the run
//! before the fleet boots. At the end of a run every failed check is sorted into expected (its
//! id is listed and its detail carries the signature), changed (its id is listed but its detail
//! lacks the signature: it is red for another reason now), new (its id is not listed) or stale
//! (a listed id with no failed check), and the run fails on any changed, new or stale entry, so
//! the file cannot drift from the checks.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{bail, Context, Result};

/// One listed check: the text its failure detail must carry and why it is red.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Entry {
    pub(crate) signature: String,
    pub(crate) reason: String,
}

/// The listed check ids, each with its entry.
pub(crate) type ExpectedRed = BTreeMap<String, Entry>;

/// A failed check: its name and the detail it printed.
pub(crate) type Failure = (String, String);

/// Where the list lives, relative to the workspace root.
pub(crate) const EXPECTED_RED_PATH: &str = "tools/splitproof/expected-red.txt";

/// Read and parse the list under `root`.
pub(crate) fn load(root: &Path) -> Result<ExpectedRed> {
    let path = root.join(EXPECTED_RED_PATH);
    let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    parse(&text).with_context(|| format!("parse {}", path.display()))
}

/// Parse `id | signature | reason` lines. An id must be one word with no brackets, the
/// signature and the reason must not be empty (the reason may itself hold `|`), and an id may
/// be listed once.
pub(crate) fn parse(text: &str) -> Result<ExpectedRed> {
    let mut out = ExpectedRed::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(3, '|').map(str::trim);
        let (Some(id), Some(signature), Some(reason)) = (parts.next(), parts.next(), parts.next()) else {
            bail!("line {}: expected `id | signature | reason`, got {line:?}", n + 1);
        };
        if id.is_empty() || id.contains(char::is_whitespace) || id.contains(['[', ']']) {
            bail!("line {}: bad check id {id:?}", n + 1);
        }
        if signature.is_empty() {
            bail!("line {}: {id} has no failure signature", n + 1);
        }
        if reason.is_empty() {
            bail!("line {}: {id} has no reason", n + 1);
        }
        let entry = Entry { signature: signature.to_string(), reason: reason.to_string() };
        if out.insert(id.to_string(), entry).is_some() {
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

/// A listed check that failed without its signature in the detail.
#[derive(Debug, PartialEq)]
pub(crate) struct Changed {
    pub(crate) name: String,
    pub(crate) detail: String,
    pub(crate) signature: String,
    pub(crate) reason: String,
}

/// A run's failed checks sorted against the list.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Sorted {
    /// Failed checks whose id is listed and whose detail carries the signature, each with its
    /// listed reason.
    pub(crate) expected: Vec<(String, String)>,
    /// Failed checks whose id is listed but whose detail lacks the signature.
    pub(crate) changed: Vec<Changed>,
    /// Failed checks whose id is not listed (or that have no id), each with its detail.
    pub(crate) new: Vec<Failure>,
    /// Listed ids no failed check carries, each with its listed reason.
    pub(crate) stale: Vec<(String, String)>,
}

impl Sorted {
    /// What fails the run: every changed and new failure and every stale entry.
    pub(crate) fn blocking(&self) -> usize {
        self.changed.len() + self.new.len() + self.stale.len()
    }
}

/// Sort `failed` checks against `expected`. A listed id covers the one check carrying it; two
/// different failed check names under one listed id are refused, since the entry's signature
/// and reason can describe only one of them. A listed id is stale only when no check carrying
/// it failed.
pub(crate) fn sort(expected: &ExpectedRed, failed: &[Failure]) -> Result<Sorted> {
    let mut out = Sorted::default();
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for (name, detail) in failed {
        let Some((id, entry)) = check_id(name).and_then(|id| expected.get_key_value(id)) else {
            out.new.push((name.clone(), detail.clone()));
            continue;
        };
        if let Some(other) = seen.insert(id.as_str(), name.as_str()).filter(|other| *other != name.as_str()) {
            bail!("two different checks carry the listed id {id}: {other:?} and {name:?}; give each its own id");
        }
        if detail.contains(entry.signature.as_str()) {
            out.expected.push((name.clone(), entry.reason.clone()));
        } else {
            out.changed.push(Changed {
                name: name.clone(),
                detail: detail.clone(),
                signature: entry.signature.clone(),
                reason: entry.reason.clone(),
            });
        }
    }
    let seen: BTreeSet<&str> = seen.into_keys().collect();
    out.stale = expected
        .iter()
        .filter(|(id, _)| !seen.contains(id.as_str()))
        .map(|(id, entry)| (id.clone(), entry.reason.clone()))
        .collect();
    Ok(out)
}

#[cfg(test)]
#[path = "expected_red_tests.rs"]
mod tests;
