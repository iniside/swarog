# Plan: External C# client generator + QUIC hard-gate verification

**Date:** 2026-07-08 22:18 · **Revised** after grumpy-reviewer pass (think-hard).
**Status:** DRAFT (pending user approval).

## Goal

Add a **new verification** that exercises the backend from the perspective of an
**external client** — the angle no current test covers (all 232+ tests are
internal-Rust; `playercli` is the only external driver and it is hand-written, not
derived from the contract). The deliverable is a **Rust cmd in this workspace** that
scrapes the player-reachable API and **generates a typed C# client**, plus the
hand-written C# transport/CLI that client sits on, wired into `verify` so it runs on
the safety-net pass.

**QUIC is the hard gate** (user's framing): an external client MUST be able to
establish the player-QUIC connection and invoke a method. We de-risk that FIRST
(Step 1) before investing in codegen.

**Decisions locked with the user:**
- Deliverable scope: **full typed client** (handshake + all `#[http]` methods + C#
  DTOs + `Status` enum).
- Contract source: a **Rust cmd links the `api/*/api` crates** (runtime
  `route_bindings()` for reachability + `syn` for types — see Context §4).
- Generator stack: **Rust, one cmd in the workspace** (`tools/csharp-client-gen`).
- **Dev mode = trust-any server cert (`--insecure`, client-side) + a trivial
  register/login for a real bearer.** The `dev-<pid>` token path was REJECTED: it has
  nowhere to run (Context §5) and the user chose the cheaper trivial-auth path. Dev
  mode requires **zero backend change**.

## Context — overlapping existing systems (Research-before-planning)

Per CLAUDE.md this is a *new* surface, so the mandate is to map what already exists
and justify not extending it. Findings (two research rounds, 9 subagents + direct
reads + an independent grumpy review that verified claims against source).

### 1. The player-QUIC plane (the hard gate) — `core/edge/`
- Transport: quinn 0.11 + rustls, **TLS 1.3 only**, ALPN **`edge-player`**
  (`core/edge/src/tls.rs:43`), SNI **`localhost`** (`player.rs:225`), default port
  **`:9100`** (`core/app/src/lib.rs:27`, env `PLAYER_EDGE_ADDR`).
- Trust: **server-cert-only** — client presents NO cert, verifies the server against
  the dev-CA (`TrustAnchor::client_tls_public`, `tls.rs:296-304`; `load_cert_only`
  `tls.rs:260-275`). In dev the server can auto-generate an ephemeral CA
  (`dev_ca_from_env`, `tls.rs:356-368`).
- Wire: **4-byte big-endian length prefix + JSON body** (`frame.rs:17-57`), one framed
  request/response per bidirectional stream, persistent connection, 1 MiB cap.
- Request `PlayerRequest` (`player.rs:54-65`): `{ "method": String, "token": String?,
  "payload": <raw JSON> }`. No-arg calls send `payload: null` (valid RawValue; the
  front's well-formedness gate `modules/gateway/src/lib.rs:260` accepts it — reviewer-
  confirmed).
- Response `Response` (`wire.rs:29-36`): `{ "ok": bool, "payload"?, "error"? }`.
  `ok:false` = transport fault ONLY; a completed op is `ok:true` with the domain
  `{status, err, value?}` inside `payload`.
- **Why not extend an existing client:** the only external driver is `tools/playercli`
  (single hardcoded call, no types). A C# client cannot link Rust; it must
  re-implement the transport — that is the point of the exercise.

### 2. The gateway allow-list — `modules/gateway/src/lib.rs`
- Player reachability gate: `find_by_method` (`:387-389`) matches ONLY ops in the
  route table, and only `#[http]`-bound ops are contributed. Wire-only methods
  (`characters.ownerOf`, `accounts.verifySession`, `config.snapshot`,
  `admin.adminData`, `rating.mmr`) are invisible to a player.
- Wire method string = **`prefix.lowerCamel`** (`tools/rpc-macro/src/lib.rs:330`,
  `to_lower_camel` `:1002`) — reviewer-confirmed. Live in split-proof:
  `inventory.listCharacter`, `leaderboard.topScores`, `characters.ownerOf`.
- No rate-limit on the QUIC plane; only transport caps.

### 3. The player-reachable surface — the 12 `#[http]` methods
| Wire method | Verb / Path | Auth | Args (Identity stripped) | Return |
|---|---|---|---|---|
| `accounts.register` | POST /accounts/register | none | email, password, display_name (body→`displayName`) | Session |
| `accounts.login` | POST /accounts/login | none | email, password | Session |
| `accounts.loginEpic` | POST /accounts/login/epic | none | id_token | Session |
| `accounts.me` | GET /accounts/me | player | — | MeView |
| `characters.create` | POST /characters | player | name, class | Character |
| `characters.list` | GET /characters | player | — | Vec\<Character\> |
| `characters.delete` | DELETE /characters/{id} | player | character_id (path `id`) | () |
| `inventory.listMine` | GET /inventory/me | player | — | Vec\<Holding\> |
| `inventory.listCharacter` | GET /inventory/character/{id} | player | character_id (path `id`) | Vec\<Holding\> |
| `inventory.grant` | POST /inventory/me/grant | player | item_id, qty (i64) | Vec\<Holding\> |
| `match.report` | POST /match/report | none | winner, loser | () |
| `leaderboard.topScores` | GET /leaderboard | none | — | Vec\<Score\> |

DTOs (`api/*/api/src/lib.rs`, serde-default **snake_case** wire keys):
`Session{player_id,token}`, `IdentityRef{provider,subject}`,
`MeView{player_id,display_name,identities:Vec<IdentityRef>}`,
`Character{id,player_id,name,class,created_at}` (created_at = Postgres text → keep
`string`), `Holding{owner_type,owner_id,item_id,item_name,quantity:i64}`,
`Score{player,wins:i64}`. `Status` enum (`core/opsapi/src/lib.rs:124-146`): Ok,
NotFound, Forbidden, Invalid, Unavailable, Internal, Unauthorized, Conflict.
**Rename reality (reviewer #4):** the only *body* rename is request-side
`register.display_name`→`displayName`; the DTO **response** fields are plain
snake_case (`player_id`, `created_at`, `owner_type`, …). System.Text.Json is
case-sensitive → the generator MUST map every field, not just renamed ones.

### 4. Metadata recoverability — the architecture-deciding finding
- **Runtime `route_bindings()`** (`tools/rpc-macro/src/lib.rs:485`, impl-free, no DB/
  lifecycle/tokio; emitted into the pure `*api` crate — reviewer-confirmed) returns
  `Operation{method,verb,path,auth,success}` (`opsapi:237-248`) and **nothing else** —
  `OpBinding.decode/encode` are opaque closures; no arg names/types/return type.
- **Typed shape** lives ONLY in api-crate source and is recovered with **`syn`** —
  the job `build_method`/`result_ok_type` (`rpc-macro:257-394`) already do.
- `schemars` is absent workspace-wide; adding it is invasive — rejected.
- **Chosen architecture: hybrid, in ONE cmd.** `route_bindings()` for the
  authoritative reachable set + transport facts; `syn` source-parse of the same crates
  for typed shape; cross-check the two (drift gate) + a provider-completeness scan
  (reviewer #6). One cmd with an internal typed model (a `--emit-manifest` JSON flag
  for debugging/golden tests) — two cmds were over-scoped for 6 DTOs / 12 methods
  (reviewer #9).

### 5. Dev mode — trivial auth, no backend change
- **`dev-<pid>` token path REJECTED:** `DevSessionVerifier` (`verifier.rs:57-73`) runs
  only when the `accounts.sessions` capability is ABSENT (`resolve_verifier:111-124`).
  `cmd/gateway-svc/src/main.rs:62-66` hard-codes the accounts stub as mandatory, so the
  capability is always present and `dev-` is always rejected (split-proof `[A5]`,
  `[M2]`). No existing process hosts a gateway without accounts. Reaching it would
  need a new binary — not worth it (user decision).
- **Adopted dev mode:** client `--insecure` = `RemoteCertificateValidationCallback
  => true` (no CA file; pairs with the server's ephemeral-CA auto-gen — reviewer-
  confirmed sound) **+** a trivial `register`+`login` (2 HTTP calls) for a real bearer,
  exactly as split-proof already does (`:420-437`). Prod-like mode loads the real
  `edge-ca.crt` and pins via `X509Chain`.

### 6. .NET QUIC feasibility — GO
- `System.Net.Quic` stable from **.NET 9**; this box has SDK **9.0.315 + 10.0.301** →
  target `net9.0`. Custom ALPN via `SslApplicationProtocol("edge-player")`; no client
  cert; trust-any via the validation callback; bidi stream via
  `OpenOutboundStreamAsync(Bidirectional)` + `WriteAsync(..., completeWrites:true)` +
  `ReadAsync` loop — maps 1:1.
- **Caveat:** msquic is in-box on Windows 11 (this dev box) — clean. Linux CI would
  need `libmsquic`. Mitigated: `verify.sh` runs locally on this Windows machine
  (CLAUDE.md: "there is no CI — this IS it"); the C# run/build stage **SKIPs** if
  `dotnet` absent or `QuicConnection.IsSupported == false` (fuzz-stage convention).

### 7. Harness integration points — `verify.sh` (+ `.ps1`); `split-proof.sh` studied only as a scenario reference
- `playercli` contract to mirror: `--addr --ca --token <method> [json]`, **exit 0 iff
  transport ok AND `status=="Ok"`** (`playercli/src/main.rs:120-129`).
- Scenario shapes (from split-proof `:824-898`): create ok; listCharacter ok;
  missing/bad token → Unauthorized+exit1; wire-only `ownerOf` → NotFound+exit1. The
  wire-only `ownerOf` is **not in the generated typed client** (no `#[http]`) — it can
  only be driven through the raw CLI path (reviewer #8).
- `verify.sh` PASS/FAIL/SKIP: `simple_stage` (`:113-124`) only PASS/FAILs; SKIP needs a
  bespoke function like `fuzz_stage`/`cargo_audit_stage` (`:237-270`,`:142-163`) that
  calls `add_result <name> SKIP false` (reviewer #7).
- **Both `.sh` and `.ps1` exist and must be extended in lockstep** (CLAUDE.md). The C#
  scenarios do NOT go into `split-proof.sh` (a blocking stage with no SKIP path —
  reviewer #2); they live in the self-contained advisory C# stage.
- Build lists: `split-proof.sh:221`, `run.sh:75/78`, workspace `Cargo.toml` members.

## Layout of new artifacts
```
tools/csharp-client-gen/       # NEW Rust cmd — the whole generator [Steps 2-3]
  src/main.rs                  #   route_bindings() ∪ syn types → C# emit; --emit-manifest
  src/scrape.rs  src/emit.rs   #   internal typed model + C# emitter
  src/tests.rs                 #   golden manifest JSON + golden .cs
clients/csharp/                # the C# client project
  GameBackend.Client.csproj    #   net9.0 — OWNED by Step 1, extended later
  Transport/IPlayerTransport.cs#   HAND, the pinned seam (Step 1 stub → Step 4 impl)
  Transport/QuicPlayerClient.cs#   HAND transport: framing, TLS, dev/prod [Step 4]
  Cli/Program.cs               #   HAND CLI: raw playercli-parity + typed-flow mode [Step 4]
  Generated/                   #   GENERATED: Dtos.cs Status.cs Client.cs [Step 3]
```

---

## Steps (ordered — what / why-now / how / dispatch tag)

### Step 1 — Hard-gate spike + pinned seam `[inline]`
**(a) What:** Minimal C# console app at `clients/csharp/` that connects to a **live
monolith** player plane and invokes `leaderboard.topScores` (auth=none → no token,
simplest call) end-to-end. Creates `GameBackend.Client.csproj` (`net9.0`) and the
**`IPlayerTransport` interface stub** both later steps code against (reviewer #5).
**(b) Why now:** THE hard gate — if .NET can't speak the plane (ALPN/TLS1.3/trust-any/
framing) every later step is wasted. Pinning `IPlayerTransport` here fixes the seam so
the generator (Step 3) and the transport impl (Step 4) cannot drift.
**(c) How:**
- `QuicClientConnectionOptions` with `ApplicationProtocols=[SslApplicationProtocol
  ("edge-player")]`, `TargetHost="localhost"`, `RemoteCertificateValidationCallback
  => true`, no `ClientCertificates`. Guard on `QuicConnection.IsSupported`.
- `OpenOutboundStreamAsync(Bidirectional)`; write `[4-byte BE len]["{"method":
  "leaderboard.topScores","payload":null}"]`; `completeWrites:true`; read len+body;
  parse `{ok,payload,error}`.
- `IPlayerTransport`: `Task<Response> CallAsync(string method, string? token,
  ReadOnlyMemory<byte> payload, CancellationToken ct)` + a `Response{bool Ok; JsonNode?
  Payload; string? Error}` record. The spike implements it minimally.
- Run target: `cargo run -p server` with `PLAYER_EDGE_ADDR=:9100` (ephemeral CA →
  trust-any needs no file).
- `[inline]`: exploratory mid-edit judgment against a live process — the one step
  where handoff loses the observe-adjust loop.
**Exit criteria:** prints a `{"ok":true,...}` response (empty `[]` array is a PASS).

### Step 2 — Scraper half of `tools/csharp-client-gen` `[opus]`
**(a) What:** The Rust cmd's scrape phase → an internal typed model (+ `--emit-manifest`
JSON): per method — provider, wire method, verb, path, auth, success, args (name, type
category, body/path rename); a DTO registry (fields+types, recursed); `Status`
variants.
**(b) Why now:** Source of truth the emitter (Step 3) consumes; independent of the C#
spike, sequenced after it (no point generating for an unproven plane).
**(c) How:**
- Deps: `charactersapi, inventoryapi, accountsapi, matchapi, leaderboardapi, opsapi,
  serde, serde_json, syn (full, extra-traits)` — no `edge`/DB/tokio. Add to workspace
  members. (`tools/*` linking `api/*` breaks no archcheck — reviewer-confirmed.)
- **Phase A (runtime reachability):** call `<crate>::<snake>_rpc::route_bindings()` for
  the 5 providers; collect `operation.{method,verb,path,auth,success}`. Provider list =
  one commented edit point (topiccheck-style).
- **Phase B (syn types):** `syn::parse_file` the 5 `api/*/api/src/lib.rs` +
  `core/opsapi/src/lib.rs`; extract `#[http]` method sigs (arg idents+types,
  leading-`Identity` strip `rpc-macro:309-314`, `#[http(path_args(...))]` + body
  renames), every reachable `pub struct` DTO (recurse `Vec<IdentityRef>`), field
  `#[serde(rename)]`, and the `Status` variants.
- **Two gates:** (i) **drift** — join A∩B on the lowerCamel wire string (reimplement
  the 3 pure name fns); FAIL if any route_bindings method lacks a parsed sig or vice-
  versa. (ii) **provider-completeness** (reviewer #6) — scan ALL `api/*/api` crates for
  `#[rpc]` traits containing `#[http]` methods; FAIL if any such trait's provider is
  absent from the hardcoded list. This turns "forgot a new module" into a build
  failure, closing the gap vs a hardcoded list.
- Type category enum: String/I64/Unit/Vec\<X\>/Struct\<Name\>.
- Tests (`src/tests.rs`): golden manifest JSON for the 12 methods + DTOs; assert both
  gates fire on synthetic mismatches.
- `[opus]`: syn parsing + identity-strip + rename + two gates are correctness-critical.

### Step 3 — Emitter half → typed C# `[opus]`
**(a) What:** Same cmd's emit phase → `clients/csharp/Generated/`: `Dtos.cs`,
`Status.cs` (enum, JSON string names), `Client.cs` (request/response records + one
typed async method per reachable op) coding against `IPlayerTransport`.
**(b) Why now:** Needs Step 2's model; precedes Step 4's transport impl (which
implements the seam Step 3 targets).
**(c) How:**
- **Field mapping (reviewer #4):** set `JsonSerializerOptions.PropertyNamingPolicy =
  SnakeCaseLower` **and/or** emit `[JsonPropertyName("<wire>")]` on **every** DTO
  field (not just renamed) — snake_case wire keys will silently null otherwise.
- Type map: `String`→`string`, `I64`→`long`, `Unit`→`Task` (no value), `Vec<T>`→`T[]`,
  `Struct<N>`→`N`; `created_at` stays `string`. Request body renames (`displayName`)
  via `[JsonPropertyName]`.
- Each typed method builds the request DTO, serializes to `payload`, calls
  `IPlayerTransport.CallAsync(wireMethod, token, payload)`, deserializes
  `payload.value` into the return DTO, throws a typed `GameBackendStatusException` on
  `status != Ok`. `auth=none` methods take no token param. Wire-only `ownerOf` is NOT
  emitted (not `#[http]`).
- **Deterministic output** (stable provider/method/field order, fixed header, no
  timestamps) so the freshness gate's `git diff --exit-code` is meaningful.
- Tests: golden `.cs` snapshot from the committed golden manifest.
- `[opus]`: type/rename correctness is the crux of "typed client is actually correct".

### Step 4 — C# transport impl + CLI (hand-written) `[opus]`
**(a) What:** Promote the spike into `Transport/QuicPlayerClient.cs` implementing
`IPlayerTransport` (dial, ALPN, `--insecure` trust-any vs prod CA-pinning, 4-byte
framing, bi-stream-per-call, `Response` decode, pinned error grammar) and
`Cli/Program.cs` with **two explicit modes** (reviewer #8):
  - **raw** (`playercli`-parity): `--addr --ca --token <method> [json]`, prints the
    payload, **exit 0 iff transport ok AND `status=="Ok"`**. Used for the
    Unauthorized/NotFound/ownerOf scenarios (inspects the raw status string).
  - **flow**: drives the generated typed client for register→login→create→list.
**(b) Why now:** The generated typed client is useless without the transport; the CLI
is what the C# verify stage invokes. Depends on Step 1 (proven handshake) + Step 3
(generated surface).
**(c) How:** reuse Step-1 handshake; prod mode loads `--ca` PEM into `X509Certificate2`,
validates via `X509Chain`+`CustomTrustStore`. Exit-code/error grammar identical to
`playercli/src/main.rs:106-129`.
- `[opus]`: TLS/framing correctness is protocol- and security-critical.

### Step 5 — verify.sh + verify.ps1 wiring (two independent stages) `[sonnet]`
**(a) What:** Add the new cmd to build lists and TWO stages: an **always-run
freshness stage** (blocking) and a **SKIP-aware C# stage** (advisory).
**(b) Why now:** Integration is last — depends on generator (2,3) + client (4).
**(c) How (reviewer #2, #3, #7):**
- Build lists: add `tools/csharp-client-gen` to `Cargo.toml` members +
  `cargo build -p …` in `split-proof.sh:221`/`run.sh`.
- **Freshness stage `codegen-fresh` (blocking, pure Rust+git, always runs — reviewer
  #3):** `cargo run -p csharp-client-gen -- --out clients/csharp/Generated` then
  `git diff --exit-code clients/csharp/Generated`. Needs no dotnet/QUIC → runs on
  every machine, catching stale generated code where it matters. Register via
  `simple_stage codegen-fresh true …`.
- **C# stage `csharp-client` (advisory, SKIP-aware — reviewer #7):** a bespoke
  `csharp_stage()` (modeled on `fuzz_stage`): if `dotnet` missing OR a tiny
  `QuicConnection.IsSupported` probe is false → `add_result csharp-client SKIP false`
  and return. Else: `dotnet build clients/csharp`, start a **self-contained monolith**
  (`server` on `:9100`, ephemeral CA — reviewer #2: NOT injected into split-proof),
  run the raw scenarios (create ok, listCharacter ok, no/bad token → Unauthorized,
  ownerOf → NotFound) + the typed flow (register→login→create→list), tear down,
  `add_result` PASS/FAIL. Real bearer via the same register/login curl split-proof
  uses; `--insecure` for trust-any.
- Mirror ALL of this in `verify.ps1` (dedicated PS function; same SKIP logic).
- `[sonnet]`: mechanical, fully specified — shell/ps1 mirroring existing patterns.

### Step 6 — Reference doc `[sonnet]`
**(a) What:** `docs/reference/csharp-client.md`: how the generator runs, dev vs
prod-like, the wire contract, that adding an `#[http]` method needs no tool edit (but
a new provider module does — and the completeness gate enforces it), and the msquic/
Linux-CI caveat.
**(b) Why now:** Durable knowledge; documents the finished shape. `[sonnet]`.

## Risks & mitigations
- **msquic on non-Windows:** C# stage SKIPs on `!IsSupported`/`!dotnet`; freshness
  stage (Rust-only) still runs everywhere. Documented (Step 6).
- **syn drift from macro rules:** drift gate + provider-completeness gate (Step 2) +
  freshness gate (Step 5) — three independent nets.
- **Nondeterministic codegen:** Step 3 pins output order/header so `git diff` is sound.
- **Generated/hand boundary:** `IPlayerTransport` pinned in Step 1, csproj owned by
  Step 1 — generator and transport code against a fixed seam (reviewer #5).
- **Two topologies / monolith-only:** the C# stage runs against a live monolith on the
  same `:9100` player front the split uses; the transport is topology-blind (same as
  `playercli`). Not a monolith-only feature — it exercises the public front either
  topology serves.

## Dispatch summary
Step 1 `[inline]` · Steps 2,3,4 `[opus]` (`model:"opus"`) · Steps 5,6 `[sonnet]`
(`model:"sonnet"`). Each code-writing subagent gets the nav guidance pasted and its
lane's `Co-Authored-By` trailer (Opus 4.8 / Sonnet 4.6). Review each diff against its
step before dispatching the next; commit after each step.
