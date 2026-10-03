# Push multiplex (hub lanes + WS invoke) and the `party` fortress — plan

Status (2026-10-03): Steps 1-5 (hub lanes, invoke/result frames, their tests, and the
invoke split-proof) and the push-protocol docs from Step 12 have landed. Steps 6-11
(the `party` fortress) are deferred by the user and NOT implemented; the party-specific
Step 12 items (CLAUDE.md party entry and port list, `module-reference.md`, the Parties
tracker row, the client revision rule) are likewise not done.

Status of the plan text: r3, review cap reached. Round 1 (core-reviewer, opus, think hard): REJECT,
24 items. Round 2 (fresh core-reviewer): REJECT, 11 items. All addressed below (see
the two disposition tables); no third review round per the 2-round cap — the
per-step core-reviewer passes during implementation are the next gate.
Scope decided with the user 2026-09-28: the protocol change **plus** the first domain
hub (`party`). Chat and lobby are separate plans.

## Context

### Goal

One `GET /push` WebSocket carries several independent feature streams ("hubs") at
once — party now, chat/lobby later — in both directions:

- **server→client**: every `message` frame is labelled with its hub and each hub has
  its own outbound lane, so a flood on one hub cannot evict another hub's frames;
- **client→server**: an `invoke` frame calls a player op and gets a correlated
  `result` frame — the same op catalog, auth, API-key policy and Local/Remote
  dispatch that HTTP and player-QUIC use.

The hub is a **namespace, not a new registry**: for invoke it is the op's wire-method
prefix (`party.accept` → `party`, already unique per process by the rpc `prefix`), for
events it is the topic prefix (`party.state` → `party`). No new contribution slot, no
new macro attribute, no change to the backplane wire format.

### Research method

Five parallel read-only subagents (opus ×4, sonnet ×1; targeted `rg` anchors then
end-to-end reads; no cargo), angles: opsapi + rpc-macro surface; gateway dispatch
paths; push hub internals; party overlap + fortress checklist; verify/proof surface.
Plus one core-reviewer pass that re-read the cited code. `git log`/`git diff` were
unavailable (Xcode license), so the storage-rollout checklist is reconstructed from
grep over tools/cmd/weles — a lower bound; the fleet-drift preflight, `checkmodules`,
and the self-checking hand-lists (`opscatalog-gen`, `topiccheck` golden,
`csharp-client-gen`) are the backstop in Steps 8–9.

### Verified facts the design rests on

- **Player-QUIC's allow-list IS the route table.** `FrontDoor::handle_player`
  (`modules/gateway/src/lib.rs:843-887`): JSON gate → `table.find_by_method` (`:1260`,
  exact match over `#[http]`-bound ops only) → `self.admit(api_key, token, op.auth,
  KeyCheck::Policy(&op.method))` (`:869-880`) → `table.dispatch` (`:1284-1300`, Local by
  `LOCAL_SLOT` invoker, else Remote via `PEER_SLOT` pool honouring `retry_mode`) →
  `{status, err, value?}` envelope bytes (`front_envelope`, `:1066`). `table.dispatch`
  itself is unbounded in time. A non-`#[http]` op is unreachable by players today.
- `push_ws` is a child module of the gateway crate root, so `handle_player` and
  `front_envelope` are already visible to it; no visibility change.
- `/push` admits with `KeyCheck::PresenceOnly` (`push_ws.rs:1151`) and then drops the
  api key; only `player` and `bearer` reach `serve` (`:1189`).
- `serve` (`:1271-1361`) is one unbiased `select!` over queue / socket / reverify tick.
  Every inbound Text/Binary frame is charged against `MAX_VERBS_PER_INTERVAL = 64`
  (`:252`), refilled only on the reverify tick (`:1326`); over budget the frame is
  silently dropped (`:1309-1320`). The budget exists to bound acquisitions of the
  process-wide registry `Mutex` by group verbs (doc `:240-251`).
- An inbound frame over `max_frame_bytes` (32 KiB default, `:107`, applied at
  `:1079-1080`) makes `recv` return `Err` and `serve` `break`s with no typed close
  (`:1305`).
- `ConnQueue` (`:445-528`) is one drop-oldest ring per connection (`PUSH_QUEUE_DEPTH`,
  default 64, frames ≤ `MAX_PAYLOAD_BYTES` 32 KiB, `:266,485`). The test helpers
  `queued()`/`queued_close()` read that ring directly (`push_ws_tests.rs:91-94`).
- `CloseCode::Protocol`'s reason text is "malformed handshake frame" (`:387`);
  `on_client_frame`'s doc (`:1363-1368`) and `ClientFrame`'s doc (`:268-271`) promise
  every live-connection rejection is ignored.
- `ClientFrame` is `#[serde(tag = "type")]` (`:272-274`); `serde_json::RawValue` does
  not deserialize inside an internally tagged enum.
- `push::Target`/`push::Message` changes alter the backplane `Envelope` JSON
  (`core/push/src/lib.rs:106,134`) and an old front fails the whole batch. **This plan
  changes neither.**
- `httpmw::IpLimiter` (`core/httpmw/src/limiter.rs:87,125`) refuses every new IP once
  `max_visitors` is reached (`:134-137`) unless `spawn_eviction_task` (`:160`) runs; it
  uses `std::time::Instant`.
- Graceful stop: HTTP drains (`HTTP_DRAIN_GRACE_MS`), QUIC drains
  (`EDGE_DRAIN_GRACE_MS`); `/push` closes with `Shutdown` inside `PUSH_STOP_GRACE`
  (500 ms of a 4500 ms budget, `push_ws.rs:45-57`).
- Producer pattern: `ctx.push()` → `Push::send`, sync, non-blocking, best-effort
  (`modules/notifications/src/service.rs:292-300`); a domain svc reaches the fronts via
  `remote::PushSender::new(wiring.peer_or("gateway","127.0.0.1:9013"))`
  (`cmd/notifications-svc/src/lib.rs:19-27`) + fleet `peer(…, "GATEWAY",
  GATEWAY_EDGE_PORT)` (`tools/processctl/src/fleet.rs:801-805`), not a fleet dependency.
- `dev-key-client`'s policy is the hand list `DEV_CLIENT_POLICY`
  (`modules/apikeys/src/lib.rs:105-116`); splitproof's push sockets use that key
  (`tools/splitproof/src/main.rs:3464,3519`).
- `#[http]` DTO scalars are `i64`/`String` only (`api/groups/api/src/lib.rs:11-15`).
- Conformance has four conventions — `EnvValidation`, `InputByteCaps`,
  `InfraOutage503`, `ArgonParity` (`tools/conformance/src/model.rs:13-26`); the handle
  cap precedent is groups at `policy.rs:559-563` using `accountsapi::MAX_HANDLE_BYTES`.

### Overlapping systems — why not extend / depend on them

| Existing | What it does | Why not extend it for this |
|---|---|---|
| Socket.IO (discussed 2026-09-28) | namespaces, acks, rooms, recovery | Multi-node adapter needs Redis (against the one-Postgres decision) and duplicates our `push.deliver` backplane; `core/push` stays transport-free so a Socket.IO adapter stays possible later. |
| A new `#[ws]` rpc attribute | would mark socket-invocable ops | Player-QUIC proves `#[http]` is the player allow-list; a second flag is a second authority (golden, routecheck, csharp, opscatalog derive from `route_bindings()`). Invoke reuses `find_by_method`, so the WS op set == the QUIC op set by construction. Payload size is NOT identical — see decision 7. |
| Player-QUIC plane | request/response for players | Not reachable from browsers and has no server→client stream. WS invoke reuses its handler, it does not replace it. |
| `push::Target::Group` | ephemeral per-process client-joined sets | Not permission-checked and front-local (`push_ws.rs:275-285`); a party is a privilege. Party fans out `Target::Player` per member (≤ 8). |
| `groups` fortress | durable social groups | Last admin cannot leave while members remain (`service.rs:596-617`) vs party handoff; no player promote (`admin.rs:12-17`); 30-day invites; no one-per-player rule; every change is a 30-day durable event into `audit`; a lease would edit a shipped fortress's DDL and every op. **Copy** its advisory-lock cap gate (`store.rs:59-85,171-181`), NotFound-not-Forbidden, last-member dissolution (`service.rs:635-678`). |
| `friends` | social graph, handle lookup | Party resolves handles through `accountsapi::Directory` directly; friends-only invites need a new friends capability — out of scope. |
| `accounts` presence (`online_until`) | newest session expiry | Session lifetime (≤ 60 min), not connection liveness. |
| `scheduler` for the lease sweep | durable `scheduler.fired` | A seconds-scale sweep would append a durable event every few seconds; the `mail` worker's module-owned `start()` loop is the precedent. |

### Design decisions

1. **Invoke frame.**
   `{"type":"invoke","id":<i64>,"method":"party.accept","payload":{...}}` →
   `{"type":"result","id":<same>,"response":{status,err,value?}}`; `response` is
   `handle_player`'s envelope verbatim (the QUIC shape). Missing `payload` = `{}`.
   Parsing: a `Kind { type }` probe first; `invoke` is parsed by a separate non-tagged
   `struct InvokeFrame { id: i64, method: String, #[serde(default)] payload:
   Option<Box<RawValue>> }`. `ClientFrame` stays `RawValue`-free.
2. **Per-invoke admission with the stored credentials.** Bearer + api key from the
   handshake are kept in `serve`; every invoke calls `handle_player(method,
   Some(bearer), Some(api_key), payload)`. API-key **policy** per method; fresh bearer
   check per call (no reverify-interval staleness). Cost: one `Sessions` lookup per
   invoke, as on HTTP/QUIC.
3. **Grammar errors on a live connection.** A frame whose `type` is `invoke` that fails
   `InvokeFrame` parsing, or whose `method` exceeds `MAX_INVOKE_METHOD_BYTES = 128`,
   closes with a NEW code `CloseCode::BadFrame` (`"bad_frame"`, non-retryable, reason
   "malformed invoke frame"). An inbound frame over `max_frame_bytes` closes with a NEW
   code `CloseCode::FrameTooLarge` (`"frame_too_large"`, **retryable** — reconnecting
   restores every hub stream; the client sends that request over HTTP/QUIC instead).
   Telling a size error apart needs `axum::Error::into_inner()` downcast to
   `tungstenite::Error::Capacity`, so `modules/gateway/Cargo.toml` gains a normal
   `tungstenite` dependency (same version as the existing dev-dependency's). `Protocol`
   keeps meaning "bad handshake". Other unknown `type`s keep today's ignore rule.
4. **Execution.** Each invoke runs in a `tokio::task::JoinSet` owned by `serve`, gated
   by a per-connection semaphore `PUSH_INVOKE_MAX_IN_FLIGHT` (default 16). The
   `join_next()` arm is guarded `if !join_set.is_empty()` and a `None` never breaks the
   loop. Each task is bounded by `PUSH_INVOKE_TIMEOUT_MS` (default 30000, a
   `PushLimits` field, never `HTTP_REQUEST_TIMEOUT_MS` read in the module); elapse →
   `Unavailable` "invoke timed out". The permit is released only after the result frame
   is written.
5. **Rate limits — charged before any parse, cheap lock before global lock.** Order
   per inbound frame, with no JSON work before step (i):
   (i) EVERY inbound frame is charged to a per-connection token bucket
   (`PUSH_INBOUND_RPS` 10 / `PUSH_INBOUND_BURST` 20, refilled by elapsed
   `tokio::time::Instant`, no shared lock); over budget → dropped unparsed, as today.
   (ii) `Kind` probe. (iii) `invoke` only: the shared per-IP `httpmw::IpLimiter`
   (`PUSH_INVOKE_IP_RPS` 20 / `PUSH_INVOKE_IP_BURST` 40, keyed by the IP `upgrade`
   resolves, `push_ws.rs:1065`) — its process-wide mutex is reached at most at the
   per-connection rate. (iv) `invoke` only: full `InvokeFrame` parse.
   Group verbs are additionally charged to `MAX_VERBS_PER_INTERVAL` exactly as today
   (its purpose — bounding registry-mutex acquisitions — is unchanged; invoke never
   takes that mutex). A frame dropped at (i) is silent even if it was an invoke; the
   client-visible bound is documented. `Gateway::start` spawns the limiter's
   `spawn_eviction_task()`; `Gateway::stop` aborts it.
6. **Rejection results are bounded.** An invoke refused at (iii) or by the in-flight
   semaphore gets an immediate `Unavailable` result ("rate limited" / "too many
   in-flight invokes"). Rejection entries are counted separately from permit-backed
   results; if `PUSH_INVOKE_MAX_IN_FLIGHT` rejection entries are unwritten, the next
   rejection closes the connection with `CloseCode::Capacity` (retryable). Reply-lane
   bound: 16 results + 16 rejections + the ack. A client that keeps reading and keeps
   flooding is capped by (i), not by this rule.
7. **Payload ceiling (not parity).** WS invoke requests are limited by
   `PUSH_MAX_FRAME_BYTES`; default rises from 32 KiB to 128 KiB. That is the ceiling
   on the ESCAPED frame: a JSON-text field such as `storage.put`'s `value` (a `String`,
   `api/storage/api/src/lib.rs:48,164`) grows 2× for quotes/backslashes and up to 6×
   under `\u` escaping, so a legal 64 KiB value may not fit, while HTTP and QUIC admit
   1 MiB (`modules/gateway/src/lib.rs:118`, `core/edge/src/player.rs:42-47`). The WS is
   not raised to 1 MiB because each connection also holds up to 16 in-flight payloads
   (10,000 connections × 16 × 1 MiB). Oversize closes `frame_too_large` (retryable,
   decision 3); push-hub.md states the ceiling and says large requests use HTTP/QUIC.
8. **Lanes with a total bound.** `ConnQueue` becomes: a **reply lane** (acks + results,
   never dropped, bounded by decision 6) and **event lanes** keyed by hub = topic
   prefix before the first `.` (no dot → `""`), at most `MAX_HUBS_PER_CONN = 16` lanes
   INCLUDING lane `""`, which always exists at index 0; frames of any hub past the
   15 named lanes share lane `""`. `PUSH_QUEUE_DEPTH` stays the TOTAL across event lanes
   (per-connection memory bound unchanged): at capacity the oldest frame of the
   LONGEST lane is evicted (ties → lowest lane index) and counted in `DROPS`.
   `recv` order: pending close → reply lane → event lanes round-robin.
9. **Hub label.** `ServerFrame::Message` gains `hub`, derived in the gateway from the
   topic, never carried on the backplane.
10. **Graceful stop does NOT drain invokes.** The existing shutdown path is kept as is:
    `close(Shutdown)` preempts every lane, `serve` writes the typed close inside
    `PUSH_STOP_GRACE` (whose "one small close frame" sizing, `push_ws.rs:54-56`, stays
    true) and returns, dropping the `JoinSet`. In-flight invokes are aborted and their
    results are never sent; a committed mutation's outcome is ambiguous to the client,
    which re-reads with `party.get` after reconnecting. Recorded divergence from the
    HTTP/QUIC drains; a drain sub-loop is a follow-up.
11. **Party state authority is Postgres** (schema `party`); real-time is a best-effort
    `party.state` snapshot pushed after commit. Ordering: `revision` comes from ONE
    global sequence `party.revision_seq` (`nextval` on every membership mutation, also
    stamped on the "you left" push). Client rule: a direct op RESPONSE always replaces
    the client's view and raises its high-water mark to `max(hw, revision)`; a PUSH is
    applied only if its `revision` > hw. A "no party" response (`get`/`heartbeat`/
    `leave`) carries the sequence's current value (`SELECT last_value FROM
    party.revision_seq`), which is ≥ every committed revision. No durable topics in v1.
12. **Liveness is a client-renewed lease.** Every party op by a member renews
    `members.lease_until = now() + PARTY_LEASE_SECS` (default 60, range 10..=600,
    unusable = startup failure); lease renewal never bumps `revision` and never pushes.
    A module-owned sweep removes expired members. Socket-driven leases are a follow-up.

## Steps

### Step 1 — Hub lanes + hub label in the push hub `[opus]` (core-implementer)

(a) **What:** `modules/gateway/src/push_ws.rs` — `ConnQueue`/`QueueState` (`:445-528`),
`HubState::deliver` (`:819-838`), `ServerFrame::Message` (`:304-319`), the ack enqueue
(`:1174`), new const `MAX_HUBS_PER_CONN = 16` beside `:238-266`, `fn hub_of(topic:
&str) -> &str`.
(b) **Why first:** Step 3 needs the reply lane; landing lanes alone keeps the queue
rewrite reviewable without invoke concurrency on top.
(c) **How:** replace `QueueState.ring` with `reply: VecDeque<Arc<str>>`, `events:
Vec<(Box<str>, VecDeque<Arc<str>>)>`, `event_len: usize`, `next: usize`, under the
existing `Mutex`; lane `""` is created at index 0 with the queue. `push(hub, frame)`:
find/create lane (≤ 16 total including `""`, else lane `""`); if
`event_len == depth`, pop the front of the longest lane (lowest index on tie),
increment `DROPS`; push; return false when closing (unchanged). `push_reply(frame)`
never drops. `recv`: close > reply > next non-empty event lane from `next` (advance).
`deliver` computes `hub_of` once and serializes `Message { hub, topic, payload }` once
(shared `Arc<str>` kept). Ack → `push_reply`. Single-slot mpsc wakeup unchanged.
(d) `[opus]` core-implementer, effort think hard.

### Step 2 — Tests for Step 1 `[test-author]` (model: sonnet)

Covers Step 1's commit in `modules/gateway/src/push_ws_tests.rs`. First rewrite
`queued()`/`queued_close()` (`:91-94`) over the lane structure in `recv` order (reply,
then events round-robin) and fix any existing test that counted the ack in the ring.
New tests:
- 200 frames on hub `a` with depth 64, then one `b` frame → `b` is still delivered
  (previously wrong: one shared ring evicted it) and total queued ≤ 64;
- eviction takes from the longest lane;
- the 16th distinct named hub lands in lane `""` (index 0), lane count never exceeds 16;
- close > reply > events; two non-empty lanes alternate;
- serialized `message` carries `hub` (`party.state` → `party`, `nodot` → `""`);
- `deliver_batch_replays_every_target_variant_in_order` (`:1126`) passes after the
  helper rewrite (backplane untouched).

### Step 3 — `invoke`/`result` frames on `/push` `[opus]` (core-implementer)

(a) **What:** `modules/gateway/Cargo.toml` — normal `tungstenite` dependency
(decision 3); `modules/gateway/src/push_ws.rs` — `InvokeFrame` + `Kind` structs,
`ServerFrame::Result { id: i64, response: &RawValue }`, `CloseCode::BadFrame` and
`CloseCode::FrameTooLarge` (+ `retryable()`/`ws_code()`/`reason()` arms), the reply
lane's entry type becomes `enum Reply { Frame(Arc<str>), Result(Arc<str>,
OwnedSemaphorePermit), Rejection(Arc<str>) }` (Step 1's `push_reply(Arc<str>)` becomes
`push_reply(Reply)`; the rejection count is derived from it), a production probe
`pub fn conformance_invoke_method_rejected()` beside `conformance_group_name_rejected`
(`:779-785`) for Step 8's `CapCase`, `PushLimits` (`:64-116`, env `:216-227`) fields
`invoke_max_in_flight`, `invoke_timeout`, `inbound_rps`, `inbound_burst`,
`invoke_ip_rps`, `invoke_ip_burst`, and `max_frame_bytes` default 128 KiB, all through
`PushLimits::from_values` (absent/blank → default, present-but-unusable or `0` →
startup failure); `PushHub` gains the `IpLimiter`; `run` (`:1144-1189`) passes
`creds.api_key`, `bearer`, client IP and the `FrontDoor` into `serve`;
`modules/gateway/src/lib.rs` `Gateway::start`/`stop` spawn/abort the limiter's
eviction task; new const `MAX_INVOKE_METHOD_BYTES = 128`.
(b) **Why now:** needs Step 1's reply lane; Step 5 proves it on existing ops; party
(Step 7) is invoked through it.
(c) **How:** in `serve`'s socket arm: an `Err` from `recv` that downcasts to
`tungstenite::Error::Capacity` → write `FrameTooLarge` close (other `Err`s keep
`break`). Then decision 5 in order: (i) inbound bucket, drop unparsed on miss;
(ii) `Kind` probe; non-`invoke` → unchanged `MAX_VERBS_PER_INTERVAL` charge +
`on_client_frame`; `invoke` → (iii) per-IP limiter → (iv) `InvokeFrame` parse /
decision 3 close → semaphore `try_acquire_owned` → decision 6 rejection path, else
`join_set.spawn(timeout(invoke_timeout, front.handle_player(...)))` whose output (or
the timeout `Unavailable` envelope via `front_envelope`) is pushed as
`Reply::Result(frame, permit)`; the writer drops the permit after the socket write.
Add a `join_next()` arm guarded by `!join_set.is_empty()` (logs panics only; `None`
never breaks). Shutdown path unchanged (decision 10).
(d) `[opus]` core-implementer, effort think hard.

### Step 4 — Tests for Step 3 `[test-author]` (model: sonnet)

Covers Step 3's commit (`push_ws_tests.rs`, gateway `lib.rs` tests):
- a well-formed invoke with an object payload is dispatched and the socket stays open
  (the RawValue-in-tagged-enum branch);
- the result envelope equals `handle_player`'s output for the same input;
- `characters.ownerOf` → `NotFound` (QUIC parity);
- key without the method in its policy → `Forbidden` (previously `PresenceOnly`);
- bearer revoked after bind → next invoke `Unauthorized`;
- 17 concurrent held invokes (counting fake backend on a `Notify`) → 17th gets
  in-flight `Unavailable`; releasing one admits a new one;
- 16 unwritten REJECTION entries → next rejection closes `capacity`; 16 unwritten
  permit-backed results plus one more admitted-then-rejected invoke do NOT close (the
  false-positive branch);
- inbound bucket exhaustion (paused tokio clock) → frame dropped with zero parse and
  zero backend calls (counting fake); per-IP limiter with rate/burst 1 (real
  `IpLimiter`, no paused clock) → second connection's invoke from the same IP gets
  `Unavailable` "rate limited", and the limiter is not consulted for a frame the
  inbound bucket dropped;
- invokes do not consume `MAX_VERBS_PER_INTERVAL` (65 invokes spread under the inbound
  bucket all get results) and group verbs still do;
- a held invoke past `invoke_timeout` (paused clock) → `Unavailable` "invoke timed out";
- idle connection with an empty `JoinSet` does not spin (the `select!` completes zero
  `join_next` iterations over a paused-clock interval);
- no-id invoke and over-128-byte method → `bad_frame` close (non-retryable); an
  over-cap frame → `frame_too_large` close (retryable); `{"type":"future_verb"}`
  ignored;
- shutdown with an in-flight invoke → the `shutdown` close is written and the invoke
  task is aborted (drop-guard observed), no result frame after the close;
- `from_values` rejects `PUSH_INBOUND_RPS=0`/garbage, keeps defaults on blank.
- (added after the Step 3 review, commit e171253) a Local op whose handler panics,
  invoked over `/push` → `result` with the same id, `Internal` "invoke failed", the
  permit returns (next invoke admitted), socket stays open;
- (e171253, in `core/httpmw` tests since `with_max_visitors` is crate-private) two
  limiters named `push_invoke` and `http` with a tiny visitor cap: saturating
  `push_invoke` raises only its own `limiter` series, the `http` series is unchanged;
- (e171253) `enqueue_ack` called with events already queued (and under an event
  flood at depth) → the ack is the first frame `recv` yields.

### Step 5 — Split-proof: invoke on existing ops `[opus]` (core-implementer)

(a) **What:** `tools/splitproof/src/pushws.rs` — `Frame::Message` gains `hub`,
new `Frame::Result { id, response }`, `PushClient::invoke(id, method, payload)`
awaiting the matching `result` while buffering other frames;
`tools/splitproof/src/main.rs` `push_assertions` (`:3435`) gains `[PH7{tag}]`–`[PH11{tag}]`
and the existing nudge assertion also checks `hub == "notifications"`.
(b) **Why now:** proves Steps 1–3 in split (Remote through gateway-svc) and monolith
before party exists.
(c) **How:** `[PH7]` registered player (existing register/login helper, not
`create_guest`) invokes `storage.put` then `storage.get` over `/push`; `[PH8]`
`dev-key-client` invoking `match.report` → `Forbidden`; `[PH9]` `characters.ownerOf`
→ `NotFound`; `[PH10]` no-id invoke → `bad_frame` close; `[PH11]` a nudge frame and an
invoke result interleave on one socket with correct `hub`/`id`. Split call before
`[RL1]` (`:5578`); monolith parity at `:548-570`.
(d) `[opus]` core-implementer ([[splitproof-is-proof-surface]]), effort think hard;
then `proof-auditor`.

### Step 6 — `party` contracts `[opus]` (core-implementer)

(a) **What:** `api/party/api` (`partyapi`), `api/party/rpc` (`partyrpc`,
`party_player_meta!(rpc_macro::generate_glue);`); root `Cargo.toml` members +
workspace deps (storage precedent `:138-139,158,192,376-377,400,427`). No
`partyevents` crate.
(b) **Why now:** everything downstream compiles against it.
(c) **How:** `#[rpc(prefix = "party")] trait Player`, every method
`#[http(verb = "POST", path = "/party/<name>", auth = "player")]`, first arg `Identity`:
- `create(id) -> Result<PartyView, Error>`
- `get(id) -> Result<PartyView, Error>` `#[retry_safe]` (no party → `party_id == ""`)
- `invite(id, target_handle: String) -> Result<InviteView, Error>`
- `invites(id) -> Result<InviteList, Error>` `#[retry_safe]`
- `accept(id, invite_id: String) -> Result<PartyView, Error>`
- `decline(id, invite_id: String) -> Result<(), Error>` (empty value)
- `leave(id) -> Result<PartyView, Error>` (returns the "no party" view)
- `kick(id, player_id: String) -> Result<PartyView, Error>`
- `promote(id, player_id: String) -> Result<PartyView, Error>`
- `set_ready(id, state: String) -> Result<PartyView, Error>`
- `heartbeat(id) -> Result<HeartbeatView, Error>` `#[retry_safe]`
Views (i64/String only): `PartyView { party_id, revision: i64, leader_id, members:
Vec<MemberView { player_id, handle, ready }> }`, `InviteView { invite_id, party_id,
inviter_handle, expires_at }`, `InviteList { invites: Vec<InviteView> }`,
`HeartbeatView { lease_until }` (RFC3339, `""` when not in a party). Consts:
`MAX_MEMBERS: i64 = 8`, `READY`/`NOT_READY`, `PUSH_STATE_TOPIC = "party.state"`
(payload `PartyView`), `PUSH_INVITE_TOPIC = "party.invite"` (payload `InviteView`),
`INVITE_TTL_SECS: i64 = 120`. Contract doc states: another player's party/invite is
`NotFound`, never `Forbidden`; malformed uuid or unknown `state` is `Invalid`;
`target_handle` over `accountsapi::MAX_HANDLE_BYTES` is `Invalid`.
(d) `[opus]` core-implementer, effort think.

### Step 7 — `party` module `[opus]` (core-implementer)

(a) **What:** `modules/party/src/{lib.rs,store.rs,service.rs,sweep.rs,conformance.rs}`.
(b) **Why now:** needs Step 6; Step 8 registers it.
(c) **How:**
- **Schema `party`** (idempotent DDL): `CREATE SEQUENCE party.revision_seq`;
  `parties(id uuid PK, leader_id uuid NOT NULL, revision bigint NOT NULL, created_at
  timestamptz)`; `members(player_id uuid PRIMARY KEY, party_id uuid NOT NULL, ready text
  NOT NULL CHECK (ready IN ('ready','not_ready')), joined_at timestamptz NOT NULL,
  lease_until timestamptz NOT NULL)` + indexes `(party_id)`, `(lease_until)`;
  `invites(id uuid PK, party_id uuid, inviter_id uuid, invitee_id uuid, expires_at
  timestamptz, UNIQUE (party_id, invitee_id))`. Ids bound `$1::uuid` like every other
  module; the `members` PK is the one-party-per-player rule.
- **Locking:** groups' namespaced FNV key → `pg_advisory_xact_lock` per party
  (`modules/groups/src/store.rs:59-85,171-181`), namespace `party.party/`. Every
  membership mutation takes it and sets `parties.revision = nextval(...)`. A `23505` on
  the `members` insert (two ops by one player under different party locks) maps to
  `Conflict`.
- **Rules:** `create` → Conflict if caller in `members`. `invite` → caller must be
  leader (else NotFound); self → Invalid; target already a member → Conflict; resolve
  via `Directory::find_by_handle` (unknown → NotFound; outage → Unavailable/503);
  upsert invite, `expires_at = now() + INVITE_TTL_SECS`. `accept` → invite must be the
  caller's and unexpired (else NotFound); Conflict if caller in a party or party at
  `MAX_MEMBERS` (counted under the lock); deletes the invite. `leave` → delete member;
  leader leaving hands leadership to the earliest `(joined_at, player_id)`; last member
  leaving deletes party + invites. `kick`/`promote` → leader only, target a member
  (else NotFound). `set_ready` validates `state`. `get`/`heartbeat` in no party →
  "no party" view / `lease_until = ""`, no error. Lease renewal (any op by a member,
  incl. `get`/`heartbeat`) never bumps `revision` and never pushes.
- **Response and push after commit:** after commit, load the snapshot, resolve handles
  with ONE `Directory::players_by_id` call; on failure the op STILL succeeds, handles
  are `""` in both the response and the push (the mutation is committed; failing it
  would make a retry answer `Conflict`). Push `party.state` to each current member,
  and to a removed member a "no party" view stamped with the mutation's `revision`;
  `invite` pushes `party.invite` to the invitee. `Push::send` errors → debug log.
- **Sweep** (`start()` loop, `mail` worker precedent; stop = grace then abort under the
  app's 5 s): every 5 s pick ≤ 64 `party_id`s with an expired member; per party take the
  lock, re-check, apply the `leave` rule to each expired member, commit, push; then
  `DELETE FROM party.invites WHERE expires_at < now()`. Replica-safe by re-check under
  the lock.
- **`PARTY_LEASE_SECS`** read in the module (retention-env precedent), 10..=600, unset
  = 60, unusable = startup failure.
- **Purge:** `on_tx(SubscriptionSpec { id: "party.player-deleted.v1", start:
  StartPosition::AfterRegistration }, …)` on `accountsevents::PLAYER_DELETED` applies
  the `leave` rule for that player and deletes invites to/from them in the delivery
  tx; no push from there. No `purged_players` tombstone: rows are lease-bound, the
  player's sessions are revoked in the accounts tx that emits `player.deleted`, the
  gateway verifier has no session cache, and `Directory` no longer resolves the handle.
- `requires()` = `vec!["accounts".into()]` (groups precedent `lib.rs:134-136`).
- **Conformance stances** (`conformance.rs`): `EnvValidation` Applies —
  `PARTY_LEASE_SECS` cases blank→60, `9`, `601`, `abc` fail; `InputByteCaps` Applies —
  `target_handle` capped at `accountsapi::MAX_HANDLE_BYTES` with fixture
  `conformance_target_handle_rejected`; `invite_id`/`player_id` and `state` are `Opaque {
  rationale }` (uuid parse / closed vocabulary; a failure answers `Invalid`; storage
  `acl` precedent `policy.rs:131`); `InfraOutage503` Applies —
  probe: failing `Directory` fake on `invite` → 503; `ArgonParity` NotApplicable (no
  password handling).
(d) `[opus]` core-implementer, effort think hard.

### Step 8 — Wiring `party-svc` into both topologies `[sonnet]`

(a) **What:**
- `cmd/party-svc/{Cargo.toml,src/lib.rs,src/main.rs}`: metrics + `PartyModule` +
  accounts `remote::Stub` (as `cmd/groups-svc`) + `remote::PushSender::new(wiring.
  peer_or("gateway","127.0.0.1:9013"))` (as `cmd/notifications-svc/src/lib.rs:19-27`).
- `cmd/server/{Cargo.toml,src/lib.rs:66}`.
- `cmd/gateway-svc` `describe_peer` (`src/lib.rs:148-160`), `addrs.rs:211-217`.
- `modules/apikeys/src/lib.rs` `DEV_CLIENT_POLICY` (`:105-116`) += `party.create,
  party.get,party.invite,party.invites,party.accept,party.decline,party.leave,
  party.kick,party.promote,party.setReady,party.heartbeat` (exact `METHOD_*`
  spellings; the seed is a self-healing upsert).
- `tools/checkmodules/{Cargo.toml,src/lib.rs:55}`.
- `tools/processctl/src/fleet.rs`: `service("party-svc", 8098, Some(9017),
  vec!["accounts-svc"])`, `peer(ACCOUNTS, 9003)`, `peer(GATEWAY, GATEWAY_EDGE_PORT)`
  (no gateway dependency, `:801-805` reasoning), gateway-svc's dependency and peer
  list += party-svc, `PARTY_LEASE_SECS=10` in the split-proof profile for party-svc
  AND in the monolith proof overlay. `MONOLITH_BASE_ENV`, `concat_env_16_10` and
  `MONOLITH_OVERRIDEABLE_ENV_ARR` (`:46-75`) are NOT changed, and party-svc gets no
  `overrideable_env` entry (`:890-915`): an operator override of `PARTY_LEASE_SECS` is
  wired in neither topology's dev tooling (parity kept; recorded gap).
- `weles/fleet.split.toml` (`:206-211,298`).
- `tools/opscatalog-gen/{Cargo.toml,src/main.rs:87}`;
  `tools/csharp-client-gen/{Cargo.toml,src/scrape.rs:38,56}` `PROVIDERS`;
  `tools/topiccheck/{Cargo.toml:65,src/golden.rs:245-249}` `rpc_modules()` +=
  `partyapi::player_rpc`; `tools/conformance/{Cargo.toml,src/policy.rs}` —
  `entries()` (`:52`), input policies (`:124-130`) for `target_handle`, `invite_id`,
  `player_id`, `state`, and the gateway entry's `CapCase` for
  `MAX_INVOKE_METHOD_BYTES` beside `gateway push group name` (`:645-651`);
  `tools/splitproof/Cargo.toml` (`partyapi`).
(b) **Why now:** needs Steps 6–7; Step 9's generators self-check against these lists.
(c) **How:** mechanical copy of groups-svc/storage-svc shapes; fleet-drift preflight,
`checkmodules` and the generators' own drift checks confirm completeness.
(d) `[sonnet]`, effort default; prompt carries the rollout pre-check.

### Step 8b — Fixture-list updates forced by drift checks `[test-author]` (model: sonnet)

`tools/processctl/src/fleet_tests.rs:123-127` (party-svc in the fleet snapshot, gateway
dependency list) and `weles/master/src/manifest_tests.rs:275,320`. Separate from Step 8
because tests are never the `[sonnet]` lane.

### Step 9 — Regenerate and bless contract artifacts `[sonnet]`

(a) **What:** `cargo run -p opscatalog-gen`; csharp-client-gen into
`clients/csharp/Generated`; `verifyctl --bless-contract-golden`,
`--bless-input-golden`, `--bless-public-api` (creates
`docs/reference/public-api-baseline/partyapi.txt`).
(b) **Why now:** after all contract/wiring changes, before any `verifyctl` run.
(c) **How:** one rollout at a time (`pgrep -x cargo; pgrep -x rustc` as its OWN call,
read before launching); after each bless, diff the baseline dir and restore files this
change did not cause ([[pgrep-check-must-gate-not-decorate]]).
(d) `[sonnet]`, effort default.

### Step 10 — Party tests `[test-author]` (model: sonnet)

Covers Steps 6–8 in `modules/party/src/tests.rs` (`testdb::test_pool`):
- concurrent `create` vs `accept` by one player → one succeeds, the other `Conflict`
  (not 500 — the 23505 branch);
- two concurrent accepts into a 7-member party → exactly one succeeds (advisory lock);
- leader-only invite/kick/promote → `NotFound` for a member; foreign/expired invite →
  `NotFound`; self-invite → `Invalid`; invite of a member → `Conflict`;
- leader leave → handoff to earliest `(joined_at, player_id)`; last leave → party and
  invites gone;
- leave X then accept into Y → the Y snapshot's `revision` > the "left X" revision;
- heartbeat/get never bump `revision` and never push (counting `push::Sink` fake);
- push only after commit (rolled-back mutation → fake sees nothing);
- failing `Directory` fake after commit → op succeeds with empty handles;
- sweep removes an expired member with leader handoff; two concurrent sweeps remove it
  once;
- `PARTY_LEASE_SECS` bounds; `player.deleted` delivery removes membership + invites;
- conformance fixtures from Step 7 execute their failing branch.

### Step 11 — Split-proof: party over the multiplexed socket `[opus]` (core-implementer)

(a) **What:** `tools/splitproof/src/party_checks.rs` + `party_assertions` wrapper in
`main.rs` (friends/groups shape, `[PY0{tag}]` completion), monolith call at `:548-570`,
split call before `[RL1]`, and a separate deletion assertion `[DL14{tag}]` (party is
NOT added to the `purged_players` lists at `:3050,3066`).
(b) **Why now:** needs the whole fleet (Step 8) and Step 5's client.
(c) **How:** players A, B (registered), one `/push` each. `[PY1]` A invokes
`party.create`. Before each of `[PY2]`–`[PY4]` both members invoke `party.heartbeat`
and the step asserts the returned `lease_until` is ≥ 5 s ahead, so the 10 s proof lease
cannot expire mid-scenario. `[PY2]` A invokes `party.invite(B's handle)` → B's socket gets
`hub: "party"`, `topic: "party.invite"`; `[PY3]` B invokes `party.accept` → both get
`party.state` with 2 members and equal `revision`; `[PY4]` B invokes `party.kick` on
A → `NotFound`; `[PY5]` B stops heartbeating, A heartbeats → A gets `party.state` with
1 member within `PARTY_LEASE_SECS` (10) + 5 s sweep, polled against a deadline;
`[PY6]` A invokes `party.leave` → party row gone (sqlx); `[PY7]` notifications nudge and
party frames interleave with distinct `hub`s. `[DL14]`: C creates a party, invites
D (D accepts) and invites E (pending, 120 s TTL); E leads its own party and invites C
(pending). C deletes the account. Assert (sqlx, deadline polling) that BOTH pending
invites involving C are gone well inside their 120 s TTL — the sweep cannot delete an
unexpired invite, so only `party.player-deleted.v1` passes this — and that C's
`members` row is gone and D became leader. The members half alone is not
discriminating (the 10 s lease sweep would also remove it); the invite half is.
(d) `[opus]` core-implementer, effort think hard; then `proof-auditor`.

### Step 12 — Docs `[docs]` (model: sonnet)

- `docs/reference/push-hub.md`: invoke/result grammar, `hub`, lanes + total-depth
  eviction, `bad_frame`/`frame_too_large`, env rows (`PUSH_INBOUND_*`,
  `PUSH_INVOKE_*`, new `PUSH_MAX_FRAME_BYTES` default), per-invoke admission cost, the
  escaped payload ceiling, the client revision rule (decision 11), invokes aborted on
  stop,
  carried gaps.
- `modules/gateway/src/push_ws.rs` doc comments made false by Steps 1 and 3:
  `ClientFrame` (`:268-271`), `on_client_frame` (`:1363-1368`),
  `MAX_VERBS_PER_INTERVAL` (`:240-251`, now group verbs only, behind the inbound
  bucket), `ConnQueue` (`:437-444`), `PushLimits::queue_depth` (`:72-73`, now the total
  across event lanes), `deliver` ("the ring holds a shared handle", `:815-818`),
  `LocalSink::send` ("whose ring later evicts", `:1001-1003`), the ack comment ("the
  one bounded queue", `:1172-1173`), and the `max_frame_bytes`/`PUSH_MAX_FRAME_BYTES`
  field doc (new default, escaped-bytes ceiling). The `PUSH_STOP_GRACE` rationale
  (`:54-56`) stays true (decision 10) and is not edited.
- `CLAUDE.md`: "Domain modules (18 fortresses + gateway)" with a `party` entry;
  split-proof port list `party :8098/:9017`; the `/push` sentence under gateway.
- `docs/roadmap/feature-tracker.md`: Parties row, push hub row, change log.
- `docs/reference/module-reference.md` "Sending a push notification": the
  post-commit snapshot + global `revision` pattern.
- Errata section in THIS plan: (1) `http_rate_limit_table_saturated_total` gained a
  `limiter` label (`http`, `admin_login`, `push_invoke`) in e171253, reversing the
  single global counter decided in
  `docs/plans/2026-07-12-1214-architecture-remediation-rust-tooling-plan.md:254`
  (archive, not edited); a process that builds no `IpLimiter` no longer exports the
  family; the `http_` prefix is kept although `push_invoke` limits WebSocket frames.
  (2) decision 4's "the `join_next` arm logs panics only" is superseded: a panicking
  invoke is answered with `Internal` "invoke failed" inside the task (e171253).
  (3) Step 2's flood test ordering (`b` must be queued BEFORE the `a` flood) per the
  Step 2 proof audit.

## Reviews

Every implementation step gets one `core-reviewer` pass before the next step; Steps 5
and 11 add `proof-auditor`. Commit after each step, staging by explicit path while
agents run. Precondition for the rollout: git works on this machine (Xcode license
accepted).

## Known gaps carried forward (recorded, not scheduled)

- No fuzz target covers the `/push` frame parser (`stages/fuzz.rs:24`).
- Party disconnect is detected by lease TTL (≤ `PARTY_LEASE_SECS` + 5 s), not socket
  close; socket-driven leases (also the path to socket presence) are a follow-up.
- No `Target::Players(Vec)` — fine for 8-member parties; large chat/lobby audiences
  need it, and it changes the backplane `Envelope` (fronts-before-producers rollout).
- No invoke resume/replay: a result lost to a dropped socket is ambiguous, as on QUIC.
- WS invokes are aborted, not drained, on graceful stop (vs 5 s drains on HTTP/QUIC).
- WS payload ceiling is 128 KiB escaped vs 1 MiB on HTTP/QUIC.
- `PARTY_LEASE_SECS` has no devctl operator-override entry in either topology.
- No admin page for parties (not a conformance convention; ephemeral lease-bound state).
- `/push` still has no `routecheck` coverage of its fixed route.

## Errata (recorded with the Step 12 push-protocol docs)

1. `http_rate_limit_table_saturated_total` gained a `limiter` label (`http`,
   `admin_login`, `push_invoke`) in e171253, reversing the single global counter decided
   in `docs/plans/2026-07-12-1214-architecture-remediation-rust-tooling-plan.md:254`
   (archive, not edited). A process that builds no `IpLimiter` no longer exports the
   family. The `http_` prefix is kept although `push_invoke` limits WebSocket frames.
2. Decision 4's "the `join_next` arm logs panics only" is superseded: a panicking
   invoke is answered with `Internal` "invoke failed" inside the task (e171253); the
   `join_next` arm still logs a task panic that escapes that guard.
3. Step 2's flood test ordering: `b` must be queued BEFORE the `a` flood (Step 2 proof
   audit).
4. Step 4's "idle connection ... zero `join_next` iterations" test proved unbuildable:
   `select!` disables a branch whose pattern does not match the `None` an empty set
   yields, so the `!is_empty()` guard is unobservable. The landed test pins the
   neighbouring regression (the arm must not end the loop) and records the limit in its
   doc comment (`push_ws_tests.rs`).

## Review round 1 — disposition

| # | Finding | Resolution |
|---|---|---|
| 1 | verbs budget silently drops invokes | Decision 5: invokes exempt, bucket is their only limiter; verbs keeps non-invoke frames |
| 2 | `join_next` hot loop | Decision 4 / Step 3 guard; Step 4 test |
| 3 | RawValue in tagged enum | Decision 1 separate `InvokeFrame`; Step 4 test |
| 4 | `DEV_CLIENT_POLICY` | Step 8 |
| 5 | topiccheck `rpc_modules` + Cargo deps | Step 8 |
| 6 | purge list vs no tombstone | Step 11 `[DL14]`, not the lists |
| 7 | per-party revision | Decision 11 global sequence; Step 10 test |
| 8 | rejection results unbounded | Decision 6 |
| 9 | 16× memory | Decision 8 total depth |
| 10 | IpLimiter eviction | Decision 5 / Step 3; Step 4 real-limiter test |
| 11 | no drain on stop | Decision 10 + known gap |
| 12 | frame cap vs storage.put | Decision 7, `bad_frame` |
| 13 | no invoke deadline | Decision 4 `PUSH_INVOKE_TIMEOUT_MS` |
| 14 | signatures / post-commit Directory failure | Step 6 signatures, Step 7 rule, Step 10 test |
| 15 | 23505 → 500 | Step 7 mapping, Step 10 race test |
| 16 | conformance stances | Step 7 stances; admin-page claim removed |
| 17 | test helpers read `ring` | Step 2 |
| 18 | splitproof can't see `hub` | Step 5 |
| 19 | monolith lease env | Step 8 |
| 20 | Protocol reason / false docs | Decision 3 `BadFrame`; Step 12 names the comments |
| 21 | test files in `[sonnet]` | Step 8b |
| 22 | unspecified rules | Step 7 rules |
| 23 | requires / visibility | Step 7; facts section |
| 24 | CLAUDE.md count | Step 12 |

## Review round 2 — disposition

| # | Finding | Resolution |
|---|---|---|
| 1 | drain impossible behind close preemption | Decision 10: no drain, existing close path kept; Step 4 test; known gap |
| 2 | global mutex / parse before charge / unbudgeted rejections | Decision 5 order (inbound bucket before parse, IP limiter after); decision 6 |
| 3 | 128 KiB false parity claim | Decision 7 escaped ceiling stated; `frame_too_large` retryable; known gap |
| 4 | capacity false positive | Decision 6 counts rejections only; Step 4 test |
| 5 | DL14 blind | Step 11 unexpired invites discriminate |
| 6 | no-party revision undefined | Decision 11 client rule + `last_value` stamping |
| 7 | false prose from Step 1 | Step 12 list |
| 8 | Validated vs Opaque | Step 7 `Opaque` |
| 9 | tungstenite dep, probe fn, Reply type, lane count | Decision 3, Step 3, decision 8, Step 1/2 |
| 10 | MONOLITH_BASE_ENV edit | Step 8: proof overlays only, arrays untouched; known gap |
| 11 | 10 s lease mid-scenario | Step 11 heartbeat + lease assertion before PY2–PY4 |
