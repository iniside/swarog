//! Tests for the scraper, its gates and the C# emitter. Gate functions take hand-built
//! inputs, so no real crate is mutated.

use std::collections::BTreeSet;

use std::path::PathBuf;

use crate::emit::{emit_client, emit_dtos, emit_status};
use crate::model::{ArgDef, DtoDef, FieldDef, Manifest, MethodDef, TypeRef};
use crate::scrape::{check_completeness, check_drift, map_type, parse_sources, scrape};

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
    // A structural sanity check independent of the string golden: exactly the 52
    // player-reachable methods and the 28 reachable DTOs.
    let m: Manifest = serde_json::from_str(GOLDEN).unwrap();
    assert_eq!(m.methods.len(), 52, "expected 52 #[http] methods");
    assert_eq!(m.dtos.len(), 28, "expected 28 reachable DTOs");
    assert_eq!(m.statuses.len(), 8, "expected 8 Status variants");

    // Set EQUALITY, not inclusion: an added method that is counted and re-blessed
    // would slip past a `contains` loop with no name ever read by a human.
    let wires: BTreeSet<&str> = m.methods.iter().map(|x| x.wire_method.as_str()).collect();
    assert_eq!(
        wires,
        BTreeSet::from([
            "accounts.register",
            "accounts.login",
            "accounts.loginFederated",
            "accounts.createGuest",
            "accounts.refresh",
            "accounts.me",
            "accounts.link",
            "accounts.beginDelete",
            "accounts.deleteAccount",
            "characters.create",
            "characters.list",
            "characters.delete",
            "notifications.list",
            "notifications.markRead",
            "notifications.delete",
            "inventory.listMine",
            "inventory.listCharacter",
            "inventory.grant",
            "match.report",
            "leaderboard.topScores",
            "wallet.myBalances",
            "wallet.listCurrencies",
            "friends.request",
            "friends.accept",
            "friends.decline",
            "friends.remove",
            "friends.list",
            "friends.pending",
            "groups.create",
            "groups.listMine",
            "groups.members",
            "groups.pending",
            "groups.join",
            "groups.leave",
            "groups.invite",
            "groups.respond",
            "groups.decide",
            "storage.put",
            "storage.get",
            "storage.delete",
            "storage.list",
            "party.accept",
            "party.create",
            "party.decline",
            "party.get",
            "party.heartbeat",
            "party.invite",
            "party.invites",
            "party.kick",
            "party.leave",
            "party.promote",
            "party.setReady",
        ]),
        "the generated wire-method surface drifted from the reviewed list"
    );

    let dtos: BTreeSet<&str> = m.dtos.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        dtos,
        BTreeSet::from([
            "Session",
            "GuestSession",
            "IdentityRef",
            "MeView",
            "Notification",
            "Page",
            "Character",
            "Holding",
            "Score",
            "Balance",
            "Currency",
            "DeleteTicket",
            "DeleteReceipt",
            "Friend",
            "FriendPage",
            "GroupSummary",
            "GroupPage",
            "MemberSummary",
            "MemberPage",
            "StorageObject",
            "ObjectVersion",
            "ObjectSummary",
            "ObjectPage",
            "PartyView",
            "MemberView",
            "InviteView",
            "InviteList",
            "HeartbeatView",
        ]),
        "the generated DTO surface drifted from the reviewed list"
    );
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
    let rid = report.args.iter().find(|a| a.name == "report_id").unwrap();
    assert_eq!(rid.wire_name, "ReportId", "the idempotency key rides capitalized");
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
        emit_dtos(&m).unwrap(),
        DTOS_GOLDEN,
        "Dtos.cs drifted from the committed golden — regenerate with --out"
    );
}

#[test]
fn emitted_client_matches_golden() {
    let m = scrape().expect("scrape must succeed on a healthy tree");
    assert_eq!(
        emit_client(&m).unwrap(),
        CLIENT_GOLDEN,
        "Client.cs drifted from the committed golden — regenerate with --out"
    );
}

#[test]
fn args_named_like_generator_identifiers_stay_distinct() {
    let arg = |name: &str| ArgDef { name: name.into(), wire_name: name.into(), ty: TypeRef::String };
    let m = Manifest {
        methods: vec![MethodDef {
            provider: "storage".into(),
            wire_method: "storage.put".into(),
            verb: "POST".into(),
            path: "/storage".into(),
            auth: "player".into(),
            success: 200,
            args: [
                "value", "request", "payload", "resp", "envelope", "transport", "token", "ct", "await",
                "class",
            ]
            .into_iter()
            .map(arg)
            .collect(),
            ret: TypeRef::String,
        }],
        dtos: vec![],
        statuses: vec![],
    };
    let cs = emit_client(&m).unwrap();
    assert!(cs.contains(
        "StoragePutAsync(string token, string value, string request, string payload, string resp, \
         string envelope, string transport, string token_, string ct_, string @await, string @class, \
         CancellationToken ct = default)"
    ));
    assert!(cs.contains(
        "var _request = new StoragePutRequest(value, request, payload, resp, envelope, transport, \
         token_, ct_, @await, @class);"
    ));
    assert!(cs.contains("await _transport.CallAsync(\"storage.put\", token, _payload, ct)"));
    assert!(cs.contains("JsonNode _value = _envelope[\"value\"]"));
}

fn dto_with_fields(dto: &str, fields: &[&str]) -> Manifest {
    let field = |name: &&str| FieldDef { name: (*name).into(), wire_name: (*name).into(), ty: TypeRef::String };
    Manifest {
        methods: vec![],
        dtos: vec![DtoDef { name: dto.into(), fields: fields.iter().map(field).collect() }],
        statuses: vec![],
    }
}

fn method_with_args(args: &[&str]) -> Manifest {
    let arg = |name: &&str| ArgDef { name: (*name).into(), wire_name: (*name).into(), ty: TypeRef::String };
    Manifest {
        methods: vec![MethodDef {
            provider: "storage".into(),
            wire_method: "storage.put".into(),
            verb: "POST".into(),
            path: "/storage".into(),
            auth: "player".into(),
            success: 200,
            args: args.iter().map(arg).collect(),
            ret: TypeRef::Unit,
        }],
        dtos: vec![],
        statuses: vec![],
    }
}

fn assert_refused(err: anyhow::Error, record: &str, field: &str, why: &str) {
    let msg = format!("{err:#}");
    assert!(msg.contains(record), "err must name the record {record}: {msg}");
    assert!(msg.contains(&format!("{field:?}")), "err must name the field {field}: {msg}");
    assert!(msg.contains(why), "err must say why ({why}): {msg}");
}

#[test]
fn fields_named_like_record_synthesized_members_are_refused() {
    for (field, prop) in [
        ("equals", "Equals"),
        ("get_hash_code", "GetHashCode"),
        ("to_string", "ToString"),
        ("print_members", "PrintMembers"),
        ("equality_contract", "EqualityContract"),
        ("deconstruct", "Deconstruct"),
    ] {
        let err = emit_dtos(&dto_with_fields("Score", &["value", field])).expect_err(field);
        assert_refused(err, "Score", field, &format!("record-synthesized {prop}"));
        let err = emit_client(&method_with_args(&[field])).expect_err(field);
        assert_refused(err, "StoragePutRequest", field, &format!("record-synthesized {prop}"));
    }
}

#[test]
fn fields_named_like_inherited_object_methods_are_refused() {
    for (field, prop) in [
        ("get_type", "GetType"),
        ("memberwise_clone", "MemberwiseClone"),
        ("finalize", "Finalize"),
    ] {
        let err = emit_dtos(&dto_with_fields("Score", &[field])).expect_err(field);
        assert_refused(err, "Score", field, &format!("object.{prop}"));
        let err = emit_client(&method_with_args(&[field])).expect_err(field);
        assert_refused(err, "StoragePutRequest", field, &format!("object.{prop}"));
    }
}

#[test]
fn field_named_like_its_enclosing_record_is_refused() {
    let err = emit_dtos(&dto_with_fields("Score", &["score"])).expect_err("dto own name");
    assert_refused(err, "Score", "score", "record's own name");
    let err = emit_client(&method_with_args(&["storage_put_request"])).expect_err("request own name");
    assert_refused(err, "StoragePutRequest", "storage_put_request", "record's own name");
}

#[test]
fn fields_mapping_to_one_property_are_refused() {
    let err = emit_dtos(&dto_with_fields("Score", &["player_id", "player__id"])).expect_err("dto dup");
    assert_refused(err, "Score", "player__id", "both become property PlayerId");
    let err = emit_client(&method_with_args(&["token", "token_"])).expect_err("request dup");
    assert_refused(err, "StoragePutRequest", "token_", "both become property Token");
}

#[test]
fn near_miss_member_names_are_emitted() {
    let fields = ["clone", "reference_equals", "hash_code", "type", "scores"];
    let cs = emit_dtos(&dto_with_fields("Score", &fields)).unwrap();
    for prop in ["Clone", "ReferenceEquals", "HashCode", "Type", "Scores"] {
        assert!(cs.contains(&format!("string {prop}")), "missing {prop}: {cs}");
    }
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

// --- Cross-domain DTO name collision -----------------------------------------

#[test]
fn struct_collision_across_files_bails_naming_both_paths() {
    // Two synthetic api-crate sources declaring the SAME short struct name — a flat
    // `name -> fields` map would silently let whichever file sorts last overwrite the
    // other DTO's fields. This must be a hard error instead, naming both source files.
    let files = vec![
        (
            "characters".to_string(),
            PathBuf::from("api/characters/api/src/lib.rs"),
            "pub struct Score { pub value: i64 }".to_string(),
        ),
        (
            "leaderboard".to_string(),
            PathBuf::from("api/leaderboard/api/src/lib.rs"),
            "pub struct Score { pub points: i64 }".to_string(),
        ),
    ];
    let err = match parse_sources(&files) {
        Ok(_) => panic!("must bail on colliding struct name"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert!(msg.contains("Score"), "err was: {msg}");
    assert!(
        msg.contains("api/characters/api/src/lib.rs"),
        "err must name the first declaring file: {msg}"
    );
    assert!(
        msg.contains("api/leaderboard/api/src/lib.rs"),
        "err must name the second declaring file: {msg}"
    );
}

#[test]
fn struct_no_collision_when_names_differ() {
    let files = vec![
        (
            "characters".to_string(),
            PathBuf::from("api/characters/api/src/lib.rs"),
            "pub struct Character { pub id: i64 }".to_string(),
        ),
        (
            "leaderboard".to_string(),
            PathBuf::from("api/leaderboard/api/src/lib.rs"),
            "pub struct Score { pub value: i64 }".to_string(),
        ),
    ];
    assert!(parse_sources(&files).is_ok());
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

// --- Gate 3: the type lattice ----------------------------------------------

#[test]
fn unmodelled_scalar_bails_naming_the_lattice_not_a_missing_dto() {
    // `u32` is the shape of the next scalar a contract adds. Before the named arm it fell
    // through to `TypeRef::Struct("u32")` and died much later in `collect_dtos` as
    // `DTO "u32" referenced but no pub struct found`, sending the reader after a struct that
    // was never meant to exist.
    let ty: syn::Type = syn::parse_str("u32").expect("parses");
    let err = map_type(&ty).expect_err("an unmodelled scalar must bail");
    let msg = err.to_string();
    assert!(msg.contains("u32"), "err was: {msg}");
    assert!(
        msg.contains("TypeRef") && msg.contains("cs_type"),
        "the diagnostic must name the remedy (a TypeRef variant + its cs_type mapping); err was: {msg}"
    );

    // The recognised scalars stay recognised.
    assert_eq!(
        map_type(&syn::parse_str::<syn::Type>("i32").unwrap()).unwrap(),
        TypeRef::I32
    );
    assert_eq!(
        map_type(&syn::parse_str::<syn::Type>("Wallet").unwrap()).unwrap(),
        TypeRef::Struct("Wallet".into())
    );
}

