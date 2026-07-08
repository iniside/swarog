# External C# player client + generator

## What it is / why

`clients/csharp/` (`gbclient`) is an **external, non-Rust** driver of the
player-QUIC front. Every other test in this repo (232+ unit/integration/proptests,
`tools/playercli`) runs inside or alongside the Rust process tree; none proves that
a client with **no access to Rust types** can actually speak the wire contract. This
client fills that gap: it is generated from the same contract source the server
compiles (`api/*/api` crates), then driven over a real QUIC connection against a
live `cmd/server` monolith.

**QUIC is the hard gate.** If .NET's `System.Net.Quic` can't complete the ALPN/TLS
1.3 handshake against `core/edge`'s player plane, nothing downstream matters — that
risk was de-risked first (a standalone spike) before any codegen was built.

The generator, `tools/csharp-client-gen`, is a normal Rust workspace member. It
scrapes the player-reachable surface and emits `clients/csharp/Generated/*.cs`;
everything else in `clients/csharp/` is hand-written and committed once.

## The wire contract

The client speaks the player-edge plane implemented in `core/edge/`:

- **Address:** `:9100` by default (`PLAYER_EDGE_ADDR`).
- **ALPN:** `edge-player`, **TLS 1.3** only.
- **Trust:** server-cert-only — the client presents no client certificate; it
  verifies the server against a CA (dev: trust-any; prod-like: pin to exactly one
  CA file).
- **SNI:** `localhost` (the server leaf carries a `localhost` SAN + loopback IPs).
- **Framing:** one bidirectional QUIC stream per call. Each side writes a **4-byte
  big-endian length prefix + JSON body**, single contiguous write, `completeWrites`
  on the request to signal EOF.
- **Request envelope:** `{ "method": "<prefix.lowerCamel>", "token": string?,
  "payload": <raw JSON> }`.
- **Response envelope:** `{ "ok": bool, "payload"?: <raw JSON>, "error"?: string }`.
  **`ok:false` is a transport fault only** (bad frame, decode error, unknown
  method). A *completed* op — including an auth failure — comes back `ok:true` with
  the domain outcome nested inside `payload` as `{ "status": "...", "err"?:
  string, "value"?: <raw JSON> }` (`Status` is one of `Ok, NotFound, Forbidden,
  Invalid, Unavailable, Internal, Unauthorized, Conflict`).

**Live-verified gotcha:** a no-arg method must send `payload: {}`, never `null`.
The server decodes `payload` into an (empty) request struct; `serde` rejects a bare
JSON `null` there even for a zero-field struct. `gbclient raw` defaults the payload
to `"{}"` for exactly this reason (`clients/csharp/Program.cs`), and
`QuicPlayerClient.CallAsync` only emits `null` when the caller's byte buffer is
literally empty (an already-decided decision, not a default worth relying on —
always pass `{}` for no-arg calls).

## Generator architecture

`tools/csharp-client-gen/src/{scrape.rs,emit.rs,model.rs,main.rs}` implements a
**hybrid** scrape, because no single source of truth has everything:

- **Phase A — runtime `route_bindings()`.** Each provider's generated
  `<name>_rpc::route_bindings()` (impl-free, no DB/lifecycle/tokio) returns the
  *authoritative reachable set*: `Operation{method, verb, path, auth, success}` for
  every `#[http]`-bound op. This is ground truth for "is a player allowed to call
  this" — but it carries no argument names, argument types, or return type
  (`OpBinding.decode/encode` are opaque closures).
- **Phase B — `syn` source parse.** The same `api/*/api/src/lib.rs` crates (plus
  `core/opsapi`) are parsed with `syn` to recover argument names/types (leading
  `Identity` stripped, `#[http(path_args(...))]` and body renames applied), every
  reachable DTO's fields, and the `Status` enum variants.
- **Cross-checks (two gates, both fail the build):**
  - **Drift gate** — Phase A and Phase B are joined on the wire method string
    (`prefix.lowerCamel`); any method present in one set but not the other fails.
  - **Provider-completeness gate** — scans *every* `api/*/api` crate for `#[rpc]`
    traits with `#[http]` methods and fails if that trait's provider prefix is
    missing from the generator's hardcoded `PROVIDERS` list
    (`tools/csharp-client-gen/src/scrape.rs:36`:
    `["characters", "inventory", "accounts", "match", "leaderboard"]`).

Neither phase alone suffices: Phase A has no types, Phase B has no notion of
*reachability* (it would happily parse a wire-only trait like `characters::Ownership`
that is never player-facing). The **provider list is the one conscious edit point**
in the whole generator — everything else is derived.

`--emit-manifest [path]` dumps the internal typed model (`model::Manifest`) as
pretty JSON for debugging/golden tests without touching the C# output.

## Adding a method / a module

- **Adding an `#[http]` method to an already-listed provider trait needs NO
  generator edit.** `route_bindings()` and the `syn` parse both pick it up
  automatically the next time the generator runs; just regenerate and commit the
  diff.
- **Adding a NEW player-facing provider module** (a new `api/<name>/api` crate
  exposing `#[http]` methods) requires exactly one edit: add its prefix to
  `PROVIDERS` in `tools/csharp-client-gen/src/scrape.rs`. Forgetting this is not a
  silent gap — the provider-completeness gate turns it into a hard build failure
  the next time `csharp-client-gen` (or the `codegen-fresh` verify stage) runs.

## Dev vs prod-like mode

Both `gbclient` modes and `QuicPlayerClient.ConnectAsync` take mutually exclusive
`--insecure` / `--ca <path>` trust options:

- **`--insecure`** — dev trust-any: `RemoteCertificateValidationCallback` always
  returns `true`, no CA file needed. Pairs with the server's ephemeral-CA
  auto-generation in dev (`core/edge`'s `dev_ca_from_env`).
- **`--ca <edge-ca.crt>`** — prod-like: loads exactly one CA cert (PEM,
  `X509Certificate2.CreateFromPem`, cert-only — no private key, matching an
  anchor) and validates the server chain against it via
  `X509ChainTrustMode.CustomRootTrust` with **no system-roots fallback**
  (mirrors Rust's `TrustAnchor` with exactly one anchor). Revocation checking is
  off (the dev CA publishes no CRL/OCSP).

**The `dev-<pid>` token shortcut is NOT available to this client.** That path only
activates in `DevSessionVerifier` when a process's gateway has no `accounts.sessions`
capability wired — but every gateway-hosting process in this repo (including
`cmd/gateway-svc`) hard-codes the accounts stub as mandatory, so the capability is
always present and `dev-` tokens are always rejected. Instead, get a real bearer the
cheap way: a trivial `register`/`login` round-trip (2 HTTP calls, same as
`split-proof.sh` does), or just run `gbclient flow` which performs `register` for
you as its first step.

## CLI usage

`gbclient` (built from `clients/csharp/`, entry point `Program.cs`) has two modes:

```
gbclient raw  --addr HOST:PORT (--ca PATH | --insecure) [--token TOK] METHOD [JSON-PAYLOAD]
gbclient flow --addr HOST:PORT (--ca PATH | --insecure)
```

- **`raw`** — drives the untyped `IPlayerTransport` directly with any method
  string (including wire-only ones like `characters.ownerOf`, which the generated
  typed client deliberately omits because it has no `#[http]` binding). Mirrors
  `tools/playercli`'s exit grammar. Prints the response payload to stdout, status
  info to stderr.
- **`flow`** — drives the **generated typed client** (`Generated/Client.cs`)
  end-to-end: register → create character → list characters, asserting the created
  character appears in the list.

Exit codes (both modes):

| Code | Meaning |
|---|---|
| 0 | Transport ok AND domain `status == "Ok"` (raw), or the flow completed and asserted (flow) |
| 1 | Reached the server but outcome not `Ok` — transport fault OR non-Ok domain status (auth failures arrive as `ok:true` per the pinned error grammar) — or a typed-client exception in flow mode |
| 2 | Usage / argument error |
| 3 | QUIC (`System.Net.Quic`/msquic) unsupported on this platform |

## The verify stages

Two independent stages in `verify.sh` / `verify.ps1`, deliberately split by
runtime dependency:

- **`codegen-fresh` (blocking, always runs).** `cargo run -p csharp-client-gen --
  --out clients/csharp/Generated` followed by `git diff --exit-code -- 
  clients/csharp/Generated`. Pure Rust + git — no `dotnet`, no QUIC — so it runs on
  every machine and catches a contract change that wasn't regenerated.
- **`csharp-client` (advisory, SKIP-aware).** Builds `clients/csharp` with
  `dotnet build -c Release`, boots a self-contained monolith
  (`cargo build -p server`, `PORT=:8099`, `PLAYER_EDGE_ADDR=:9100`, ephemeral CA →
  `--insecure`), and runs four named scenarios through `gbclient`: a raw
  `leaderboard.topScores` probe (auth=none), `characters.create` with no token
  (expect exit 1 + `Unauthorized`), `characters.ownerOf` with a bogus token
  (expect exit 1 + `NotFound` — the wire-only method, only reachable via `raw`),
  and `gbclient flow` (typed register→create→list). SKIPs — not FAILs — when
  `dotnet` is absent, or when the first scenario's exit code is `3`
  (`QuicConnection.IsSupported` false). Both `.sh` and `.ps1` implement this in
  lockstep per CLAUDE.md.

## The msquic / Linux-CI caveat

`System.Net.Quic` is stable from .NET 9 but depends on `msquic`. On Windows 11
(this repo's dev box, where `verify.sh`/`.ps1` actually run — "there is no CI —
this IS it") msquic ships in-box, so the `csharp-client` stage runs cleanly. On
Linux, `msquic` requires the separate `libmsquic` package, and recent-distro
packaging has known gaps — so the stage is written to SKIP cleanly there
(`QuicConnection.IsSupported == false` → exit 3 → `add_result csharp-client SKIP
false`) rather than FAIL. The `codegen-fresh` stage has no such dependency and is
the one guaranteed to run everywhere.

## Regeneration

To regenerate the C# client after a contract change:

```
cargo run -p csharp-client-gen -- --out clients/csharp/Generated
```

Generated output (`clients/csharp/Generated/{Client.cs,Dtos.cs,Status.cs}`) is
**committed to the repo**, not built on the fly by the C# project — the
`codegen-fresh` verify stage is exactly the freshness gate that keeps the committed
copy honest against the current contract crates. The emitter is deterministic
(stable provider/method/field ordering, fixed header, no timestamps) so a stale
diff always means a real drift, never generator jitter.
