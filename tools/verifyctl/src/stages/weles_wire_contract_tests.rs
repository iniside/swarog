//! Tests for the `weles-wire-contract` stage.
//!
//! Two halves, and BOTH are needed:
//!
//! * **Positive control** — the comparators pass on the REAL types at HEAD, and
//!   they demonstrably compared something (a count guard against the vacuous
//!   loop: a check over an empty table passes trivially and proves nothing).
//! * **Negative control** — every comparator is driven with a SYNTHETIC drifted
//!   pair and must FAIL. A gate that has never been seen to fail is theatre; the
//!   comparators take plain data precisely so this is possible.

use super::*;

fn spelling(variant: &str, weles: &str, remote: &str) -> Spelling {
    Spelling {
        variant: variant.to_string(),
        weles: weles.to_string(),
        remote: remote.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Positive control: HEAD agrees, and the check was not vacuous.
// ---------------------------------------------------------------------------

#[test]
fn head_agrees_on_the_whole_wire_contract() {
    assert_eq!(contract_diffs(), Vec::<String>::new());
}

#[test]
fn a_renamed_borrow_marker_is_a_finding_naming_the_consequence() {
    // The staged drift: processctl bumps its marker, weles's hand-copy does not.
    // Neither crate's own tests can see this — weles would keep parsing its own
    // spelling green — and the symptom is the bug this arm was added for: a
    // borrowed `weles up` dies in argv parsing, so nothing can borrow a lease.
    let diffs = borrow_marker_diffs(
        "--processctl-borrowed-lease-v1",
        "--processctl-borrowed-lease-v2",
    );
    assert_eq!(diffs.len(), 1, "{diffs:?}");
    assert!(diffs[0].contains("unknown argument"), "{diffs:?}");
    assert!(diffs[0].contains("acquire_or_borrow"), "{diffs:?}");
}

#[test]
fn head_agrees_on_the_borrow_marker_through_the_real_consts() {
    // Not a re-assertion of the literal: these are the two values the two crates
    // actually use — processctl's is what `spawn_borrower` appends, weles's is
    // what `cli::parse` and `borrow_inherited_if_present` match on.
    assert_eq!(
        borrow_marker_diffs(weles::lock::BORROWED_LEASE_ARG, processctl::BORROWED_LEASE_ARG),
        Vec::<String>::new()
    );
}

#[test]
fn the_stage_actually_compares_every_variant_both_enums_declare() {
    // The count guard. `head_agrees_on_the_whole_wire_contract` would pass just
    // as happily over empty tables, so the tables' size is pinned against what
    // serde itself declares — not against a number typed in here.
    let addr_kinds = declared_variants::<WAddrKind>().expect("weles AddrKind derives Deserialize");
    assert_eq!(addr_kinds, vec!["edge", "http"]);
    assert_eq!(addr_kind_spellings().unwrap().len(), addr_kinds.len());

    let weles_codes = declared_variants::<WErrorCode>().expect("weles ErrorCode derives Deserialize");
    let remote_codes = declared_variants::<RErrorCode>().expect("remote ErrorCode derives Deserialize");
    assert_eq!(
        weles_codes,
        vec!["unknown_route", "unknown_peer", "bad_request", "internal"]
    );
    assert_eq!(weles_codes, remote_codes);
    assert_eq!(error_code_read_backs().unwrap().len(), weles_codes.len());
}

#[test]
fn every_error_code_survives_the_trip_from_weles_into_remote() {
    // The property, stated as an assertion rather than left implicit in the
    // aggregate: the client reads the server's own bytes, as the same variant.
    let read_backs = error_code_read_backs().unwrap();
    for read_back in &read_backs {
        assert_eq!(
            read_back.got.as_deref(),
            Ok(read_back.want.as_str()),
            "{} ({:?})",
            read_back.variant,
            read_back.wire
        );
    }
    assert_eq!(read_backs.len(), 4);
}

// ---------------------------------------------------------------------------
// Negative control: the near-term trap this stage was built for.
// ---------------------------------------------------------------------------

#[test]
fn a_player_edge_variant_under_the_wrong_rename_all_is_caught() {
    // THE scenario, with the sides the way round they would actually happen.
    // BOTH `AddrKind`s are `rename_all = "lowercase"`, and in `remote/resolve.rs`
    // `ErrorCode` — `snake_case` — sits fifteen lines below `AddrKind`. So the
    // mistake is copying `snake_case` onto REMOTE's `AddrKind`: weles keeps
    // emitting `"playeredge"` and remote starts emitting `"player_edge"`.
    // `ServiceDef` already carries a `player_port`, so `PlayerEdge` is the next
    // variant. Today `Edge`/`Http` render identically under both derives, which
    // is exactly what makes the mistake invisible — and why this must be driven
    // synthetically rather than waited for.
    let drifted = vec![
        spelling("Edge", "edge", "edge"),
        spelling("Http", "http", "http"),
        spelling("PlayerEdge", "playeredge", "player_edge"),
    ];
    let diffs = spelling_diffs("AddrKind", &drifted);
    assert_eq!(diffs.len(), 1, "{diffs:?}");
    assert!(diffs[0].contains("PlayerEdge"), "{diffs:?}");
    assert!(diffs[0].contains("player_edge"), "{diffs:?}");
    assert!(diffs[0].contains("playeredge"), "{diffs:?}");

    // ...and the same list with the mistake NOT made passes, so the check is
    // discriminating rather than merely noisy.
    let agreed = vec![
        spelling("Edge", "edge", "edge"),
        spelling("Http", "http", "http"),
        spelling("PlayerEdge", "playeredge", "playeredge"),
    ];
    assert_eq!(spelling_diffs("AddrKind", &agreed), Vec::<String>::new());
}

#[test]
fn a_comparison_over_an_empty_table_is_a_failure_not_a_pass() {
    // The vacuous-loop failure mode, refused explicitly by both loop-bearing
    // comparators.
    assert_eq!(spelling_diffs("AddrKind", &[]).len(), 1);
    assert!(spelling_diffs("AddrKind", &[])[0].contains("proved nothing"));
    assert_eq!(read_back_diffs("ErrorCode", &[]).len(), 1);
    assert!(read_back_diffs("ErrorCode", &[])[0].contains("proved nothing"));
}

#[test]
fn a_pair_table_short_of_what_serde_declares_is_caught() {
    // The other half of "didn't-forget tooling must self-check": adding a
    // variant to weles is a COMPILE error in `addr_kind_peer`, but fixing that
    // error without extending the table would leave the new variant compared by
    // nobody. serde's declared set is what refuses that.
    let declared = vec![
        "edge".to_string(),
        "http".to_string(),
        "player_edge".to_string(),
    ];
    let compared = vec!["edge".to_string(), "http".to_string()];
    let diffs = coverage_diffs("AddrKind", "weles::manifest::AddrKind", &declared, &compared);
    assert_eq!(diffs.len(), 1, "{diffs:?}");
    assert!(diffs[0].contains("player_edge"), "{diffs:?}");
    assert!(diffs[0].contains("does not cover it"), "{diffs:?}");

    // A stale table (comparing something serde no longer declares) is also a
    // finding — the removal direction, which a length check alone would miss.
    let stale = coverage_diffs(
        "AddrKind",
        "weles::manifest::AddrKind",
        &compared,
        &declared,
    );
    assert_eq!(stale.len(), 1, "{stale:?}");
    assert!(stale[0].contains("stale"), "{stale:?}");
}

#[test]
fn an_error_code_remote_reads_as_the_wrong_variant_is_caught() {
    // The read-back check's whole point: bytes could match while the pairing is
    // wrong (two variants transposed in the table, or renamed on one side).
    let drifted = vec![ReadBack {
        variant: "UnknownRoute".to_string(),
        wire: "unknown_route".to_string(),
        got: Ok("UnknownPeer".to_string()),
        want: "UnknownRoute".to_string(),
    }];
    let diffs = read_back_diffs("ErrorCode", &drifted);
    assert_eq!(diffs.len(), 1, "{diffs:?}");
    assert!(diffs[0].contains("pairs it with UnknownRoute"), "{diffs:?}");
}

#[test]
fn an_error_code_remote_cannot_read_at_all_is_caught() {
    // A `rename_all` drift on ErrorCode is NOT a transposition — it is a code
    // the client refuses outright, which at runtime is `ResolveError::Malformed`
    // ("the agent is not speaking this contract") from the agent's own answer.
    let drifted = vec![ReadBack {
        variant: "UnknownPeer".to_string(),
        wire: "unknownpeer".to_string(),
        got: Err("unknown variant `unknownpeer`".to_string()),
        want: "UnknownPeer".to_string(),
    }];
    let diffs = read_back_diffs("ErrorCode", &drifted);
    assert_eq!(diffs.len(), 1, "{diffs:?}");
    assert!(diffs[0].contains("CANNOT read it"), "{diffs:?}");
}

// ---------------------------------------------------------------------------
// Negative control: the field names.
// ---------------------------------------------------------------------------

#[test]
fn a_renamed_request_field_is_caught_by_weles_deny_unknown_fields() {
    // If `remote` renamed `kind` to `addr_kind`, weles's `deny_unknown_fields`
    // parser refuses the body its own client sends. Driven with the drifted
    // BYTES, so this exercises the real server-side derive.
    let diffs = request_diffs(
        br#"{"provider":"characters","addr_kind":"edge"}"#,
        "characters",
        WAddrKind::Edge,
    );
    assert_eq!(diffs.len(), 1, "{diffs:?}");
    assert!(diffs[0].contains("CANNOT parse"), "{diffs:?}");

    // A renamed `provider` is caught the same way (it is required, not
    // defaulted), and the honest body passes.
    assert_eq!(
        request_diffs(
            br#"{"provider":"characters","kind":"edge"}"#,
            "characters",
            WAddrKind::Edge
        ),
        Vec::<String>::new()
    );
}

#[test]
fn a_renamed_response_field_is_caught() {
    let diffs = response_diffs(br#"{"addresses":["127.0.0.1:9000"]}"#, &[
        "127.0.0.1:9000".to_string()
    ]);
    assert_eq!(diffs.len(), 1, "{diffs:?}");
    assert!(diffs[0].contains("CANNOT parse"), "{diffs:?}");
}

#[test]
fn a_renamed_envelope_error_field_is_caught_even_though_remote_defaults_it() {
    // The subtle one. remote's `error` is `#[serde(default)]`, so a rename does
    // NOT fail the parse — it silently blanks the operator's only prose. Only
    // comparing the VALUE catches it; asserting the parse succeeded would not.
    let diffs = envelope_diffs(
        br#"{"code":"unknown_peer","message":"no Edge address"}"#,
        RErrorCode::UnknownPeer,
        "no Edge address",
    );
    assert_eq!(diffs.len(), 1, "{diffs:?}");
    assert!(diffs[0].contains("`error` field name drifted"), "{diffs:?}");
    assert!(diffs[0].contains("silent"), "{diffs:?}");

    // A renamed `code` is not silent — it is a refusal.
    let renamed_code = envelope_diffs(
        br#"{"reason":"unknown_peer","error":"no Edge address"}"#,
        RErrorCode::UnknownPeer,
        "no Edge address",
    );
    assert_eq!(renamed_code.len(), 1, "{renamed_code:?}");
    assert!(renamed_code[0].contains("CANNOT parse"), "{renamed_code:?}");
}

// ---------------------------------------------------------------------------
// The collector seam: the checks that survive a miscollected column.
// ---------------------------------------------------------------------------

#[test]
fn every_addr_kind_round_trips_through_the_production_types() {
    // The check with NO hand-copied column in between: remote's real `Serialize`
    // into weles's real `Deserialize`, once per variant. If `addr_kind_spellings`
    // ever miscollected (reading weles's type into BOTH columns — a one-word
    // slip that would leave `spelling_diffs` comparing a value to itself and
    // every other test green), this is what still fails.
    for (weles_kind, remote_kind) in addr_kind_pairs() {
        let body = remote::resolve::drift_probe_encode_resolve_request("characters", remote_kind);
        assert_eq!(
            request_diffs(&body, "characters", weles_kind),
            Vec::<String>::new(),
            "{weles_kind:?} -> {}",
            String::from_utf8_lossy(&body)
        );
    }
    assert_eq!(addr_kind_pairs().len(), 2);
}

#[test]
fn the_round_trip_discriminates_kinds_rather_than_accepting_any_body() {
    // Proves the loop above can fail. `Edge`/`Http` render identically under
    // both derives at HEAD, so a spelling drift cannot be staged with the real
    // types — a transposed pair is the observable equivalent, and it exercises
    // the same comparison (weles parsed a kind that is not the one remote meant)
    // through the same production `Serialize`/`Deserialize` path a `PlayerEdge`
    // drift would take.
    let body = remote::resolve::drift_probe_encode_resolve_request("characters", RAddrKind::Http);
    let diffs = request_diffs(&body, "characters", WAddrKind::Edge);
    assert_eq!(diffs.len(), 1, "{diffs:?}");
    assert!(diffs[0].contains("weles read kind=Http"), "{diffs:?}");
    assert!(diffs[0].contains("remote sent Edge"), "{diffs:?}");
}

#[test]
fn a_transposed_pair_table_is_caught_by_the_bijection() {
    // `addr_kind_peer` and `addr_kind_peer_back` are two independent hand-written
    // matches; composing them is what makes them an actual bijection rather than
    // two tables free to mirror the same mistake.
    assert_eq!(bijection_diffs(&addr_kind_pairs()), Vec::<String>::new());

    let transposed = vec![
        (WAddrKind::Edge, RAddrKind::Http),
        (WAddrKind::Http, RAddrKind::Edge),
    ];
    let diffs = bijection_diffs(&transposed);
    assert_eq!(diffs.len(), 2, "{diffs:?}");
    assert!(diffs[0].contains("transposed arm"), "{diffs:?}");
}

#[test]
fn remote_only_addr_kind_variants_are_a_compile_error_not_a_runtime_check() {
    // States the trade the module doc records, so it cannot be quietly lost:
    // `remote::AddrKind` is Serialize-only, so serde declares no set to read
    // back and no runtime check here enumerates from remote's side. What holds
    // the line is `addr_kind_peer_back`'s exhaustive match — this call is the
    // reason a `RAddrKind::PlayerEdge` cannot compile without touching this file.
    assert_eq!(addr_kind_peer_back(RAddrKind::Edge), WAddrKind::Edge);
    assert_eq!(addr_kind_peer_back(RAddrKind::Http), WAddrKind::Http);
    // The asymmetry itself, asserted rather than asserted-in-prose: weles's enum
    // has a declared set, remote's does not.
    assert!(declared_variants::<WAddrKind>().is_ok());
}

#[test]
fn a_drifted_resolve_path_is_caught() {
    assert_eq!(
        path_diffs(
            weles::agentapi::RESOLVE_PATH,
            remote::resolve::RESOLVE_PATH
        ),
        Vec::<String>::new()
    );
    let diffs = path_diffs("/resolve", "/v2/resolve");
    assert_eq!(diffs.len(), 1, "{diffs:?}");
    assert!(diffs[0].contains("404 unknown_route"), "{diffs:?}");
}

// ---------------------------------------------------------------------------
// The reflection channel itself.
// ---------------------------------------------------------------------------

#[test]
fn the_variant_sniffer_refuses_a_non_enum_instead_of_reporting_an_empty_set() {
    // `declared_variants` is what makes the coverage check non-vacuous, so its
    // own failure must be loud: an empty set reported as `Ok` would silently
    // disable `coverage_diffs`.
    let error = declared_variants::<String>().unwrap_err();
    assert!(error.contains("declared no variants"), "{error}");
    assert!(error.contains("pass over nothing"), "{error}");
}
