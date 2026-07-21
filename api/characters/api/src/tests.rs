//! `describe()` for `characters` — the PATH-WILDCARD case and the wire-only-exclusion
//! case (D1). `Player::delete` binds `/characters/{id}` with the `{id}` wildcard riding
//! into the wire field `character_id`; `Ownership::owner_of` is wire-only (no `#[http]`)
//! and must NOT appear in any describe manifest — only player-facing HTTP ops are
//! gateway-routed.

use crate::{ownership_rpc, player_rpc};
use opsapi::{ArgSource, AuthReq, RetryMode};

/// `describe()` reflects `delete`'s path wildcard as an `ArgSource::Path` mapping: the
/// arg is taken from the `{id}` wildcard and written into the wire request under the
/// param-name key `character_id` (path args keep their param-name key — they are never
/// sent by a client under the wildcard name). This is exactly what the generated
/// `decode` does; a data-driven gateway replays it from this data.
#[test]
fn describe_reflects_delete_path_wildcard() {
    let manifest = player_rpc::describe();
    let delete = manifest
        .ops
        .iter()
        .find(|o| o.method == "characters.delete")
        .expect("delete op present");

    assert_eq!(delete.verb, "DELETE");
    assert_eq!(delete.path, "/characters/{id}");
    assert_eq!(delete.auth, AuthReq::Player);
    assert_eq!(delete.success, 204);

    // The identity leading param is stripped from the wire request by the macro, so it
    // never appears as an arg mapping — `character_id` (the wildcard) is the only arg.
    assert_eq!(delete.args.len(), 1);
    let arg = &delete.args[0];
    assert_eq!(arg.param, "character_id");
    assert_eq!(arg.wire_key, "character_id");
    assert_eq!(arg.source, ArgSource::Path { wildcard: "id".into() });
}

/// `create` and `list` are BODY/no-arg HTTP ops on the same trait — the manifest carries
/// all three `#[http]` ops with their bindings, none of the leading `Identity` params.
#[test]
fn describe_covers_all_player_http_ops() {
    let methods: std::collections::BTreeSet<String> =
        player_rpc::describe().ops.into_iter().map(|o| o.method).collect();
    assert_eq!(
        methods,
        ["characters.create", "characters.delete", "characters.list"]
            .into_iter()
            .map(String::from)
            .collect()
    );
}

/// `describe()` carries each op's `retry_mode` faithfully: the `#[retry_safe]` read
/// `list` (GET) surfaces `OnceAfterReconnect`, while the mutations `create` (POST) and
/// `delete` (DELETE) stay fail-closed at `Never`. This pins that a data-driven gateway
/// reconstructing an `Operation` from `describe()` neither drops a read's replay nor
/// grants a mutation an unearned one — the retry_mode value the routed `Operation`
/// carries, from the SAME `#[retry_safe]` marker.
#[test]
fn describe_carries_per_op_retry_mode() {
    let by_method: std::collections::HashMap<String, RetryMode> = player_rpc::describe()
        .ops
        .into_iter()
        .map(|o| (o.method, o.retry_mode))
        .collect();
    assert_eq!(by_method["characters.list"], RetryMode::OnceAfterReconnect);
    assert_eq!(by_method["characters.create"], RetryMode::Never);
    assert_eq!(by_method["characters.delete"], RetryMode::Never);

    // Byte-identical to the routed `Operation`s (one authority — no describe/route drift).
    for b in player_rpc::route_bindings() {
        assert_eq!(
            by_method[&b.operation.method], b.operation.retry_mode,
            "method {}", b.operation.method
        );
    }
}

/// A WIRE-ONLY trait (`Ownership::owner_of`: no `#[http]`) produces an EMPTY manifest —
/// wire-only capabilities are never gateway-routed, so they contribute no describe ops.
#[test]
fn describe_excludes_wire_only_methods() {
    assert!(ownership_rpc::describe().ops.is_empty());
}

/// The described op set is EXACTLY the routed op set (`route_bindings()`), by
/// construction — so any NEW `#[http]` op appears in `describe()` automatically with
/// zero gateway change, and a wire-only method is excluded from both.
#[test]
fn describe_covers_exactly_the_route_bindings() {
    for (described, routed) in [
        (player_rpc::describe(), player_rpc::route_bindings()),
        (ownership_rpc::describe(), ownership_rpc::route_bindings()),
    ] {
        let d: std::collections::BTreeSet<String> =
            described.ops.into_iter().map(|o| o.method).collect();
        let r: std::collections::BTreeSet<String> =
            routed.into_iter().map(|b| b.operation.method).collect();
        assert_eq!(d, r);
    }
}
