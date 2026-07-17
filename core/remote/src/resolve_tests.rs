//! Tests for the `resolve` client, against a fake agent THIS crate stands up.
//!
//! `remote` is in the shipping graph, so it may not dev-dep `weles`: nothing
//! here can observe the real server, and nothing here proves the two sides
//! agree about the wire (see [`super`]'s doc — that is the live
//! `weles-managed-gateway` stage's job, and only its). What these DO prove is
//! everything that is this side's own: that a caller can branch the two 404s
//! without touching prose, that an empty list is not an unknown peer, and that
//! an agent that is absent or babbling produces a typed error rather than a
//! hang or a panic.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use super::{resolve_peer_within, AddrKind, ErrorCode, ResolveError};

/// Every test's outer bound: a `resolve` that hangs must fail the test, not
/// wedge the suite. Deliberately far above every budget under test, so it can
/// only fire on a real hang and never on a slow machine.
const HANG_GUARD: Duration = Duration::from_secs(10);

/// What a test lets `resolve_peer` spend, when the point is the budget itself.
const TEST_BUDGET: Duration = Duration::from_millis(300);

// ---------------------------------------------------------------------------
// The fake agent: a raw HTTP/1.1 canner. Not a mock of weles — a stand-in for
// "something at that URL", which is all this side can honestly test against.
// ---------------------------------------------------------------------------

/// Answers one request. `None` = never answer at all (the hung-agent case).
type Answer = Arc<dyn Fn(&Seen) -> Option<(u16, String)> + Send + Sync>;

/// One request as it actually arrived.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Seen {
    /// `POST /resolve` — the request line's method and target.
    target: String,
    body: String,
}

struct FakeAgent {
    addr: SocketAddr,
    /// Every request the client actually sent, so a test can pin what went on
    /// the wire rather than only what came back.
    seen: Arc<Mutex<Vec<Seen>>>,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeAgent {
    async fn start(answer: Answer) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind fake agent");
        let addr = listener.local_addr().expect("fake agent addr");
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
        let (shutdown, mut stop) = oneshot::channel();
        let task = tokio::spawn({
            let seen = Arc::clone(&seen);
            async move {
                loop {
                    tokio::select! {
                        _ = &mut stop => break,
                        accepted = listener.accept() => {
                            let Ok((stream, _)) = accepted else { break };
                            let answer = Arc::clone(&answer);
                            let seen = Arc::clone(&seen);
                            tokio::spawn(async move { serve_one(stream, answer, seen).await });
                        }
                    }
                }
            }
        });
        Self { addr, seen, shutdown: Some(shutdown), task }
    }

    /// A canner that answers every request the same way.
    async fn always(status: u16, body: impl Into<String>) -> Self {
        let body = body.into();
        Self::start(Arc::new(move |_| Some((status, body.clone())))).await
    }

    /// A canner that accepts the connection and never answers.
    async fn silent() -> Self {
        Self::start(Arc::new(|_| None)).await
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().expect("fake agent requests").clone()
    }
}

impl Drop for FakeAgent {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        // Kills any connection task still parked (the silent case parks
        // forever by design), so no test leaks a listener into the next.
        self.task.abort();
    }
}

async fn serve_one(mut stream: TcpStream, answer: Answer, seen: Arc<Mutex<Vec<Seen>>>) {
    let Some(request) = read_request(&mut stream).await else { return };
    seen.lock().expect("record request").push(request.clone());
    let Some((status, reply)) = answer(&request) else {
        // The hung agent: hold the connection open and never write. The client
        // must decide on its own budget, not on ours.
        std::future::pending::<()>().await;
        return;
    };
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        reason(status),
        reply.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(reply.as_bytes()).await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}

/// Reads headers, then exactly `Content-Length` body bytes. Enough for one
/// well-formed reqwest request; anything else is a bug in the test, not a case
/// to handle.
async fn read_request(stream: &mut TcpStream) -> Option<Seen> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    let head_end = loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(at) = find(&buffer, b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    // `POST /resolve HTTP/1.1` → `POST /resolve`.
    let target = head
        .split("\r\n")
        .next()
        .map(|line| line.rsplit_once(' ').map(|(start, _)| start).unwrap_or(line).to_string())
        .unwrap_or_default();
    let length: usize = head
        .split("\r\n")
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length").then_some(value)
        })
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    while buffer.len() < head_end + length {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body = String::from_utf8_lossy(&buffer[head_end..head_end + length]).into_owned();
    Some(Seen { target, body })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

// ---------------------------------------------------------------------------
// The caller's branch, written the way `cmd/gateway-svc` must be able to write
// it: on the TYPE, never on a message.
// ---------------------------------------------------------------------------

/// What a Step-4-shaped caller concludes. `Verdict` deliberately keeps the two
/// 404s apart, because the caller's policies for them differ: an agent that
/// does not speak the contract is always fatal, while "not in this topology" is
/// fatal for an edge peer and merely a blank origin for a passthrough.
#[derive(Debug, Eq, PartialEq)]
enum Verdict {
    Addrs(Vec<String>),
    AgentDoesNotSpeakTheContract,
    NotInThisTopology,
    AskedWrong,
    AgentsOwnFault,
    NoAnswer,
    NotThisContract,
}

/// The whole branch a caller writes. If this function ever needs a `contains()`
/// on the prose, the client's error type has failed its only job.
fn branch(result: Result<Vec<String>, ResolveError>) -> Verdict {
    match result {
        Ok(addrs) => Verdict::Addrs(addrs),
        Err(ResolveError::Refused { code, .. }) => match code {
            ErrorCode::UnknownRoute => Verdict::AgentDoesNotSpeakTheContract,
            ErrorCode::UnknownPeer => Verdict::NotInThisTopology,
            ErrorCode::BadRequest => Verdict::AskedWrong,
            ErrorCode::Internal => Verdict::AgentsOwnFault,
        },
        Err(ResolveError::Unreachable(_)) => Verdict::NoAnswer,
        Err(ResolveError::Malformed(_)) => Verdict::NotThisContract,
    }
}

async fn ask(url: &str, provider: &str, kind: AddrKind) -> Verdict {
    let asked = tokio::time::timeout(HANG_GUARD, resolve_peer_within(url, provider, kind, TEST_BUDGET))
        .await
        .expect("resolve_peer hung past the guard");
    branch(asked)
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_resolved_peer_answers_its_addresses_and_asks_the_contract_question() {
    let agent = FakeAgent::always(200, r#"{"addrs":["127.0.0.1:9000"]}"#).await;

    let verdict = ask(&agent.url(), "characters", AddrKind::Edge).await;

    assert_eq!(verdict, Verdict::Addrs(vec!["127.0.0.1:9000".to_string()]));
    // The question that went out, not just the answer that came back: the verb
    // is a POST to /resolve, the provider is the SHORT name, and `kind` is
    // spelled the way the server's `AddrKind` derive reads it. (That the two
    // spellings MATCH is unprovable here — see the module doc.)
    assert_eq!(
        agent.requests(),
        vec![Seen {
            target: "POST /resolve".to_string(),
            body: r#"{"provider":"characters","kind":"edge"}"#.to_string(),
        }]
    );
}

#[tokio::test]
async fn the_http_kind_is_a_different_question_from_the_edge_kind() {
    let agent = FakeAgent::always(200, r#"{"addrs":["127.0.0.1:8084"]}"#).await;

    let verdict = ask(&agent.url(), "accounts", AddrKind::Http).await;

    assert_eq!(verdict, Verdict::Addrs(vec!["127.0.0.1:8084".to_string()]));
    // `accounts` is BOTH kinds at once (edge 9003, http 8084), so the kind must
    // ride on the wire; a question keyed on the provider alone could not tell
    // these two apart.
    assert_eq!(
        agent.requests()[0].body,
        r#"{"provider":"accounts","kind":"http"}"#.to_string()
    );
}

/// The load-bearing one. Both agents answer **404 with byte-identical prose**,
/// so a client that string-matched the message could not tell them apart — and
/// would read "this agent predates the verb" as "admin has no origin". Only the
/// code differs, and only the code is branched.
#[tokio::test]
async fn the_two_404s_are_branchable_by_type_with_identical_prose() {
    // ONE prose string, used by both agents: the premise is structural, not
    // asserted. Nothing but the code differs between these two answers.
    let prose = "not found";
    let stale_agent =
        FakeAgent::always(404, format!(r#"{{"code":"unknown_route","error":"{prose}"}}"#)).await;
    let live_agent =
        FakeAgent::always(404, format!(r#"{{"code":"unknown_peer","error":"{prose}"}}"#)).await;

    let stale = ask(&stale_agent.url(), "characters", AddrKind::Edge).await;
    let live = ask(&live_agent.url(), "characters", AddrKind::Edge).await;

    assert_eq!(stale, Verdict::AgentDoesNotSpeakTheContract);
    assert_eq!(live, Verdict::NotInThisTopology);
    assert_ne!(stale, live, "the two 404s must not collapse into one answer");
}

/// `200 {"addrs":[]}` is *"it is a thing; nothing is live right now"* — a
/// liveness answer a caller may not treat as final. `404 unknown_peer` is
/// *"not a thing in this topology"*, which it may. Collapsing them is the exact
/// thing the design forbids.
#[tokio::test]
async fn an_empty_addr_list_is_not_the_unknown_peer_answer() {
    let nothing_live = FakeAgent::always(200, r#"{"addrs":[]}"#).await;
    let not_a_thing = FakeAgent::always(404, r#"{"code":"unknown_peer","error":"no such peer"}"#).await;

    let empty = ask(&nothing_live.url(), "characters", AddrKind::Edge).await;
    let unknown = ask(&not_a_thing.url(), "characters", AddrKind::Edge).await;

    assert_eq!(empty, Verdict::Addrs(Vec::new()));
    assert_eq!(unknown, Verdict::NotInThisTopology);
    assert_ne!(empty, unknown);
}

#[tokio::test]
async fn an_absent_orchestrator_is_a_typed_error_not_a_hang() {
    // A port that was bound and released: the connect is REFUSED promptly
    // rather than blackholed, so this pins the transport-error branch (the
    // budget branch is the silent-agent test's job).
    let addr = {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        listener.local_addr().expect("addr")
    };

    let verdict = ask(&format!("http://{addr}"), "characters", AddrKind::Edge).await;

    assert_eq!(verdict, Verdict::NoAnswer);
}

/// The branch that used to be a hang: an agent that accepts the connection and
/// then says nothing at all. The client's own budget must end it — the hang
/// guard fires only if it does not.
#[tokio::test]
async fn an_agent_that_never_answers_fails_inside_its_own_budget() {
    let agent = FakeAgent::silent().await;

    let started = std::time::Instant::now();
    let verdict = ask(&agent.url(), "characters", AddrKind::Edge).await;

    assert_eq!(verdict, Verdict::NoAnswer);
    assert!(
        started.elapsed() < HANG_GUARD,
        "the budget, not the guard, must be what ended this call"
    );
    // The request DID reach the agent, so this is a real hang on the answer and
    // not a connection that never happened.
    assert_eq!(agent.requests().len(), 1);
}

#[tokio::test]
async fn a_2xx_that_is_not_the_answer_shape_is_typed_not_a_panic() {
    for body in [r#"{"peers":["127.0.0.1:9000"]}"#, "not json at all", "", "[]"] {
        let agent = FakeAgent::always(200, body).await;

        let verdict = ask(&agent.url(), "characters", AddrKind::Edge).await;

        assert_eq!(verdict, Verdict::NotThisContract, "body {body:?}");
    }
}

#[tokio::test]
async fn a_refusal_without_the_envelope_is_not_guessed_at() {
    for (status, body) in [(500_u16, "<html>gateway blew up</html>"), (404, ""), (400, "{}")] {
        let agent = FakeAgent::always(status, body).await;

        let verdict = ask(&agent.url(), "characters", AddrKind::Edge).await;

        // NOT a refusal with an invented code — a code we did not read is a
        // code we do not have.
        assert_eq!(verdict, Verdict::NotThisContract, "{status} {body:?}");
    }
}

/// A code outside the closed contract means the agent is not speaking THIS
/// contract. It must never be tolerated into the nearest known code — silently
/// nearest-matching `unknown_something` onto `unknown_peer` is the same
/// collapse the two-404 test forbids, arriving by another door.
#[tokio::test]
async fn a_code_outside_the_contract_is_not_read_as_a_known_refusal() {
    let agent = FakeAgent::always(404, r#"{"code":"unknown_planet","error":"???"}"#).await;

    let verdict = ask(&agent.url(), "characters", AddrKind::Edge).await;

    assert_eq!(verdict, Verdict::NotThisContract);
}

#[tokio::test]
async fn the_other_two_contract_codes_survive_as_themselves() {
    let asked_wrong = FakeAgent::always(400, r#"{"code":"bad_request","error":"parse"}"#).await;
    let their_fault = FakeAgent::always(500, r#"{"code":"internal","error":"oops"}"#).await;

    assert_eq!(ask(&asked_wrong.url(), "characters", AddrKind::Edge).await, Verdict::AskedWrong);
    assert_eq!(ask(&their_fault.url(), "characters", AddrKind::Edge).await, Verdict::AgentsOwnFault);
}

/// A URL with a trailing slash is the same URL. `ORCHESTRATOR_URL` is written
/// by a human in a manifest, and `http://127.0.0.1:8099//resolve` would be a
/// 404 `unknown_route` — i.e. a spelling slip would surface as "the agent does
/// not speak the contract", which is a lie about the agent.
#[tokio::test]
async fn a_trailing_slash_in_the_url_is_not_a_different_endpoint() {
    let agent = FakeAgent::always(200, r#"{"addrs":["127.0.0.1:9000"]}"#).await;

    let verdict = ask(&format!("{}/", agent.url()), "characters", AddrKind::Edge).await;

    assert_eq!(verdict, Verdict::Addrs(vec!["127.0.0.1:9000".to_string()]));
    assert_eq!(agent.requests()[0].target, "POST /resolve", "the path must not double its slash");
}
