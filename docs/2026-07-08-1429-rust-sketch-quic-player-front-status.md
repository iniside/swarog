# rust-sketch: QUIC player front + gateway-svc — status (DONE, split-verified live)

**Date:** 2026-07-08 14:29
**Plan:** `docs/plans/2026-07-08-1330-rust-sketch-quic-player-front-plan.md`
**Commits:** `7fe9035` (Step 1) → `78d44f6` (Step 8), all on master, trailers audited
per lane (`[fable]`→Fable 5, `[opus]`→Opus 4.8, `[sonnet]`→Sonnet 4.6).

## What exists now

- **`edge` player plane** (`core/edge/src/player.rs` + `tls.rs`): server-cert-only
  TLS (`DevCA::server_tls_public`), ALPN `edge-player` (planes cannot cross —
  proven both directions by e2e tests), envelope `PlayerRequest{method, token,
  payload}` — `token` is a CLAIM, never an identity; the internal trusted
  `wire::Request` is not accepted on the public port. 1 MiB frame cap +
  explicit `quinn::TransportConfig` (16 bidi streams/conn, 30 s idle, window =
  max frame). `TrustAnchor`/`DevCA::load_cert_only` — a client trusts with the
  CA cert only, no key.
- **`gateway::FrontDoor`**: ONE route table + verifier fronting BOTH planes —
  the axum HTTP fallback (unchanged behavior) and `player_handler` (JSON gate →
  method allow-list via `find_by_method` → auth-once per the op's `AuthReq` →
  Local/Remote dispatch). Pinned error grammar: transport `ok:false` only for
  transport faults; every domain outcome (auth failures included) is the
  generated `{status, err}` envelope. Remote-caller cache now evicts on error
  (provider restart self-heals next request).
- **`remote::Stub` contributes `route_bindings()`** for its provider — any
  stub-holding process is front-capable; gateway-svc is stub-only by invariant
  (a Stub and its provider module are mutually exclusive in one process).
- **`app::run`**: 4th param (optional shared `PlayerServer`), `PLAYER_EDGE_ADDR`
  (default `:9100`), `Config::without_db()` (pure-transport process: no pool,
  `/readyz` plain 200). Teardown: HTTP → player edge → internal edge → bus →
  modules.
- **`cmd/gateway-svc`**: the single front door — HTTP `:8082` + player QUIC
  `:9100`, no DB, no messaging, Stubs for characters+inventory, every op
  dispatched Remote over the mTLS edge. **`cmd/server` (monolith) serves the
  player QUIC front too** (all ops Local) — no monolith-only/split-only gap.
- **`cmd/inventory-svc` serves its edge** (`:9001`) — required because
  gateway-svc dispatches `inventory.*` Remote. Rule (same as Go): edge server ⇔
  the process hosts a provider some peer calls synchronously. Monolith,
  messaging, config: no edge server.
- **`tools/playercli`**: script-friendly QUIC driver; exit 0 iff transport ok
  AND payload `status == "Ok"`.

## Proven live (`split-proof.sh`, committed, repeatable — ran green 2026-07-08)

3-process split (A `:8080`/edge `:9000`, B `:8081`/edge `:9001`, G `:8082`/QUIC
`:9100`, shared dev CA):
- All pre-existing assertions kept green (create 201, starter sword async A→B,
  403 cross-player, delete 204, holdings wipe via event).
- **P1** player QUIC create → G → mTLS edge → A ⇒ ok.
- **P2** player QUIC `inventory.listCharacter` → G → Remote → **B's new `:9001`
  edge** → `owner_of` QUIC → A ⇒ ok (the full new composition).
- **P3** G's HTTP `:8082` routes `inventory.*` cross-provider to B ⇒ 200.
- **P4/P4b** no token / bad token ⇒ exit 1 + `{status:"Unauthorized"}`.
- **P5** wire-only `characters.ownerOf` ⇒ exit 1 + `{status:"NotFound"}` — the
  allow-list gate live (internal methods unreachable from the public port).
- **Monolith parity**: `cmd/server` + `PLAYER_EDGE_ADDR` boots, playercli create
  ⇒ ok.

This closes the gap the Go backend left open: Go's `:9100` QUIC front is an
unauthenticated prefix relay that additionally requires mTLS client certs (a
real player cannot even handshake). The Rust front is player-connectable
(server-cert-only) AND authenticated (bearer verified at the front against the
op's `AuthReq`) AND allow-listed (no blind prefix relay).

## Honest limits (unchanged from plan's out-of-scope)

- `DevSessionVerifier` (`Bearer dev-<id>`) — real sessions land with accounts (M2)
  via the `Gateway::with_verifier` seam.
- No rate limiting/conn limits beyond the quinn transport knobs (Go's gateway-svc
  has per-IP limits; the sketch does not).
- JSON codec, stream-per-call, no push/streaming/pipelining.
- No admin fan-out through gateway-svc (no admin module in the sketch yet — M2).
- Players verify the dev CA; production would present a WebPKI cert at the
  `server_tls_public` seam.
