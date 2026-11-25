//! The run's record: per-kind seed tallies, every call per op (declared success, other expected
//! status, or failed), every error and skip, and the catalog coverage computed from them at the
//! end (I3): only an answer equal to the op's `OpInfo::success` covers it. Rendering it decides
//! the exit code (I8): any error or any uncovered op fails the run.

use std::collections::BTreeMap;

use opscatalog::OpInfo;

use crate::SPLIT_ADMIN_URL;

/// How many characters of a response body an error keeps.
pub const EXCERPT_CHARS: usize = 400;

pub fn excerpt(text: &str) -> String {
    text.chars().take(EXCERPT_CHARS).collect()
}

/// How a seed call that answered one of its expected statuses counts: a 409 the convergent seed
/// accepts means the row was already there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Created,
    AlreadyExists,
}

impl Outcome {
    pub fn from_status(status: u16) -> Outcome {
        if status == 409 {
            Outcome::AlreadyExists
        } else {
            Outcome::Created
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub created: usize,
    pub existing: usize,
}

impl Tally {
    pub fn record(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Created => self.created += 1,
            Outcome::AlreadyExists => self.existing += 1,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub players: Tally,
    pub characters: Tally,
    pub grants: Tally,
    pub saves: Tally,
    pub matches: Tally,
    pub friendships: Tally,
    pub groups: Tally,
    pub memberships: Tally,
}

impl Summary {
    pub fn render(&self, base_url: &str) -> String {
        let row = |kind: &str, tally: &Tally, created: &str, existing: &str| {
            format!("  {kind:<12} {:>4} {created:<10} {:>4} {existing}\n", tally.created, tally.existing)
        };
        let mut out = String::from("demoseed summary\n");
        out += &row("players", &self.players, "created", "already registered");
        out += &row("characters", &self.characters, "created", "already present");
        out += &row("inventory", &self.grants, "granted", "already at target");
        out += &row("saves", &self.saves, "created", "already saved");
        out += &row("matches", &self.matches, "sent", "bound to another result (409)");
        out += "  (an identical match re-send is a 202 no-op, so re-runs still count it as sent)\n";
        out += &row("friendships", &self.friendships, "accepted", "already friends");
        out += &row("groups", &self.groups, "created", "already present");
        out += &row("memberships", &self.memberships, "added", "already held");
        out += "\nAdmin panel (admin/admin):\n";
        out += &format!("  monolith  {base_url}/admin\n");
        out += &format!("  split     {SPLIT_ADMIN_URL}\n");
        out
    }
}

/// What a failed call got instead of an expected status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Got {
    Status(u16),
    Transport(String),
    /// An expected status whose body is not the shape the flow needs.
    Shape(String),
    /// An event-fed read that never showed what it waited for before the deadline (I7).
    Timeout(String),
}

impl std::fmt::Display for Got {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Got::Status(status) => write!(f, "HTTP {status}"),
            Got::Transport(cause) => write!(f, "transport error: {cause}"),
            Got::Shape(cause) => write!(f, "shape error: {cause}"),
            Got::Timeout(cause) => write!(f, "timed out: {cause}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallError {
    pub scenario: String,
    pub op: &'static str,
    pub method: &'static str,
    pub path: String,
    /// Empty for a shape error, whose status was expected.
    pub expected: Vec<u16>,
    pub got: Got,
    pub excerpt: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub scenario: String,
    pub cause: String,
}

/// How one call of an op counts: `Success` answered the op's declared `OpInfo::success` and
/// covers it; `Expected` answered another status the caller listed (a convergent 409, a
/// deliberate refusal) and is neither an error nor coverage; `Failed` is in the errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Success,
    Expected,
    Failed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpCalls {
    pub succeeded: usize,
    pub expected: usize,
    pub failed: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub summary: Summary,
    pub calls: BTreeMap<&'static str, OpCalls>,
    pub errors: Vec<CallError>,
    pub skipped: Vec<Skipped>,
}

/// The catalog ops no call of this run covered: `failed_only` were called but never answered
/// their declared success and failed at least once (their errors are in the report);
/// `uncovered` were never called or only answered another expected status (e.g. a 409).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    pub failed_only: Vec<&'static str>,
    pub uncovered: Vec<&'static str>,
}

pub struct Rendered {
    pub text: String,
    pub failed: bool,
}

impl Report {
    pub fn record_call(&mut self, op: &'static str, answer: Answer) {
        let calls = self.calls.entry(op).or_default();
        match answer {
            Answer::Success => calls.succeeded += 1,
            Answer::Expected => calls.expected += 1,
            Answer::Failed => calls.failed += 1,
        }
    }

    pub fn skip(&mut self, scenario: String, cause: String) {
        self.skipped.push(Skipped { scenario, cause });
    }

    pub fn coverage(&self, catalog: &[OpInfo]) -> Coverage {
        let mut coverage = Coverage::default();
        for row in catalog {
            match self.calls.get(row.method) {
                Some(calls) if calls.succeeded > 0 => {}
                Some(calls) if calls.failed > 0 => coverage.failed_only.push(row.method),
                _ => coverage.uncovered.push(row.method),
            }
        }
        coverage
    }

    /// The seed tallies, then the errors grouped by op, the skips, and the ops the run did not
    /// cover; `failed` when there is any error or any op without a call answering its success.
    pub fn render(&self, base_url: &str, catalog: &[OpInfo]) -> Rendered {
        let coverage = self.coverage(catalog);
        let mut text = self.summary.render(base_url);
        let mut by_op: BTreeMap<&str, Vec<&CallError>> = BTreeMap::new();
        for error in &self.errors {
            by_op.entry(error.op).or_default().push(error);
        }
        if !self.errors.is_empty() {
            text += &format!("\nerrors ({}):\n", self.errors.len());
        }
        for (op, errors) in &by_op {
            text += &format!("  {op} ({})\n", errors.len());
            for error in errors {
                text += &format!("    [{}] {} {}", error.scenario, error.method, error.path);
                if !error.expected.is_empty() {
                    text += &format!(" expected {:?}", error.expected);
                }
                text += &format!(", got {}", error.got);
                if let Some(hint) = hint(error) {
                    text += &format!(" ({hint})");
                }
                if !error.excerpt.is_empty() {
                    text += &format!(": {}", error.excerpt);
                }
                text += "\n";
            }
        }
        if !self.skipped.is_empty() {
            text += &format!("\nskipped ({}):\n", self.skipped.len());
            for skipped in &self.skipped {
                text += &format!("  [{}] {}\n", skipped.scenario, skipped.cause);
            }
        }
        if !coverage.failed_only.is_empty() {
            text += &format!("\nops called but never answering their declared success: {}\n", coverage.failed_only.join(", "));
        }
        if !coverage.uncovered.is_empty() {
            text += &format!("\nops not covered (no call answered the declared success): {}\n", coverage.uncovered.join(", "));
        }
        let failed = !self.errors.is_empty() || !coverage.failed_only.is_empty() || !coverage.uncovered.is_empty();
        text += &if failed {
            format!(
                "\ndemoseed: FAILED ({} errors, {} ops not covered)\n",
                self.errors.len(),
                coverage.failed_only.len() + coverage.uncovered.len()
            )
        } else {
            format!("\ndemoseed: OK, all {} catalog ops covered\n", catalog.len())
        };
        Rendered { text, failed }
    }
}

/// The dev flag a 404 from a dev-gated route points at.
fn hint(error: &CallError) -> Option<&'static str> {
    match (error.op, &error.got) {
        ("accounts.register", Got::Status(404)) => Some("a 404 means the fleet runs without ACCOUNTS_OPEN_REGISTRATION"),
        ("inventory.grant", Got::Status(404)) => Some("a 404 means the fleet runs without INVENTORY_DEV_GRANT"),
        _ => None,
    }
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
