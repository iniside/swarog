//! `weles-wire-contract` — a BLOCKING, pure in-memory verify stage that
//! machine-checks weles's agent wire contract (`weles::agentapi`, the SERVER)
//! against its hand-copied twin (`remote::resolve`, the CLIENT).
//!
//! ## Why this stage exists (the authority)
//!
//! weles is zero-sharing: `core/remote` may not import it, and it may not import
//! `core/remote`. So the `resolve` contract exists TWICE, hand-copied —
//! `AddrKind` (`weles::manifest` / `remote::resolve`), `ErrorCode`
//! (`weles::agentapi` / `remote::resolve`), and the field names of the request,
//! the answer and the refusal envelope. Both files say so in prose, and both say
//! that neither crate's tests can catch a drift: each side is tested against a
//! fake of the other. Until this stage, NOTHING pinned the two together at all.
//!
//! It is now the EARLY gate rather than the only one: `weles-managed-gateway`
//! (also BLOCKING, same manifest) boots the real fleet and drives this contract
//! over a socket. The division is deliberate — this stage is in-memory and runs
//! under `--fast`, so a drift FAILs in seconds instead of surviving to a rollout;
//! that one costs a fleet boot but reaches what no in-memory check can (the HTTP
//! method, and that `cmd/gateway-svc`'s main is wired to this client at all).
//! Neither subsumes the other, and a drift caught here is a drift that never
//! reaches a boot.
//!
//! `verifyctl` is the one place allowed to see both (`docs/reference/
//! weles-design.md`, Non-negotiables: the shipping graph may never import weles;
//! verification tooling is the narrow exception, also exercised by the other
//! `weles-*` stages). This stage is that gate. It is BLOCKING because a
//! drift here is a boot-time outage in `cmd/gateway-svc`, and it is pure
//! in-memory (no DB, no rollout, no process), so it is cheap and safe under
//! `--fast`.
//!
//! ## The trap it is built for
//!
//! `AddrKind`'s `rename_all = "lowercase"` is, at HEAD, unfalsifiable by
//! inspection: `Edge` and `Http` render identically under `lowercase` and
//! `snake_case`, and `ErrorCode` fifteen lines below carries `snake_case`.
//! `ServiceDef` already has a `player_port`. The day someone adds
//! `AddrKind::PlayerEdge`, copying the wrong `rename_all` diverges
//! `"playeredge"` from `"player_edge"` — silently, with only a live rollout
//! watching. [`spelling_diffs`] is the thing that watches instead, and
//! `a_player_edge_variant_under_the_wrong_rename_all_is_caught` pins exactly
//! that scenario.
//!
//! ## What it pins, and what it does not
//!
//! Pinned:
//! * **Every `AddrKind` variant's wire bytes**, both sides, compared
//!   ([`spelling_diffs`]).
//! * **Every `ErrorCode` variant's wire bytes**, both sides — and, better,
//!   weles's bytes DESERIALIZED INTO remote's enum, asserting the paired variant
//!   ([`read_back_diffs`]). Byte equality is only a proxy for "the client can
//!   read what the server writes"; that is the property, so that is what is
//!   checked.
//! * **The declared variant SETS** of every enum on either side that derives
//!   `Deserialize`, read out of serde itself ([`declared_variants`]) rather than
//!   hand-listed — so a variant added to weles and forgotten in the pair tables
//!   below is a FAIL, not a silently-unchecked case ([`coverage_diffs`]).
//! * **The field names** `provider`/`kind`/`addrs`/`code`/`error`, by
//!   round-tripping each body through the FAR side's real parser
//!   ([`request_diffs`], [`response_diffs`], [`envelope_diffs`]). weles's
//!   `deny_unknown_fields` is what makes the request direction bite.
//! * **The `resolve` path** ([`path_diffs`]) — weles's `route` match arm against
//!   the URL remote's `format!` actually builds.
//! * **The `AddrKind` pairing itself**, end to end and with no column this stage
//!   hand-copied in between: [`contract_diffs`] drives remote's real `Serialize`
//!   into weles's real `Deserialize` once per variant. This is the check that
//!   holds if [`addr_kind_spellings`] is ever miscollected.
//!
//! ## NOT pinned — read this before assuming a gap
//!
//! This list is the least-audited part of the file and the place a future reader
//! stops looking, so each entry states the REASON, not just the fact. If a
//! reason below is false, that is a bug in this stage.
//!
//! * **The HTTP method and the status↔`ErrorCode` pairing.** Both are
//!   hand-copied (weles `(&Method::POST, …)`; remote `.post(…)`), so this is a
//!   real gap, accepted rather than overlooked: pinning them means importing
//!   hyper's and reqwest's method types into a serde-spelling stage to compare
//!   two constants. A drift makes weles answer `404 unknown_route`, which
//!   `remote::resolve`'s doc calls fatal-for-every-caller — a `cmd/gateway-svc`
//!   that refuses to boot, loudly, on the first dial. Cheap to add if that ever
//!   stops being true.
//! * **`hello`'s body** (`service`/`pid`) and the `/hello` + `/healthz` paths:
//!   `remote` has no client for any of them, so there is no second copy to drift
//!   against. When one is written, it belongs here.
//! * **A variant added to `remote::AddrKind` alone** is caught, but ONLY as a
//!   compile error in [`addr_kind_peer_back`] — not by any runtime check. That
//!   enum is `Serialize`-only, so [`declared_variants`] cannot read its set back
//!   the way it can for both `ErrorCode`s, and every runtime `AddrKind` check
//!   here enumerates from weles's side. Deriving `Deserialize` on it would close
//!   this properly, and is deliberately NOT done: that widens a shipping crate's
//!   surface to serve a verify stage. The compile error is the trade.
//!
//! Everything else about the endpoint — that it binds, serves, and answers the
//! live gateway — belongs to `weles-managed-gateway`, which does that against a
//! real fleet. Nothing in this file should be read as claiming it.

use crate::{model::Outcome, runner::Context};
use anyhow::Result;
use serde::de::{self, DeserializeOwned, Visitor};
use serde::Deserializer;
use std::collections::BTreeSet;
use std::fmt;

type WAddrKind = weles::manifest::AddrKind;
type RAddrKind = remote::AddrKind;
type WErrorCode = weles::agentapi::ErrorCode;
type RErrorCode = remote::ErrorCode;

// ---------------------------------------------------------------------------
// The pair tables.
//
// Rust cannot enumerate an enum, so the variant list IS hand-written — but it
// is NOT hand-maintained in the sense that matters, for two independent
// reasons, both of which have to fail before a drift gets through:
//
//   1. Each `*_peer` function matches EXHAUSTIVELY on weles's enum, so adding a
//      variant there is a COMPILE ERROR in this stage. The only way to fix the
//      error is to write an arm — and an arm can only be written by naming the
//      remote twin, which is the decision this stage exists to check.
//   2. `coverage_diffs` compares the table's length and content against the
//      variant set SERDE ITSELF declares (`declared_variants`). So a table that
//      compiles but is short — the classic vacuous-loop failure — is a FAIL
//      with the missing variant named, not a green pass over nothing.
// ---------------------------------------------------------------------------

/// Exhaustive on `weles::manifest::AddrKind`: a new variant there does not
/// compile until its remote twin is named here.
fn addr_kind_peer(kind: WAddrKind) -> RAddrKind {
    match kind {
        WAddrKind::Edge => RAddrKind::Edge,
        WAddrKind::Http => RAddrKind::Http,
    }
}

/// Exhaustive on `remote::AddrKind` — the OTHER direction, and it exists purely
/// to be a compile error.
///
/// [`addr_kind_peer`] matches on weles's enum, so it says nothing about remote's:
/// a `RAddrKind::PlayerEdge` added alone would compile straight past it. And
/// remote's `AddrKind` is `Serialize`-only, so [`declared_variants`] cannot read
/// its set back either — meaning a remote-only variant would otherwise be
/// invisible to this entire stage. This match is what makes "you cannot add a
/// variant to either enum without touching this file" true by construction
/// rather than by hope.
///
/// It is also load-bearing at runtime, via [`bijection_diffs`]: mapping across
/// and back must land where it started, so a transposed arm (`Edge => Http`) is
/// a FAIL and not a silently mirrored mistake.
fn addr_kind_peer_back(kind: RAddrKind) -> WAddrKind {
    match kind {
        RAddrKind::Edge => WAddrKind::Edge,
        RAddrKind::Http => WAddrKind::Http,
    }
}

/// Exhaustive on `weles::agentapi::ErrorCode`, same contract as
/// [`addr_kind_peer`].
fn error_code_peer(code: WErrorCode) -> RErrorCode {
    match code {
        WErrorCode::UnknownRoute => RErrorCode::UnknownRoute,
        WErrorCode::UnknownPeer => RErrorCode::UnknownPeer,
        WErrorCode::BadRequest => RErrorCode::BadRequest,
        WErrorCode::Internal => RErrorCode::Internal,
    }
}

fn addr_kind_pairs() -> Vec<(WAddrKind, RAddrKind)> {
    [WAddrKind::Edge, WAddrKind::Http]
        .into_iter()
        .map(|kind| (kind, addr_kind_peer(kind)))
        .collect()
}

fn error_code_pairs() -> Vec<(WErrorCode, RErrorCode)> {
    [
        WErrorCode::UnknownRoute,
        WErrorCode::UnknownPeer,
        WErrorCode::BadRequest,
        WErrorCode::Internal,
    ]
    .into_iter()
    .map(|code| (code, error_code_peer(code)))
    .collect()
}

// ---------------------------------------------------------------------------
// Reading serde's own declared variant set.
// ---------------------------------------------------------------------------

/// A `Deserializer` that answers nothing and captures one thing: the variant
/// name list a derived `Deserialize` hands to `deserialize_enum`.
///
/// This is serde's OWN post-`rename_all` list (serde_derive emits it as the
/// type's `VARIANTS` const and passes it here), so it is structural — not the
/// prose of an "unknown variant, expected one of …" error message, which is a
/// human string serde is free to reword between versions. Every other method is
/// an error: a non-enum has no variant set, and answering `deserialize_any` with
/// anything would let a caller mistake "not an enum" for "an enum with no
/// variants".
struct VariantSniffer<'a> {
    out: &'a mut Vec<String>,
}

#[derive(Debug)]
struct SniffError(String);

impl fmt::Display for SniffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SniffError {}

impl de::Error for SniffError {
    fn custom<T: fmt::Display>(message: T) -> Self {
        SniffError(message.to_string())
    }
}

impl<'de> Deserializer<'de> for VariantSniffer<'_> {
    type Error = SniffError;

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        variants: &'static [&'static str],
        _visitor: V,
    ) -> std::result::Result<V::Value, Self::Error> {
        self.out.extend(variants.iter().map(|name| (*name).to_string()));
        // The list is the whole errand; there is no value to build.
        Err(SniffError("variant list captured".into()))
    }

    fn deserialize_any<V: Visitor<'de>>(
        self,
        _visitor: V,
    ) -> std::result::Result<V::Value, Self::Error> {
        Err(SniffError("not a serde enum".into()))
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct identifier ignored_any
    }
}

/// The wire names serde declares for `T`, in declaration order.
///
/// `Err` (never an empty `Ok`) if `T` is not a plain derived enum — a stage that
/// silently compared an empty set would be exactly the vacuous check this one
/// exists to prevent.
fn declared_variants<T: DeserializeOwned>() -> std::result::Result<Vec<String>, String> {
    let mut out = Vec::new();
    // Always `Err`: the sniffer refuses on purpose. What matters is `out`.
    let _ = T::deserialize(VariantSniffer { out: &mut out });
    if out.is_empty() {
        return Err(format!(
            "serde declared no variants for {} — this stage could not read its \
             variant set and must not report a pass over nothing",
            std::any::type_name::<T>()
        ));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// The comparators. Every one of them takes plain data, so a test can drive it
// with a SYNTHETIC drifted pair — a comparator callable only with the real
// types can never be shown to fail, which makes it theatre.
// ---------------------------------------------------------------------------

/// One shared enum variant as each side renders it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Spelling {
    /// The Rust variant name, for the operator.
    variant: String,
    /// Exactly what weles's derive writes.
    weles: String,
    /// Exactly what remote's derive writes.
    remote: String,
}

/// The `resolve` path, hand-copied on both sides (weles's `route` match arm vs
/// remote's URL `format!`). Pure, so a test can drive it with a drifted pair.
///
/// A drift is admittedly NOT silent at runtime — weles answers `404
/// unknown_route`, which is fatal-loud at gateway boot by design. It is pinned
/// because one const turns that boot-time outage into a `--fast` FAIL, not
/// because the risk was unbounded.
fn path_diffs(weles: &str, remote: &str) -> Vec<String> {
    if weles == remote {
        return Vec::new();
    }
    vec![format!(
        "resolve path: weles serves {weles:?}, remote POSTs to {remote:?} — every question \
         would come back 404 unknown_route (\"this agent does not speak the contract\")"
    )]
}

/// The rollout-lease borrow marker, weles's copy against processctl's original.
///
/// Not the `resolve` contract — this argument crosses a different seam (a spawn's
/// argv, not an HTTP body) — but it is the SAME hand-copy problem, with the same
/// pair of crates, and this stage is the same one place that may see both halves.
/// It lives here rather than in a new stage because a second stage would be a
/// second answer to "who checks weles's hand-copies".
///
/// The drift is silent by construction and has already bitten once. weles's
/// `cli::parse` must let this argument through (a borrowed `weles up` meets the
/// parser before `lock::acquire_or_borrow` ever reads argv); before that arm
/// existed, every borrowed run died with "unknown argument". A rename on
/// processctl's side re-creates exactly that — and a test in EITHER crate would
/// still pass, since each would be parsing its own spelling.
fn borrow_marker_diffs(weles: &str, processctl: &str) -> Vec<String> {
    if weles == processctl {
        return Vec::new();
    }
    vec![format!(
        "rollout-lease borrow marker: processctl appends {processctl:?} to a borrower's argv, \
         weles recognises {weles:?} — a borrowed `weles up` would die in its argv parser with \
         \"unknown argument\", so lock::acquire_or_borrow could never be reached and the \
         weles-managed-gateway stage could never borrow verifyctl's lease"
    )]
}

/// Mapping a weles variant across to remote and back must land where it started.
///
/// [`addr_kind_peer`] and [`addr_kind_peer_back`] are two independent hand-
/// written matches; a transposed arm in either (`Edge => Http`) is a real
/// possibility and would otherwise be invisible, since `Edge` and `Http` render
/// identically-shaped bytes. Composing them is what turns two exhaustive matches
/// into an actual bijection.
fn bijection_diffs(pairs: &[(WAddrKind, RAddrKind)]) -> Vec<String> {
    let mut diffs = Vec::new();
    for (weles, remote) in pairs {
        let back = addr_kind_peer_back(*remote);
        if back != *weles {
            diffs.push(format!(
                "AddrKind::{weles:?}: pairs to remote {remote:?}, which maps BACK to \
                 weles {back:?} — `addr_kind_peer`/`addr_kind_peer_back` disagree, so one \
                 of the two tables has a transposed arm"
            ));
        }
    }
    diffs
}

/// Byte-level agreement, variant by variant, plus the guard that the loop ran.
fn spelling_diffs(label: &str, spellings: &[Spelling]) -> Vec<String> {
    let mut diffs = Vec::new();
    if spellings.is_empty() {
        diffs.push(format!(
            "{label}: compared NO variants — the pair table is empty, so this check proved nothing"
        ));
        return diffs;
    }
    for spelling in spellings {
        if spelling.weles != spelling.remote {
            diffs.push(format!(
                "{label}::{}: wire spelling weles={:?} remote={:?} — the two hand-copied \
                 `rename_all` derives disagree",
                spelling.variant, spelling.weles, spelling.remote
            ));
        }
    }
    diffs
}

/// The table this stage compares must cover every variant serde declares. A
/// table that compiles but is SHORT would make [`spelling_diffs`] pass over a
/// hole; this is what makes the hand-written list non-forgettable.
fn coverage_diffs(label: &str, side: &str, declared: &[String], compared: &[String]) -> Vec<String> {
    let declared_set: BTreeSet<&str> = declared.iter().map(String::as_str).collect();
    let compared_set: BTreeSet<&str> = compared.iter().map(String::as_str).collect();
    let mut diffs = Vec::new();
    for missing in declared_set.difference(&compared_set) {
        diffs.push(format!(
            "{label}: {side} declares variant {missing:?} but this stage's pair table does not \
             cover it — extend `weles_wire_contract`'s table (the drift it would have hidden is \
             unchecked until you do)"
        ));
    }
    for extra in compared_set.difference(&declared_set) {
        diffs.push(format!(
            "{label}: this stage's pair table compares {extra:?}, which {side} does not declare \
             — the table is stale"
        ));
    }
    diffs
}

/// What remote's `Deserialize` actually produced when fed weles's bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ReadBack {
    /// weles's variant name, for the operator.
    variant: String,
    /// The bytes weles put on the wire for it.
    wire: String,
    /// `Ok(v)` = the remote variant that came out, debug-printed; `Err(e)` = the
    /// client refused the server's own bytes.
    got: std::result::Result<String, String>,
    /// The remote variant the pair table says should have come out.
    want: String,
}

/// The property that actually matters: the client can READ what the server
/// writes, and reads it as the SAME thing. Byte equality is only a proxy — this
/// is the proof.
fn read_back_diffs(label: &str, read_backs: &[ReadBack]) -> Vec<String> {
    let mut diffs = Vec::new();
    if read_backs.is_empty() {
        diffs.push(format!(
            "{label}: read back NO variants — this check proved nothing"
        ));
        return diffs;
    }
    for read_back in read_backs {
        match &read_back.got {
            Ok(got) if got == &read_back.want => {}
            Ok(got) => diffs.push(format!(
                "{label}::{}: weles writes {:?}; remote reads it as {} but the contract pairs it \
                 with {}",
                read_back.variant, read_back.wire, got, read_back.want
            )),
            Err(error) => diffs.push(format!(
                "{label}::{}: weles writes {:?} and remote CANNOT read it ({error}) — the server \
                 emits a code its own client rejects as not-this-contract",
                read_back.variant, read_back.wire
            )),
        }
    }
    diffs
}

// ---------------------------------------------------------------------------
// The body round-trips: each takes the encoded bytes, so a test can hand them a
// DRIFTED body (a renamed field) and watch the far side refuse.
// ---------------------------------------------------------------------------

/// remote's question, read by weles's real `deny_unknown_fields` parser.
/// `provider` and `kind` must both arrive intact — a renamed field on either
/// side is an `Err` (weles rejects the unknown one AND misses the required one).
fn request_diffs(body: &[u8], want_provider: &str, want_kind: WAddrKind) -> Vec<String> {
    match weles::agentapi::drift_probe_parse_resolve_request(body) {
        Ok((provider, kind)) => {
            let mut diffs = Vec::new();
            if provider != want_provider {
                diffs.push(format!(
                    "resolve request: weles read provider={provider:?}, remote sent \
                     {want_provider:?}"
                ));
            }
            if kind != want_kind {
                diffs.push(format!(
                    "resolve request: weles read kind={kind:?}, remote sent {want_kind:?}"
                ));
            }
            diffs
        }
        Err(error) => vec![format!(
            "resolve request: weles's server CANNOT parse the body its own client sends \
             ({error}); remote sent {}",
            String::from_utf8_lossy(body)
        )],
    }
}

/// weles's answer, read by remote's real parser. Pins `addrs`.
fn response_diffs(body: &[u8], want_addrs: &[String]) -> Vec<String> {
    match remote::resolve::drift_probe_parse_resolve_response(body) {
        Ok(addrs) if addrs == want_addrs => Vec::new(),
        Ok(addrs) => vec![format!(
            "resolve response: remote read addrs={addrs:?}, weles sent {want_addrs:?}"
        )],
        Err(error) => vec![format!(
            "resolve response: remote CANNOT parse the answer weles sends ({error}); weles sent {}",
            String::from_utf8_lossy(body)
        )],
    }
}

/// weles's refusal, read by remote's real parser. Pins `code` AND `error` —
/// `error` is `#[serde(default)]` on remote's side, so a rename there would NOT
/// fail the parse; it would silently blank the operator's only prose. That is
/// exactly why the VALUE is compared here and not just the parse's success.
fn envelope_diffs(body: &[u8], want_code: RErrorCode, want_message: &str) -> Vec<String> {
    match remote::resolve::drift_probe_parse_error_envelope(body) {
        Ok((code, message)) => {
            let mut diffs = Vec::new();
            if code != want_code {
                diffs.push(format!(
                    "error envelope: remote read code={code:?}, weles sent {want_code:?}"
                ));
            }
            if message != want_message {
                diffs.push(format!(
                    "error envelope: remote read error={message:?}, weles sent {want_message:?} \
                     — the `error` field name drifted (remote defaults it, so this is silent)"
                ));
            }
            diffs
        }
        Err(error) => vec![format!(
            "error envelope: remote CANNOT parse the refusal weles sends ({error}); weles sent {}",
            String::from_utf8_lossy(body)
        )],
    }
}

// ---------------------------------------------------------------------------
// Collecting the real types into the comparators' inputs.
// ---------------------------------------------------------------------------

fn wire_of<T: serde::Serialize>(value: &T, what: &str) -> std::result::Result<String, String> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => Ok(name),
        Ok(other) => Err(format!("{what} does not serialize to a JSON string: {other}")),
        Err(error) => Err(format!("{what} does not serialize at all: {error}")),
    }
}

fn addr_kind_spellings() -> std::result::Result<Vec<Spelling>, String> {
    addr_kind_pairs()
        .into_iter()
        .map(|(weles, remote)| {
            Ok(Spelling {
                variant: format!("{weles:?}"),
                weles: wire_of(&weles, "weles::manifest::AddrKind")?,
                remote: wire_of(&remote, "remote::AddrKind")?,
            })
        })
        .collect()
}

/// weles's bytes, deserialized into remote's enum. This is the ErrorCode check
/// that carries the weight; the `Spelling` rows above only name the variants.
fn error_code_read_backs() -> std::result::Result<Vec<ReadBack>, String> {
    error_code_pairs()
        .into_iter()
        .map(|(weles, remote)| {
            let wire = wire_of(&weles, "weles::agentapi::ErrorCode")?;
            // Through remote's REAL envelope parser, not a bare enum parse:
            // that is the code path `resolve_peer` runs, so a drift in either
            // the code spelling or the `code` field name lands here.
            let body = weles::agentapi::drift_probe_encode_error_response(weles, "drift probe");
            let got = remote::resolve::drift_probe_parse_error_envelope(&body)
                .map(|(code, _)| format!("{code:?}"));
            Ok(ReadBack {
                variant: format!("{weles:?}"),
                wire,
                got,
                want: format!("{remote:?}"),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The whole check.
// ---------------------------------------------------------------------------

fn contract_diffs() -> Vec<String> {
    let mut diffs = Vec::new();

    // --- The one hand-copied fact that is not serde's: the path.
    diffs.extend(path_diffs(
        weles::agentapi::RESOLVE_PATH,
        remote::resolve::RESOLVE_PATH,
    ));

    // --- The OTHER hand-copied wire between weles and this workspace.
    diffs.extend(borrow_marker_diffs(
        weles::lock::BORROWED_LEASE_ARG,
        processctl::BORROWED_LEASE_ARG,
    ));

    // --- AddrKind: bytes, both sides, every variant.
    diffs.extend(bijection_diffs(&addr_kind_pairs()));
    match addr_kind_spellings() {
        Ok(spellings) => {
            diffs.extend(spelling_diffs("AddrKind", &spellings));
            match declared_variants::<WAddrKind>() {
                Ok(declared) => diffs.extend(coverage_diffs(
                    "AddrKind",
                    "weles::manifest::AddrKind",
                    &declared,
                    &spellings.iter().map(|s| s.weles.clone()).collect::<Vec<_>>(),
                )),
                Err(error) => diffs.push(error),
            }
        }
        Err(error) => diffs.push(error),
    }

    // --- ErrorCode: the read-back proof, plus coverage on BOTH sides.
    //
    // Byte equality needs no separate check here, and is not a gap: BOTH enums
    // derive `Deserialize`, so serde declares both sets, and each is compared
    // against the SAME list of weles-produced wire names. Two sets that each
    // equal that list equal each other — the spelling agreement falls out of
    // `coverage_diffs`, read from serde rather than from a `remote` derive this
    // stage would otherwise have to guess the rendering of.
    match error_code_read_backs() {
        Ok(read_backs) => {
            diffs.extend(read_back_diffs("ErrorCode", &read_backs));
            let compared: Vec<String> = read_backs.iter().map(|r| r.wire.clone()).collect();
            for (side, declared) in [
                (
                    "weles::agentapi::ErrorCode",
                    declared_variants::<WErrorCode>(),
                ),
                ("remote::ErrorCode", declared_variants::<RErrorCode>()),
            ] {
                match declared {
                    Ok(declared) => {
                        diffs.extend(coverage_diffs("ErrorCode", side, &declared, &compared))
                    }
                    Err(error) => diffs.push(error),
                }
            }
        }
        Err(error) => diffs.push(error),
    }

    // --- The field names, in all three directions that have two copies.
    //
    // Over EVERY `AddrKind` pair, not just one: this is remote's real
    // `Serialize` driven into weles's real `Deserialize`, with no column this
    // stage hand-copied in between. That matters twice over.
    //
    //   * It is the only AddrKind check with no collector seam. `spelling_diffs`
    //     reads two columns `addr_kind_spellings` built; if that collector ever
    //     read the wrong side's type into the `remote` column (a one-word slip,
    //     in exactly the wrong-sided direction this stage exists to catch), the
    //     columns would agree with each other forever and every test would stay
    //     green. This loop cannot be fooled that way, because there is no
    //     column — a `PlayerEdge` spelling drift makes weles's parser reject the
    //     body its own client sent.
    //   * It round-trips `Http`, which nothing else did.
    for (weles_kind, remote_kind) in addr_kind_pairs() {
        diffs.extend(request_diffs(
            &remote::resolve::drift_probe_encode_resolve_request("characters", remote_kind),
            "characters",
            weles_kind,
        ));
    }
    let addrs = vec!["127.0.0.1:9000".to_string()];
    diffs.extend(response_diffs(
        &weles::agentapi::drift_probe_encode_resolve_response(addrs.clone()),
        &addrs,
    ));
    diffs.extend(envelope_diffs(
        &weles::agentapi::drift_probe_encode_error_response(
            WErrorCode::UnknownPeer,
            "no Edge address for provider \"nope\"",
        ),
        RErrorCode::UnknownPeer,
        "no Edge address for provider \"nope\"",
    ));

    diffs
}

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    let diffs = contract_diffs();
    if diffs.is_empty() {
        return Ok(Outcome::Pass);
    }
    eprintln!(
        "verifyctl: weles<->remote agent wire contract drift ({} finding(s)):",
        diffs.len()
    );
    for diff in &diffs {
        eprintln!("  {diff}");
        ctx.note(diff)?;
    }
    let scope = "the two copies are `weles::{manifest::AddrKind, agentapi::{ErrorCode, \
                 RESOLVE_PATH, ResolveRequest, ResolveResponse, ErrorResponse}}` and \
                 `remote::resolve::{AddrKind, ErrorCode, RESOLVE_PATH, ResolveRequest, \
                 ResolveResponse, ErrorEnvelope}`. Zero-sharing forbids sharing the types, so \
                 the fix is to make the two agree — never to relax this stage. This is the EARLY \
                 gate on that contract: the blocking `weles-managed-gateway` stage drives the \
                 same contract over a socket against a real fleet, so a drift left in place \
                 fails there too — but far later and only after a fleet boot. Not checked here: \
                 the HTTP method (which `weles-managed-gateway` does reach), the status<->code \
                 pairing (which NOTHING pins — both sides branch on the `code` field alone and \
                 carry `status` only for logs, so there is no pairing to break), and `hello`'s \
                 body (no second copy exists yet).";
    eprintln!("{scope}");
    ctx.note(scope)?;
    Ok(Outcome::Fail)
}

#[cfg(test)]
#[path = "weles_wire_contract_tests.rs"]
mod weles_wire_contract_tests;
