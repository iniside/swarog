//! `describe()` round-trip for `match` — the crux `body_names` case (D1). The
//! generated manifest must carry each HTTP op's `HttpBind` field-identically,
//! including match's capitalized public body keys (`ReportId`/`Winner`/`Loser`), so a
//! data-driven gateway rebuilds the exact wire request without importing `matchrpc`.

use crate::match_rpc;
use opsapi::{ArgSource, AuthReq};

/// `describe()` reflects `report`'s `#[http]` binding exactly, and its `body_names`
/// renames surface as `wire_key`s distinct from the param names — the data a
/// data-driven decode injects the raw body under.
#[test]
fn describe_reflects_report_http_bind_with_body_names() {
    let manifest = match_rpc::describe();
    // `report` is the only method, and it is `#[http]`-bound.
    assert_eq!(manifest.ops.len(), 1);
    let op = &manifest.ops[0];

    assert_eq!(op.method, "match.report");
    assert_eq!(op.verb, "POST");
    assert_eq!(op.path, "/match/report");
    assert_eq!(op.auth, AuthReq::None);
    assert_eq!(op.success, 202);

    // Args are in declaration order; every arg is a BODY arg with its `body_names`
    // wire key. This is the case the design named as the routing-as-data worry — it
    // reduces to `wire_key` data, no custom decode.
    let by_param: std::collections::HashMap<&str, &opsapi::ArgMapping> =
        op.args.iter().map(|a| (a.param.as_str(), a)).collect();
    assert_eq!(op.args.len(), 3);

    for (param, wire_key) in [
        ("report_id", "ReportId"),
        ("winner", "Winner"),
        ("loser", "Loser"),
    ] {
        let a = by_param.get(param).unwrap_or_else(|| panic!("missing arg {param}"));
        assert_eq!(a.wire_key, wire_key, "param {param}");
        assert_eq!(a.source, ArgSource::Body, "param {param}");
    }
}

/// The manifest's op set is EXACTLY the `#[http]` op set the gateway routes over
/// (`route_bindings()`), by construction — so a new `#[http]` op appears in
/// `describe()` automatically, with zero gateway change. If `describe()` ever drifted
/// from `route_bindings()` (a bespoke second op source), this fails.
#[test]
fn describe_covers_exactly_the_route_bindings() {
    let described: std::collections::BTreeSet<String> =
        match_rpc::describe().ops.into_iter().map(|o| o.method).collect();
    let routed: std::collections::BTreeSet<String> = match_rpc::route_bindings()
        .into_iter()
        .map(|b| b.operation.method)
        .collect();
    assert_eq!(described, routed);
}
