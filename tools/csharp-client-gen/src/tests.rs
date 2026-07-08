//! Tests for the scraper (Step 2). Kept in a SEPARATE file per CLAUDE.md; wired from
//! the crate root via `#[cfg(test)] #[path = "tests.rs"] mod tests;`.
//!
//! Three things are pinned:
//!
//! 1. the produced manifest matches the committed golden (the 12 methods, 6 DTOs,
//!    `Status` variants) — the whole scrape end-to-end;
//! 2. the drift gate fires on a route_bindings-without-signature mismatch;
//! 3. the completeness gate fires on a #[http]-bearing provider missing from the list.
//!
//! The gate functions are exercised directly with hand-built inputs (no need to mutate
//! real crates).

use std::collections::BTreeSet;

use crate::emit::{emit_client, emit_dtos, emit_status};
use crate::model::{Manifest, TypeRef};
use crate::scrape::{check_completeness, check_drift, scrape};

/// The committed golden manifest — regenerate with
/// `cargo run -p csharp-client-gen -- --emit-manifest testdata/manifest.golden.json`.
const GOLDEN: &str = include_str!("../testdata/manifest.golden.json");

/// The committed golden C# — regenerate with
/// `cargo run -p csharp-client-gen -- --out clients/csharp/Generated`, then copy the three
/// files into `testdata/*.golden.cs`. Byte-equality here is what makes Step 5's freshness
/// gate (`git diff --exit-code`) meaningful.
const STATUS_GOLDEN: &str = include_str!("../testdata/Status.golden.cs");
const DTOS_GOLDEN: &str = include_str!("../testdata/Dtos.golden.cs");
const CLIENT_GOLDEN: &str = include_str!("../testdata/Client.golden.cs");

#[test]
fn manifest_matches_golden() {
    let produced = scrape().expect("scrape must succeed on a healthy tree");
    let produced_json = serde_json::to_string_pretty(&produced).unwrap();
    assert_eq!(
        produced_json.trim(),
        GOLDEN.trim(),
        "scraped manifest drifted from the committed golden"
    );
}

#[test]
fn golden_covers_the_known_surface() {
    // A structural sanity check independent of the string golden: exactly the 12
    // player-reachable methods and the 6 reachable DTOs.
    let m: Manifest = serde_json::from_str(GOLDEN).unwrap();
    assert_eq!(m.methods.len(), 12, "expected 12 #[http] methods");
    assert_eq!(m.dtos.len(), 6, "expected 6 reachable DTOs");
    assert_eq!(m.statuses.len(), 8, "expected 8 Status variants");

    let wires: BTreeSet<&str> = m.methods.iter().map(|x| x.wire_method.as_str()).collect();
    for expected in [
        "accounts.register",
        "accounts.login",
        "accounts.loginEpic",
        "accounts.me",
        "characters.create",
        "characters.list",
        "characters.delete",
        "inventory.listMine",
        "inventory.listCharacter",
        "inventory.grant",
        "match.report",
        "leaderboard.topScores",
    ] {
        assert!(wires.contains(expected), "missing wire method {expected}");
    }

    let dtos: BTreeSet<&str> = m.dtos.iter().map(|d| d.name.as_str()).collect();
    for expected in ["Session", "IdentityRef", "MeView", "Character", "Holding", "Score"] {
        assert!(dtos.contains(expected), "missing DTO {expected}");
    }
}

#[test]
fn no_arg_methods_have_empty_args_not_missing() {
    // The Step-1 finding: a no-arg method must be modeled with `args: []` (the emitter
    // still sends `{}`, never `null`).
    let m: Manifest = serde_json::from_str(GOLDEN).unwrap();
    let list_mine = m
        .methods
        .iter()
        .find(|x| x.wire_method == "inventory.listMine")
        .unwrap();
    assert!(list_mine.args.is_empty());
    assert!(matches!(&list_mine.ret, TypeRef::Vec(_)));
}

#[test]
fn body_name_rename_applied_to_wire_name() {
    let m: Manifest = serde_json::from_str(GOLDEN).unwrap();
    let reg = m.methods.iter().find(|x| x.wire_method == "accounts.register").unwrap();
    let display = reg.args.iter().find(|a| a.name == "display_name").unwrap();
    assert_eq!(display.wire_name, "displayName", "body_names override must reach wire_name");

    let report = m.methods.iter().find(|x| x.wire_method == "match.report").unwrap();
    let winner = report.args.iter().find(|a| a.name == "winner").unwrap();
    assert_eq!(winner.wire_name, "Winner");
}

// --- Emit: golden C# -------------------------------------------------------

#[test]
fn emitted_status_matches_golden() {
    let m = scrape().expect("scrape must succeed on a healthy tree");
    assert_eq!(
        emit_status(&m),
        STATUS_GOLDEN,
        "Status.cs drifted from the committed golden — regenerate with --out"
    );
}

#[test]
fn emitted_dtos_matches_golden() {
    let m = scrape().expect("scrape must succeed on a healthy tree");
    assert_eq!(
        emit_dtos(&m),
        DTOS_GOLDEN,
        "Dtos.cs drifted from the committed golden — regenerate with --out"
    );
}

#[test]
fn emitted_client_matches_golden() {
    let m = scrape().expect("scrape must succeed on a healthy tree");
    assert_eq!(
        emit_client(&m),
        CLIENT_GOLDEN,
        "Client.cs drifted from the committed golden — regenerate with --out"
    );
}

// --- Gate 1: drift ---------------------------------------------------------

#[test]
fn drift_gate_passes_on_equal_sets() {
    let a: BTreeSet<String> = ["characters.create".into(), "characters.list".into()].into();
    let b = a.clone();
    assert!(check_drift(&a, &b).is_ok());
}

#[test]
fn drift_gate_fires_on_runtime_without_signature() {
    // A route_bindings() method with no parsed #[http] sig (Phase A ⊄ Phase B).
    let runtime: BTreeSet<String> =
        ["characters.create".into(), "characters.list".into()].into();
    let parsed: BTreeSet<String> = ["characters.create".into()].into();
    let err = check_drift(&runtime, &parsed).expect_err("must fire");
    assert!(err.contains("characters.list"), "err was: {err}");
    assert!(err.contains("no parsed"), "err was: {err}");
}

#[test]
fn drift_gate_fires_on_signature_without_route() {
    // A parsed #[http] method with no route_binding (Phase B ⊄ Phase A).
    let runtime: BTreeSet<String> = ["characters.create".into()].into();
    let parsed: BTreeSet<String> =
        ["characters.create".into(), "characters.ghost".into()].into();
    let err = check_drift(&runtime, &parsed).expect_err("must fire");
    assert!(err.contains("characters.ghost"), "err was: {err}");
    assert!(err.contains("no route_binding"), "err was: {err}");
}

// --- Gate 2: provider-completeness -----------------------------------------

#[test]
fn completeness_gate_passes_when_all_providers_listed() {
    let http_traits = vec!["characters".to_string(), "accounts".to_string()];
    let providers = ["characters", "inventory", "accounts", "match", "leaderboard"];
    assert!(check_completeness(&http_traits, &providers).is_ok());
}

#[test]
fn completeness_gate_fires_on_missing_provider() {
    // A new player-facing module ("quests") exposes #[http] but was never added to the
    // provider list — this must be a build failure.
    let http_traits = vec!["characters".to_string(), "quests".to_string()];
    let providers = ["characters", "inventory", "accounts", "match", "leaderboard"];
    let err = check_completeness(&http_traits, &providers).expect_err("must fire");
    assert!(err.contains("quests"), "err was: {err}");
}
