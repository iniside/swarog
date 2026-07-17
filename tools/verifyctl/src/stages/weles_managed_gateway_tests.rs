//! The comparator half of the proof (the live half is the stage itself, which
//! runs the real decoy against a dead port and observes).
//!
//! These tests stage the wrong world and require the verdict to name it. What
//! they are guarding is that each assertion fails for its OWN reason and that a
//! defect in one cannot be masked by another — the previously-wrong branch here
//! is not "the fleet works", it is "the gate would have said PASS anyway".

use super::*;

/// Everything green: the fleet answered, and the decoy died in the managed path.
fn healthy() -> Observed {
    Observed {
        decoy: DecoyRun {
            exit_code: Some(1),
            logs: format!(
                "Error: cannot resolve CHARACTERS_EDGE_ADDR (\"characters\", Edge) from the \
                 orchestrator: orchestrator did not answer: error sending request. Managed boot \
                 {MANAGED_FAILURE_EVIDENCE} — the modes are disjoint."
            ),
        },
        readyz: Ok(200),
        leaderboard: Ok(200),
        passthrough: Ok(200),
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
fn a_passthrough_404_is_reported_as_the_blank_origin_symptom_not_a_generic_miss() {
    // The Http class's failure is silent by construction: an origin that never
    // arrived is a blank string, and a blank origin is a DROPPED route, not an
    // error. So the verdict must say which, or the next reader spends the outage
    // looking at admin-svc.
    let mut observed = healthy();
    observed.passthrough = Ok(404);
    let finding = findings(&observed).remove(0);
    assert!(finding.contains("blank-origin"), "{finding}");
    assert!(finding.contains("404"), "{finding}");
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
fn a_decoy_that_booted_without_an_agent_fails_the_stage_even_when_everything_else_is_green() {
    // THE point of the decoy. This is the world where the fleet is healthy, all
    // three assertions answer 200 — and none of it depends on resolve. Before the
    // decoy existed, this world was indistinguishable from a real proof; the
    // stage must now refuse it.
    let mut observed = healthy();
    observed.decoy.exit_code = Some(0);
    let findings = findings(&observed);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].contains("BOOTED"), "{findings:?}");
    assert!(findings[0].contains("theatre"), "{findings:?}");
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

#[test]
fn a_decoy_that_hung_is_a_finding() {
    // Managed boot must FAIL on an unresolvable edge peer, not wait: there is no
    // benign value to wait for. A hang would also silently eat the stage's
    // deadline.
    let mut observed = healthy();
    observed.decoy.exit_code = None;
    let findings = findings(&observed);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].contains("still running"), "{findings:?}");
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
