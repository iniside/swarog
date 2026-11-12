# Player QUIC front presents a production certificate — plan

## Context

**Owner (2026-11-12):** GATEWAY-07 (`docs/todo/gateway.md`) before verifyctl. The player QUIC
port (`PLAYER_EDGE_ADDR`, default `:9100`) must present a production-trusted certificate in
BOTH topologies. Owner decisions: clients (Rust + C#) are in scope; reverse-proxy deployments
get their own `PLAYER_TLS_CERT_PATH`/`PLAYER_TLS_KEY_PATH`; split proof = a gateway-svc
binary-spawn test (no splitproof change).

**Today (research, file:line):**
- `core/app/src/lib.rs:1087-1098` (step 7b of `app::run`) binds the player front with
  `edge::shared_dev_ca()` → `PlayerServer::listen(addr, &DevCA)` (`core/edge/src/player.rs:235-249`)
  → `DevCA::server_tls_public` (`core/edge/src/tls.rs:233-243`: TLS 1.3, ring, no client
  auth, ALPN `PLAYER_ALPN` = `edge-player`, leaf minted at boot with SANs localhost/127.0.0.1/::1).
  The DevCA is `EDGE_CA_CERT/KEY` or ephemeral (`tls.rs:358-381`); compose sets neither.
- Same path in both topologies: `cmd/server` and `cmd/gateway-svc` both parse
  `TlsFront::from_values` and call `app::run` with the player front; HTTPS front and QUIC
  front live in the same process in both.
- Internal edge mTLS (`DevCA::server_tls`, step 7) shares the DevCA and is OUT of scope.
- ACME: `AcmeState` is built inside `serve_https` (`lib.rs:1443-1460`), AFTER step 7b.
  rustls-acme 0.15.3 `AcmeState::resolver()` (`state.rs:150`) is a `ResolvesServerCert`
  that the state driver updates in place → a QUIC config built on it picks up renewals.
  quinn-proto 0.11.16 `QuicServerConfig::try_from` needs TLS 1.3 initial suite and
  `max_early_data_size` 0 (default). rustls 0.23 single version in the lock.
- Clients: Rust `PlayerClient::dial` hardcodes server name `"localhost"`
  (`player.rs:634`) and trusts a single-anchor `TrustAnchor` (`tls.rs:284-306`), no system
  roots; `tools/playercli` requires `--ca`, `--addr` is a `SocketAddr`. C#
  `clients/csharp/Transport/QuicPlayerClient.cs`: `TargetHost = "localhost"` (:74), IP-only
  endpoint (:92), `--insecure` or `PinToCa` whose callback never checks
  `sslPolicyErrors` (no hostname check, :110-128); verifyctl csharp stage uses `--insecure`.
- Every dev/verify fleet runs `TLS_MODE=off` → must stay byte-identical (DevCA leaf).

## Invariants

- One place builds the player rustls `ServerConfig` (TLS 1.3 only, `PLAYER_ALPN`, no client
  auth, early data off); every certificate source goes through it.
- Player certificate source, decided once at boot in `core/app` (edge stays env-blind):
  1. `PLAYER_TLS_CERT_PATH` + `PLAYER_TLS_KEY_PATH` set → those files (both or neither; one
     alone = boot error).
  2. else `TLS_MODE=files` → the same `TLS_CERT_PATH`/`TLS_KEY_PATH` as HTTPS.
  3. else `TLS_MODE=acme` → the SAME `AcmeState` resolver as HTTPS (one ACME account/order,
     renewals reach both fronts without restart; `ACME_DOMAINS` must include the host players
     dial).
  4. else (`off`) → DevCA leaf as today, plus one loud boot warning that players get a dev cert.
- Bad files fail boot (no silent fallback to DevCA). No hot reload in files mode (same as HTTPS).
- Same behaviour in monolith and gateway-svc (no topology branch); internal mTLS untouched.
- Clients default to today's behaviour (server name `localhost`, `--ca` file) so dev fleets,
  splitproof and existing tests keep their certificate and trust; production mode = DNS
  host + OS trust store. Dev fleets' only visible change is the new `off`-mode warning line.

## Steps

### Step 1 — edge: one player TLS builder and a source-agnostic listener `[opus]`
(a) `core/edge/src/tls.rs` / `player.rs`: a player server-config builder taking a
`Arc<dyn rustls::server::ResolvesServerCert>` (or an equivalent source type) that enforces the
invariants; `DevCA::server_tls_public` becomes a caller of it; a constructor for a static
cert chain + key loaded from PEM files (errors name the path). `PlayerServer::listen` takes the
built player config (or a source enum) instead of `&DevCA`. Update callers (in-crate tests
compile-fixed only). Doc comments at `tls.rs:280-283`, `player.rs:5,211-212` describe the
sources truthfully.
(b) First. (d) `core-implementer`, `model:"opus"`. `cargo check -p edge --tests`.
Commit `feat(edge): build the player TLS config from any certificate source (player-quic-production-tls step 1)`.

### Step 2 — app: choose the player certificate source `[opus]`
(a) `core/app`: parse `PLAYER_TLS_CERT_PATH`/`PLAYER_TLS_KEY_PATH` alongside `TlsFront`
(`from_values`, same `get` closure; strict both-or-neither); resolve the source per the
invariant order. ACME: exactly ONE `AcmeState` per process. It is built before step 7b;
its `resolver()` feeds the player config and its `axum_acceptor(..)` is passed INTO
`serve_https` as a parameter. The `AcmeConfig…state()` construction and driver spawn inside
the `TlsFront::Acme` arm of `serve_https` (`lib.rs:~1442-1468`) are deleted. The state is
moved into the single driver task, spawned when the HTTPS listener starts serving (so a
TLS-ALPN-01 challenge can be answered from the first order and a startup failure before
that leaks no task) and aborted when HTTPS serving ends, as today; until the first
certificate exists both fronts refuse handshakes. `off` → DevCA + one `warn!`. Both mains
pass the new values through the existing parse (no new topology code). `compose.yaml`
passes `PLAYER_TLS_CERT_PATH`/`PLAYER_TLS_KEY_PATH` through (bare keys, like `TLS_*`).
(b) After Step 1. (d) `core-implementer`, `model:"opus"`. `cargo check -p app -p server -p gateway-svc --tests`.
Commit `feat(app): serve the player QUIC front with the production certificate (player-quic-production-tls step 2)`.

### Step 3 — server tests `[test-author]`
(a) `core/app` parser tests (both-or-neither, precedence, off/files/acme mapping);
`core/edge` unit tests (builder invariants: TLS1.3-only, ALPN, early data 0; files loader
errors; a resolver-backed config serves the resolver's cert). Binary tests:
`cmd/server/tests/tls_front.rs` — QUIC handshake with the rcgen cert succeeds under
`TLS_MODE=files`; a DevCA anchor fails (previously-wrong branch); `TLS_MODE=off` +
`PLAYER_TLS_*` serves the files cert on QUIC; one-path-only exits non-zero. New
`cmd/gateway-svc/tests/tls_front.rs` spawning the real gateway-svc binary (minimal env:
dev auth flags, edge addr; readiness may be red without peers — assert the QUIC handshake,
not readiness) with `TLS_MODE=files` (the at-risk split path): free `PORT`/`EDGE_ADDR`/`PLAYER_EDGE_ADDR`,
`ORCHESTRATOR_URL` scrubbed; the QUIC bind comes after `Gateway::start`'s bounded peer
refresh (~6s with peers down) — wait on the bind, guard 60s. Every spawn test scrubs
`PLAYER_TLS_*` too. A "DevCA anchor fails" case needs `EDGE_CA_CERT/KEY` set to a CA the
test minted. No sleeps as proof; bounded waits with headroom.
(b) After Steps 2 and 4 (the binary tests dial with the final `PlayerClient::dial(addr,
server_name, trust)`). Windows: bounded waits with headroom (the platform verifier may do
network lookups when rejecting). (d) `test-author`, `model:"sonnet"`; review adds `proof-auditor`.
Commit `test(app,server,gateway-svc): prove the player front serves the production certificate (player-quic-production-tls step 3)`.

### Step 4 — Rust client: server name and system roots `[opus]`
(a) `core/edge`: `TrustAnchor` gains an OS-trust-store variant via `rustls-platform-verifier`
(already in `Cargo.lock`; same semantics as the C# system mode — a private CA installed in
the OS is trusted by both; the cross-checked C-free crates are processctl and weles only
(`tools/verifyctl/src/stages/supported_targets.rs:58`) and neither depends on `edge`;
verifyctl already depends on `edge` via `remote` and is not cross-checked). ONE entry point: `PlayerClient::dial(addr, server_name, trust)`; no forwarding
wrapper; every caller is updated (edge in-crate tests, playercli, and the three splitproof call
sites `tools/splitproof/src/main.rs:~7029,7290,7440` passing `"localhost"` — a mechanical
argument, no assertion change). `tools/playercli`: `--addr` accepts `host:port` (DNS
resolved), `--server-name` (default: the host when it is a DNS name, else `localhost`), trust =
exactly one of `--ca <file>` or `--system-roots`.
(b) After Step 1 (same files). (d) `core-implementer`, `model:"opus"`.
`cargo check -p edge -p playercli -p splitproof --tests`.
Commit `feat(edge,playercli): dial the player front by DNS name with system roots (player-quic-production-tls step 4)`.

### Step 5 — C# client: DNS host, target host, system trust, pinned-mode hostname check `[opus]`
(a) `clients/csharp/Transport/QuicPlayerClient.cs` (+ its options/CLI): endpoint accepts a DNS
name (`DnsEndPoint`); `TargetHost` configurable (default: the host when DNS, else
`localhost`); new system-trust mode = no validation callback (.NET does chain + hostname);
`PinToCa` additionally rejects `RemoteCertificateNameMismatch` (and other non-chain policy
errors); `--insecure` unchanged. Read `docs/reference/csharp-client.md` and the verifyctl
csharp stage first; keep the stage's `--insecure` runs green.
(b) Independent of Steps 1-4 (may run from the start). (d) `core-implementer`, `model:"opus"`; build
with the csharp toolchain the verifyctl stage uses.
Commit `feat(csharp): dial the player front by DNS name with system trust and a hostname-checked pin (player-quic-production-tls step 5)`.

### Step 6 — client tests `[test-author]`
(a) Rust: `PlayerClient` with a non-`localhost` server name against a files cert for that
name; mismatch fails; system-roots anchor rejects a self-made CA (previously the only mode).
playercli arg parsing. C# (no C# test project exists; no new one is added): the verifyctl
csharp stage (`tools/verifyctl/src/stages/csharp.rs`) gains a second fixture boot with
`PLAYER_TLS_*` set to a cert the stage mints (CN/SAN `localhost`) and `gbclient` cases:
pinned mode with matching `TargetHost` passes; mismatching `TargetHost` fails
(previously-wrong branch — today's pin skips the hostname); system mode rejects the self-made
CA. Existing `--insecure` cases unchanged.
(b) After Steps 2, 4, 5. (d) `test-author`, `model:"opus"` (verifyctl stage = proof surface);
review adds `proof-auditor`.
Commit `test(edge,playercli,csharp): prove client server-name and trust modes (player-quic-production-tls step 6)`.

### Step 7 — docs and todo `[docs]`
(a) `README.md` (Run with Docker: QUIC uses the TLS front's cert; the reverse-proxy bullet
~307-312 gains `PLAYER_TLS_*` for QUIC), `docs/reference/hetzner-deploy-checklist.md` (open UDP 9100; `TLS_MODE`
covers QUIC; a QUIC check with playercli `--system-roots`), `docs/reference/csharp-client.md`,
`.env.example`, `.agents/shared/gamebackend.md` native-TLS paragraph, `install.sh`/`install.ps1`
hints; close GATEWAY-07 per the todo convention.
(b) Last. (d) `docs-writer`, `model:"sonnet"`.
Commit `docs: the player QUIC front serves the production certificate (player-quic-production-tls step 7)`.

### Verification / review
`core-reviewer` `model:"opus"` after each step; `proof-auditor` on Step 3. Cargo runs one at a
time. Final: `cargo test --workspace --no-fail-fast`, `cargo run -p archcheck`, then
`cargo run -p verifyctl -- --fast` (owner sign-off for verifyctl) incl. the csharp stage;
live acceptance: `docker compose` with `TLS_MODE=files` + a self-made cert, `playercli --ca`
over a DNS name.
