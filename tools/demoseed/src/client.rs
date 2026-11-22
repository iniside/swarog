//! The one call path to the gateway. A call names its `opscatalog` op; the verb and the path
//! template come from that op's `OPERATIONS` row, so the name and the route cannot disagree.
//! Every outcome is recorded into the shared [`Report`]: only the op's declared
//! `OpInfo::success` counts it as covered, another status in the caller's expect list (a
//! convergent 409, a deliberate refusal) is neither coverage nor an error, and anything else
//! (after 429 retries) is an error, never an abort.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use opscatalog::{OpInfo, OPERATIONS};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::Semaphore;

use crate::report::{excerpt, Answer, CallError, Got, Report, Summary};
use crate::{CLIENT_KEY, MAX_ATTEMPTS, REQUEST_TIMEOUT, SERVER_KEY};

/// The identity a call presents: every call sends an API key, a player call adds its bearer.
#[derive(Clone, Copy, Debug)]
pub enum Caller<'a> {
    Anonymous,
    Player(&'a str),
    /// The full dev-server key, the only one whose policy allows `match.report`.
    Server,
}

/// A call's outcome. `Ok` carries a status from the expect list and the body (JSON for a 2xx,
/// the plain-text error body as a string otherwise); `Failed` is already in the report.
#[derive(Clone, Debug, PartialEq)]
pub enum CallResult {
    Ok { status: u16, body: Value },
    Failed,
}

impl CallResult {
    pub fn ok(self) -> Option<(u16, Value)> {
        match self {
            CallResult::Ok { status, body } => Some((status, body)),
            CallResult::Failed => None,
        }
    }
}

/// What to do with one answered status on attempt `attempt` (0-based).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Accept,
    Retry(Duration),
    Reject,
}

/// The status authority: a listed status is accepted, an unlisted 429 is retried with backoff
/// until the last attempt, anything else is rejected.
pub fn decide(status: u16, expect: &[u16], attempt: u32) -> Decision {
    if expect.contains(&status) {
        Decision::Accept
    } else if status == 429 && attempt + 1 < MAX_ATTEMPTS {
        Decision::Retry(Duration::from_millis(250 * u64::from(attempt + 1).min(8)))
    } else {
        Decision::Reject
    }
}

/// The catalog row of `op`. An op name absent from the catalog is a programming error.
pub fn route(op: &str) -> &'static OpInfo {
    OPERATIONS
        .iter()
        .find(|row| row.method == op)
        .unwrap_or_else(|| panic!("demoseed: op {op:?} is not in opscatalog::OPERATIONS"))
}

/// Checks an expect list against the op's declared success: a list that accepts a 2xx accepts
/// exactly the declared one, so a hand copy of `#[http(success = …)]` cannot drift. A list with
/// no 2xx expects a refusal on purpose. A mismatch is a programming error.
pub fn check_expect(op: &str, row: &OpInfo, expect: &[u16]) {
    if let Some(other) = expect.iter().find(|s| (200..300).contains(*s) && **s != row.success) {
        panic!("demoseed: {op} expects {other}, but its declared success is {}", row.success);
    }
}

/// Fills every `{name}` segment of `template` from `params`. A missing or an extra param is a
/// programming error.
pub fn fill_path(op: &str, template: &str, params: &[(&str, &str)]) -> String {
    let mut used = vec![false; params.len()];
    let mut path = String::with_capacity(template.len());
    for segment in template.split('/').skip(1) {
        path.push('/');
        match segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            Some(name) => {
                let index = params
                    .iter()
                    .position(|(key, _)| *key == name)
                    .unwrap_or_else(|| panic!("demoseed: {op} {template}: missing path param {name:?}"));
                used[index] = true;
                path.push_str(params[index].1);
            }
            None => path.push_str(segment),
        }
    }
    if let Some(index) = used.iter().position(|u| !u) {
        panic!("demoseed: {op} {template}: extra path param {:?}", params[index].0);
    }
    path
}

struct Shared {
    http: reqwest::Client,
    base: String,
    permits: Semaphore,
    report: Mutex<Report>,
}

/// A handle on the shared client, in-flight bound and report, named for one scenario: every
/// error it records carries that name. Cloning or renaming shares the same report.
#[derive(Clone)]
pub struct Gateway {
    shared: Arc<Shared>,
    scenario: Arc<str>,
}

impl Gateway {
    pub fn new(base: &str, concurrency: usize) -> Result<Self> {
        let http = reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build().context("build HTTP client")?;
        Ok(Gateway {
            shared: Arc::new(Shared {
                http,
                base: base.to_string(),
                permits: Semaphore::new(concurrency),
                report: Mutex::new(Report::default()),
            }),
            scenario: Arc::from("startup"),
        })
    }

    pub fn scenario(&self, name: impl Into<String>) -> Gateway {
        Gateway { shared: Arc::clone(&self.shared), scenario: Arc::from(name.into()) }
    }

    /// One `GET /healthz` before the run: the only failure that ends the run early.
    pub async fn probe(&self) -> Result<()> {
        let url = format!("{}/healthz", self.shared.base);
        match self.shared.http.get(&url).send().await {
            Ok(response) if response.status().is_success() => Ok(()),
            Ok(response) => bail!("demoseed: {url} answered HTTP {}; is a fleet running there?", response.status()),
            Err(err) => bail!("demoseed: {url} is unreachable ({err}); is a fleet running there?"),
        }
    }

    pub async fn call(
        &self,
        op: &'static str,
        path_params: &[(&str, &str)],
        caller: Caller<'_>,
        body: Option<&Value>,
        expect: &[u16],
    ) -> CallResult {
        let row = route(op);
        check_expect(op, row, expect);
        let path = fill_path(op, row.path, path_params);
        let method = Method::from_bytes(row.verb.as_bytes())
            .unwrap_or_else(|_| panic!("demoseed: {op} has an invalid verb {:?}", row.verb));
        let url = format!("{}{path}", self.shared.base);
        let mut attempt = 0;
        loop {
            let answered = {
                let _permit = self.shared.permits.acquire().await.expect("the semaphore is never closed");
                self.send(method.clone(), &url, caller, body).await
            };
            let (status, text) = match answered {
                Ok(answered) => answered,
                Err(cause) => {
                    self.record_failure(op, row, path, expect, Got::Transport(cause), String::new());
                    return CallResult::Failed;
                }
            };
            match decide(status, expect, attempt) {
                Decision::Accept => {
                    let answer = if status == row.success { Answer::Success } else { Answer::Expected };
                    self.with_report(|report| report.record_call(op, answer));
                    if !(200..300).contains(&status) {
                        return CallResult::Ok { status, body: Value::String(text) };
                    }
                    if text.is_empty() {
                        return CallResult::Ok { status, body: Value::Null };
                    }
                    return match serde_json::from_str(&text) {
                        Ok(body) => CallResult::Ok { status, body },
                        Err(err) => {
                            self.shape_error_with(op, format!("HTTP {status} body is not JSON: {err}"), &text);
                            CallResult::Failed
                        }
                    };
                }
                Decision::Retry(backoff) => tokio::time::sleep(backoff).await,
                Decision::Reject => {
                    self.record_failure(op, row, path, expect, Got::Status(status), excerpt(&text));
                    return CallResult::Failed;
                }
            }
            attempt += 1;
        }
    }

    async fn send(&self, method: Method, url: &str, caller: Caller<'_>, body: Option<&Value>) -> Result<(u16, String), String> {
        let key = match caller {
            Caller::Server => SERVER_KEY,
            Caller::Anonymous | Caller::Player(_) => CLIENT_KEY,
        };
        let mut request = self.shared.http.request(method, url).header("X-Api-Key", key);
        if let Caller::Player(token) = caller {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|err| format!("no response: {err}"))?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(|err| format!("HTTP {status}, body unreadable: {err}"))?;
        Ok((status, text))
    }

    /// Turns a success body into the op's local shape, or records a shape error for the op.
    pub fn typed<T: DeserializeOwned>(&self, op: &'static str, body: Value) -> Option<T> {
        match serde_json::from_value::<T>(body.clone()) {
            Ok(value) => Some(value),
            Err(err) => {
                self.shape_error_with(op, format!("unexpected response shape: {err}"), &body.to_string());
                None
            }
        }
    }

    /// Records a well-formed answer whose content is wrong for the flow (an unknown state, a row
    /// that should exist) as an error of `op`.
    pub fn shape_error(&self, op: &'static str, message: String) {
        self.shape_error_with(op, message, "");
    }

    fn shape_error_with(&self, op: &'static str, message: String, text: &str) {
        let row = route(op);
        self.push_error(op, row, row.path.to_string(), &[], Got::Shape(message), excerpt(text));
    }

    fn record_failure(&self, op: &'static str, row: &'static OpInfo, path: String, expect: &[u16], got: Got, text: String) {
        self.with_report(|report| report.record_call(op, Answer::Failed));
        self.push_error(op, row, path, expect, got, text);
    }

    fn push_error(&self, op: &'static str, row: &'static OpInfo, path: String, expect: &[u16], got: Got, text: String) {
        let error = CallError {
            scenario: self.scenario.to_string(),
            op,
            method: row.verb,
            path,
            expected: expect.to_vec(),
            got,
            excerpt: text,
        };
        self.with_report(|report| report.errors.push(error));
    }

    /// Records that this scenario skipped a dependent step, and why.
    pub fn skip(&self, cause: impl Into<String>) {
        let scenario = self.scenario.to_string();
        let cause = cause.into();
        self.with_report(|report| report.skip(scenario, cause));
    }

    pub fn tally(&self, update: impl FnOnce(&mut Summary)) {
        self.with_report(|report| update(&mut report.summary));
    }

    pub fn take_report(&self) -> Report {
        self.with_report(std::mem::take)
    }

    fn with_report<R>(&self, f: impl FnOnce(&mut Report) -> R) -> R {
        let mut report = self.shared.report.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut report)
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
pub(crate) mod tests;
