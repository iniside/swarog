//! The splitproof checks known to be red, read from `tools/splitproof/expected-red.txt`.
//!
//! Each line of the file is `id | signature | reason`: `id` is a check name's leading bracket
//! text (`[WL6m] ...` has id `WL6m`), `signature` is one or more fragments joined by `&&`, each
//! of which the check's failure detail must contain (`code=404`, or
//! `grant=404 && subscription=Some(("active", 0))` to pin a second, independent field), and
//! `reason` says why the check is red today. Blank lines and `#` comment lines are skipped; any
//! other line that is not `id | signature | reason` fails the run before the fleet boots. At the
//! end of a run every failed check is sorted into expected (its id is listed and its detail
//! carries every fragment), changed (its id is listed but its detail lacks a fragment: it is red
//! for another reason now), new (its id is not listed) or stale (a listed id with no failed
//! check), and the run fails on any changed, new or stale entry, so the file cannot drift from
//! the checks. Every id names one check: `Proof::check` records each name it sees, passing or
//! failing, and the sort refuses an id carried by two different names.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{bail, Context, Result};

/// One listed check: the fragments its failure detail must each carry and why it is red.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Entry {
    pub(crate) fragments: Vec<String>,
    pub(crate) reason: String,
}

impl Entry {
    /// The fragments `detail` lacks; empty when the detail carries the whole signature.
    pub(crate) fn missing(&self, detail: &str) -> Vec<String> {
        self.fragments.iter().filter(|f| !detail.contains(f.as_str())).cloned().collect()
    }
}

/// The listed check ids, each with its entry.
pub(crate) type ExpectedRed = BTreeMap<String, Entry>;

/// A failed check: its name and the detail it printed.
pub(crate) type Failure = (String, String);

/// Every check name a run saw, passing or failing, under its id.
pub(crate) type Names = BTreeMap<String, BTreeSet<String>>;

/// Record one check `name` under its id; a name with no id is not recorded, since no entry can
/// list it.
pub(crate) fn record(names: &mut Names, name: &str) {
    if let Some(id) = check_id(name) {
        names.entry(id.to_string()).or_default().insert(name.to_string());
    }
}

/// The fragment separator of a signature.
pub(crate) const FRAGMENT_SEPARATOR: &str = "&&";

/// Where the list lives, relative to the workspace root.
pub(crate) const EXPECTED_RED_PATH: &str = "tools/splitproof/expected-red.txt";

/// Read and parse the list under `root`.
pub(crate) fn load(root: &Path) -> Result<ExpectedRed> {
    let path = root.join(EXPECTED_RED_PATH);
    let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    parse(&text).with_context(|| format!("parse {}", path.display()))
}

/// Parse `id | signature | reason` lines. An id must be one word with no brackets, the
/// signature must hold at least one fragment and no empty one (fragments are split on `&&` and
/// trimmed), the reason must not be empty (it may itself hold `|`), and an id may be listed
/// once.
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
        let fragments: Vec<String> = signature.split(FRAGMENT_SEPARATOR).map(|f| f.trim().to_string()).collect();
        if fragments.iter().any(String::is_empty) {
            bail!("line {}: {id} has an empty signature fragment in {signature:?}", n + 1);
        }
        if reason.is_empty() {
            bail!("line {}: {id} has no reason", n + 1);
        }
        let entry = Entry { fragments, reason: reason.to_string() };
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

/// A listed check that failed without its whole signature in the detail.
#[derive(Debug, PartialEq)]
pub(crate) struct Changed {
    pub(crate) name: String,
    pub(crate) detail: String,
    /// The listed fragments the detail lacks.
    pub(crate) missing: Vec<String>,
    pub(crate) reason: String,
}

/// A run's failed checks sorted against the list.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Sorted {
    /// Failed checks whose id is listed and whose detail carries every fragment, each with its
    /// listed reason.
    pub(crate) expected: Vec<(String, String)>,
    /// Failed checks whose id is listed but whose detail lacks a fragment.
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

/// Sort `failed` checks against `expected`. Each id names one check: an id `names` holds under
/// two different check names (the failed ones included) is refused, listed or not and whether each of them passed or
/// failed, since an entry's signature and reason can describe only one check and a passing twin
/// would let the entry cover the other when it turns red. A listed id is stale only when no
/// check carrying it failed.
pub(crate) fn sort(expected: &ExpectedRed, names: &Names, failed: &[Failure]) -> Result<Sorted> {
    let mut names = names.clone();
    for (name, _) in failed {
        record(&mut names, name);
    }
    if let Some((id, twins)) = names.iter().find(|(_, twins)| twins.len() > 1) {
        bail!("two different checks carry the id {id}: {twins:?}; give each its own id");
    }
    let mut out = Sorted::default();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for (name, detail) in failed {
        let Some((id, entry)) = check_id(name).and_then(|id| expected.get_key_value(id)) else {
            out.new.push((name.clone(), detail.clone()));
            continue;
        };
        seen.insert(id.as_str());
        let missing = entry.missing(detail);
        if missing.is_empty() {
            out.expected.push((name.clone(), entry.reason.clone()));
        } else {
            out.changed.push(Changed { name: name.clone(), detail: detail.clone(), missing, reason: entry.reason.clone() });
        }
    }
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
