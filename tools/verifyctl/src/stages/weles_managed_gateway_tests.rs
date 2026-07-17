//! The comparator half of the proof (the live half is the stage itself, which
//! runs the real decoy against a dead port and observes).
//!
//! These tests stage the wrong world and require the verdict to name it. What
//! they are guarding is that each assertion fails for its OWN reason and that a
//! defect in one cannot be masked by another — the previously-wrong branch here
//! is not "the fleet works", it is "the gate would have said PASS anyway".

use super::*;

/// Everything green: the fleet answered, the decoy died in the managed path, and
/// the swap probe showed both resolved addresses being USED.
fn healthy() -> Observed {
    Observed {
        decoy: DecoyRun {
            end: DecoyEnd::Exited(1),
            logs: format!(
                "Error: cannot resolve CHARACTERS_EDGE_ADDR (\"characters\", Edge) from the \
                 orchestrator: orchestrator did not answer: error sending request. Managed boot \
                 {MANAGED_FAILURE_EVIDENCE} — the modes are disjoint."
            ),
        },
        readyz: Ok(200),
        leaderboard: Ok(200),
        passthrough: Ok(200),
        swap: Ok(SwapProbe {
            // Serving, NOT ready: this probe sabotages one peer on purpose, and
            // every Stub contributes a readiness probe — 503 is the designed
            // answer here, and it is still an answer.
            serving: Ok(503),
            origin_marker: Ok(true),
            // The swapped edge points at a port serving no QUIC, so the op MUST
            // fail — and a datagram must have arrived there.
            leaderboard: Ok(502),
            edge_dialled: true,
        }),
    }
}

#[test]
fn a_fleet_that_proved_managed_mode_yields_no_findings() {
    assert!(findings(&healthy()).is_empty());
}

#[test]
fn each_assertion_fails_alone_and_for_its_own_reason() {
    // One wrong answer at a time: exactly one finding, and it names that probe.
    // A comparator that folded them together (or short-circuited) would show up
    // here as a count or a name that does not match.
    /// One staged defect: how to break the healthy world, and the probe the
    /// verdict must name for it.
    type Case = (&'static str, &'static dyn Fn(&mut Observed), &'static str);
    let cases: [Case; 3] = [
        ("readyz", &|o: &mut Observed| o.readyz = Ok(503), "/readyz"),
        (
            "leaderboard",
            &|o: &mut Observed| o.leaderboard = Ok(401),
            "/leaderboard",
        ),
        (
            "passthrough",
            &|o: &mut Observed| o.passthrough = Ok(404),
            "/admin/login",
        ),
    ];
    for (label, break_it, marker) in cases {
        let mut observed = healthy();
        break_it(&mut observed);
        let findings = findings(&observed);
        assert_eq!(findings.len(), 1, "{label}: {findings:?}");
        assert!(findings[0].contains(marker), "{label}: {findings:?}");
    }
}

#[test]
fn the_fleet_probes_claim_only_what_they_prove() {
    // The bug this stage was rebuilt around was a CLAIM, not a code path: the
    // fleet's /leaderboard and /admin/login are positive controls (the agent's
    // answer and the standalone default are the same bytes), and the message a
    // reader gets must say so. If someone re-inflates these into a
    // used-vs-fetched claim, the next auditor has to re-derive the whole
    // refutation from scratch.
    let mut observed = healthy();
    observed.leaderboard = Ok(500);
    observed.passthrough = Ok(404);
    let findings = findings(&observed);
    assert_eq!(findings.len(), 2, "{findings:?}");
    for finding in &findings {
        assert!(finding.contains("POSITIVE CONTROL"), "{finding}");
    }
    // And the one that DOES carry the claim points at where it lives.
    assert!(findings.iter().all(|f| f.contains("swap_probe")), "{findings:?}");
}

#[test]
fn a_transport_failure_is_a_finding_not_a_pass() {
    // `Probe` is a Result: an assertion that never got an answer must never be
    // read as an answer it liked.
    let mut observed = healthy();
    observed.leaderboard = Err("connection refused".into());
    let findings = findings(&observed);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].contains("connection refused"), "{findings:?}");
}

#[test]
fn a_dead_fleet_is_reported_once_per_fact_not_once_per_probe() {
    // The stage does not probe the ops when /readyz never came up; this pins that
    // the verdict stays legible when it does not — three findings for one fact
    // would bury the one that matters.
    let observed = Observed {
        readyz: Err("weles up exited (ExitStatus(1)) before the gateway was ready".into()),
        leaderboard: Err(NOT_PROBED.into()),
        passthrough: Err(NOT_PROBED.into()),
        ..healthy()
    };
    let findings = findings(&observed);
    assert!(findings[0].contains("weles up exited"), "{findings:?}");
    assert_eq!(
        findings.iter().filter(|f| f.contains(NOT_PROBED)).count(),
        2,
        "{findings:?}"
    );
}

#[test]
fn a_decoy_that_survived_without_an_agent_fails_the_stage_even_when_everything_else_is_green() {
    // THE point of the decoy — and the world it actually occurs in. A gateway
    // that fell back to env does NOT exit 0: it serves until signalled
    // (core/app's run loop), so this is `Survived`, established by probing its
    // port. The previous version of this test staged `Some(0)`, a world that
    // cannot happen, while the real regression landed in the "still running" arm
    // and was reported as a hang.
    let mut observed = healthy();
    observed.decoy.end = DecoyEnd::Survived;
    let findings = findings(&observed);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].contains("BOOTED and is SERVING"), "{findings:?}");
    assert!(findings[0].contains("theatre"), "{findings:?}");
}

#[test]
fn each_decoy_end_is_reported_as_its_own_fact() {
    // Five ends, five different accusations. The one that matters: a hang and a
    // boot are BOTH "still running", and only the port probe tells them apart —
    // so they must never share a message, or the reader hunts the wrong defect.
    for (end, marker) in [
        (DecoyEnd::Survived, "BOOTED and is SERVING"),
        (DecoyEnd::Hung, "answered nothing"),
        (DecoyEnd::Interrupted, "interrupted"),
        (DecoyEnd::Signalled, "killed by a signal"),
        (DecoyEnd::Exited(0), "exited 0"),
    ] {
        let mut observed = healthy();
        observed.decoy.end = end;
        let findings = findings(&observed);
        assert_eq!(findings.len(), 1, "{end:?}: {findings:?}");
        assert!(findings[0].contains(marker), "{end:?}: {findings:?}");
    }
}

#[test]
fn an_interrupted_or_signalled_decoy_is_never_an_accusation_against_the_gateway() {
    // Ctrl-C during this stage, or a signal death, must not render as "a managed
    // boot must FAIL, not hang" — that is a false accusation against addrs.rs
    // aimed at whoever reads the log next.
    for end in [DecoyEnd::Interrupted, DecoyEnd::Signalled] {
        let mut observed = healthy();
        observed.decoy.end = end;
        let finding = findings(&observed).remove(0);
        assert!(!finding.contains("must FAIL, not hang"), "{end:?}: {finding}");
        assert!(!finding.contains("addrs.rs"), "{end:?}: {finding}");
    }
}

#[test]
fn a_decoy_that_died_of_something_else_does_not_count_as_proof() {
    // A bare non-zero exit is satisfied by ANY failure to start — a missing DLL
    // would "prove" resolve. The evidence, not the exit code, is what makes the
    // decoy mean something.
    let mut observed = healthy();
    observed.decoy.logs = "Error: could not bind :8082".into();
    let findings = findings(&observed);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].contains("managed-resolve path"), "{findings:?}");
}

// ---------------------------------------------------------------------------
// The swap probe: used-vs-fetched. This is the half the first version of this
// stage got wrong — it claimed /leaderboard 200 on the real fleet proved a
// resolved address was USED, when the agent's answer and the standalone default
// are byte-identical.
// ---------------------------------------------------------------------------

#[test]
fn a_gateway_that_fetches_the_edge_address_and_dials_the_default_is_caught() {
    // THE regression: `managed_addr` "softened" to `answer.unwrap_or(env_default)`.
    // The fake agent sent leaderboard's edge to a port serving no QUIC, so a 200
    // can ONLY have come from dialling 127.0.0.1:9008 — the default. On the real
    // fleet this world is invisible: /readyz, /leaderboard and /admin/login are
    // all green.
    let mut observed = healthy();
    let swap = observed.swap.as_mut().unwrap();
    swap.leaderboard = Ok(200);
    swap.edge_dialled = false;
    let findings = findings(&observed);
    assert!(
        findings.iter().any(|f| f.contains("FETCHED AND DISCARDED")),
        "{findings:?}"
    );
    assert!(findings.iter().any(|f| f.contains("9008")), "{findings:?}");
}

#[test]
fn a_resolved_edge_address_that_nothing_dialled_is_a_finding_even_when_the_op_failed() {
    // The trap in the assertion above, taken alone: the op failing is ALSO what a
    // gateway that dialled nothing at all looks like. Absence of a 200 is not
    // proof; the datagram is the positive half.
    let mut observed = healthy();
    observed.swap.as_mut().unwrap().edge_dialled = false;
    let findings = findings(&observed);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].contains("nothing ever dialled"), "{findings:?}");
}

#[test]
fn a_resolved_http_origin_that_was_not_used_is_a_finding() {
    // The marker can only come from the port the fake agent named. Neither the
    // blank default (route dropped) nor a hypothetical :8085 default (the real
    // admin login page) could produce it.
    let mut observed = healthy();
    observed.swap.as_mut().unwrap().origin_marker = Ok(false);
    let findings = findings(&observed);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].contains("Http ORIGIN was not used"), "{findings:?}");
}

#[test]
fn the_swap_gateway_is_judged_on_serving_not_on_readiness() {
    // The trap, pinned — it cost a live run to find and would silently cost every
    // future one. This probe points a peer at a black hole ON PURPOSE, and every
    // remote::Stub contributes a per-peer readiness probe, so this gateway is
    // /readyz 503 BY DESIGN. A verdict that demanded 200 would fail forever
    // against a process that is up and serving, and blame the gateway for the
    // experiment's own sabotage.
    let mut observed = healthy();
    observed.swap.as_mut().unwrap().serving = Ok(503);
    assert!(findings(&observed).is_empty(), "503 is a designed answer, not a failure");

    // But NO answer is still a finding: that is a gateway that never served.
    observed.swap.as_mut().unwrap().serving = Err("connection refused".into());
    let findings = findings(&observed);
    assert!(findings[0].contains("never served"), "{findings:?}");
    assert!(findings[0].contains("readiness is not"), "{findings:?}");
}

#[test]
fn a_swap_probe_that_could_not_run_is_a_finding_not_a_silent_pass() {
    // A proof that did not execute must never read as a proof that passed — the
    // whole class this stage exists to refuse.
    let mut observed = healthy();
    observed.swap = Err("the fake agent could not bind".into());
    let findings = findings(&observed);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].contains("could not be run"), "{findings:?}");
    assert!(findings[0].contains("same bytes"), "{findings:?}");
}

// ---------------------------------------------------------------------------
// The fake agent. It is a MEASURING INSTRUMENT: if `cmd/gateway-svc` cannot read
// it, the swap probe proves nothing — and it would fail in the most confusing
// possible way (a stage that reports the gateway is broken when the fixture is).
// So it is driven here by the REAL client, over a REAL socket.
// ---------------------------------------------------------------------------

fn fake_agent(swaps: Vec<(String, weles::manifest::AddrKind, String)>) -> fake_http::FakeHttp {
    let real = weles::manifest::PeerAddrs::from_fleet(&weles::manifest::split_fleet());
    fake_http::FakeHttp::start(move |route, body| agent_answer(route, body, &swaps, &real)).unwrap()
}

fn ask(
    agent: &fake_http::FakeHttp,
    provider: &str,
    kind: remote::AddrKind,
) -> std::result::Result<Vec<String>, remote::ResolveError> {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(remote::resolve_peer(
        &format!("http://127.0.0.1:{}", agent.port()),
        provider,
        kind,
    ))
}

#[test]
fn the_real_client_reads_the_stage_fake_agent() {
    // `remote::resolve_peer` is the exact code cmd/gateway-svc runs: its real
    // request bytes (method, path, headers, Content-Length body) into the
    // fixture's parser, and weles's real encoder's bytes back into the client's
    // parser. This is what a unit test staging `Observed` cannot check, and it is
    // the difference between a fixture and a guess.
    let agent = fake_agent(Vec::new());
    let addrs = ask(&agent, "characters", remote::AddrKind::Edge).expect("the client must read it");
    assert_eq!(addrs, vec!["127.0.0.1:9000".to_string()]);
}

#[test]
fn the_fake_agent_swaps_exactly_what_it_was_told_to_and_nothing_else() {
    // The swap is the whole experiment: `leaderboard`'s edge must come back as
    // the stage's port (NOT the 9008 default), while every other answer stays the
    // real fleet's — or the gateway would not boot far enough to dial anything.
    let agent = fake_agent(vec![(
        "leaderboard".to_string(),
        weles::manifest::AddrKind::Edge,
        "127.0.0.1:65001".to_string(),
    )]);
    assert_eq!(
        ask(&agent, "leaderboard", remote::AddrKind::Edge).unwrap(),
        vec!["127.0.0.1:65001".to_string()],
        "the swapped answer must NOT be the 9008 default"
    );
    assert_eq!(
        ask(&agent, "apikeys", remote::AddrKind::Edge).unwrap(),
        vec!["127.0.0.1:9009".to_string()],
        "an unswapped peer must stay the real fleet's address"
    );
    // Both classes of `accounts`, since one provider carrying two kinds is the
    // reason `kind` is on the wire at all.
    assert_eq!(
        ask(&agent, "accounts", remote::AddrKind::Http).unwrap(),
        vec!["127.0.0.1:8084".to_string()]
    );
}

#[test]
fn the_fake_agent_refuses_like_weles_does() {
    // A fixture that answered 200 to everything would hide a gateway that asked
    // the wrong question. `admin` has edge_port: None — the real agent 404s
    // unknown_peer, and so must this.
    let agent = fake_agent(Vec::new());
    match ask(&agent, "admin", remote::AddrKind::Edge) {
        Err(remote::ResolveError::Refused { code, .. }) => {
            assert_eq!(code, remote::ErrorCode::UnknownPeer)
        }
        other => panic!("expected an unknown_peer refusal, got {other:?}"),
    }
}

#[test]
fn the_passthrough_defaults_must_stay_blank() {
    // Load-bearing, and nothing else pins it. `/admin/login` 200 on the real
    // fleet is only a POSITIVE CONTROL now — but ADDR_SPECS' blank default is
    // still what makes an unresolvable origin fail CLOSED (a blank origin is a
    // dropped route → 404) instead of silently proxying somewhere plausible. Give
    // these a "helpful" dev default and managed mode's passthrough failure stops
    // being observable at all.
    let source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cmd/gateway-svc/src/addrs.rs"),
    )
    .expect("read cmd/gateway-svc/src/addrs.rs");
    for provider in ["admin", "accounts"] {
        // The passthrough specs are the only ones whose env_default is empty.
        let spec = source
            .split("AddrSpec {")
            .find(|spec| {
                spec.contains(&format!(r#"provider: "{provider}""#))
                    && spec.contains("AddrClass::Passthrough")
            })
            .unwrap_or_else(|| panic!("no passthrough AddrSpec for {provider}"));
        assert!(
            spec.contains(r#"env_default: """#),
            "{provider}'s passthrough env_default is no longer blank. A blank default is what \
             makes an unresolved origin drop its route (ProxyTable::from_routes) instead of \
             proxying to something plausible — the fail-closed semantics managed mode's \
             unknown_peer arm reproduces exactly. Spec:\n{spec}"
        );
    }
}

#[test]
fn the_evidence_phrase_is_the_one_the_gateway_actually_prints() {
    // This stage reads prose in exactly one place, so it must be the SAME prose.
    // `cmd/gateway-svc` is a binary — its `addrs` module is unreachable from
    // here (no lib target), so the phrase cannot be imported; this pins it
    // against the source instead, which is the closest a reader can get to the
    // real authority.
    let source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cmd/gateway-svc/src/addrs.rs"),
    )
    .expect("read cmd/gateway-svc/src/addrs.rs");
    assert!(
        source.contains(MANAGED_FAILURE_EVIDENCE),
        "cmd/gateway-svc/src/addrs.rs no longer prints {MANAGED_FAILURE_EVIDENCE:?} — the \
         managed-gateway decoy can no longer tell a resolve failure from any other crash. \
         Re-point MANAGED_FAILURE_EVIDENCE at the new wording."
    );
}

#[test]
fn a_reserved_dead_port_is_actually_free() {
    let port = dead_port().unwrap();
    // If this bind fails, `dead_port` handed back something still listening and
    // the decoy would be dialling a live socket.
    TcpListener::bind(("127.0.0.1", port)).expect("the reserved port must be free again");
}
