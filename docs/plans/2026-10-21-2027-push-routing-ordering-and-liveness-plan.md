# Push: split ordering, socket liveness, and multiplexed-routing proof — plan

## Context

### Goal

Close what the 2026-10-08 audit of `/push` found. That audit used three read-only agents to
compare the push-hub plan (2026-09-03), the multiplex+party plan (2026-09-28) and
`docs/reference/push-hub.md` against the code and the proofs. Every finding was re-checked during
this plan's research (2026-10-21) and again by the plan review (round 1, disposition below).

**Code that does not do what the spec says**

- **A. Split ordering can differ from the monolith.**
  - Constraint 7 of the push-hub plan says ordering does not depend on topology. Four comments
    repeat that claim:
    - `core/push/src/lib.rs:117-121`
    - `core/remote/src/push_backplane.rs:13-16`
    - `core/remote/src/push_backplane.rs:373-375`
    - `modules/gateway/src/push_ws.rs:1241-1246`
  - The interleaving that breaks it:
    1. `Pool::deliver_all` (`core/remote/src/lib.rs:1090`) bounds each call with
       `FANOUT_CALL_TIMEOUT` (8 s, `:663`) or `FANOUT_UNHEALTHY_TIMEOUT` (2 s, `:672`).
    2. The edge client has already called `finish()` on the stream
       (`core/edge/src/client.rs:127-131`), so dropping the future does not cancel the peer's
       handler.
    3. The drain sends batch N+1 on a new stream over the same cached connection
       (`push_backplane.rs:406-416`).
    4. The edge server runs each stream in its own task (`core/edge/src/server.rs:592`).
    5. A batch N whose packets were delayed can therefore run `deliver_batch`
       (`push_ws.rs:1247`) after N+1.
  - Without a timeout there is no reordering. N+1 is not opened until N's response returns, and
    that response is written after N was enqueued.
- **B. Nothing checks for a pong.**
  - The re-verify tick writes `Ping` (`push_ws.rs:1552`), but an inbound `Pong` falls into
    `Some(Ok(_)) => {}` (`:1541`). axum 0.7.9 does hand `Message::Pong` to the handler.
  - A half-open peer with an idle queue is never detected, because a write fails only once the
    kernel buffer is full.

**False prose (C)**

- `push_ws.rs:11-14` and `:416`: the module header says refusals arrive as typed closes, and
  `CloseCode::Capacity` is documented as an aggregate-cap refusal. In the code, the aggregate caps
  answer HTTP 503 before the upgrade (`:1277-1279`). `capacity` is sent only when unwritten invoke
  refusals pile up (`:1728`).
- `push_ws.rs:592-593`: `Delivered::Local(n)` is said to count connections the frame "actually
  reached". It counts connections the frame was enqueued to.
- `push_ws.rs:1539-1540`: the tick ping is called a "liveness ping".
- `CloseCode::Unauthorized` is documented as "at bind time" only (`:405`). It is also sent on
  re-verify when the token now resolves to a different player (`:1561-1563`).
- `push-hub.md`, "Carried gaps": says a future op at `/push` would shadow the route. The real
  `GET /push` route sits in front of the fallback and hides the op (gateway `lib.rs:791-806`).
- "SignalR-shaped" in `push-hub.md:6` and in the CLAUDE.md gateway bullet. A hub is only a
  topic-prefix label plus a lane: there is no per-hub subscription, authorization or endpoint.
- `push-hub.md` says the first frame "must be" hello. With both credential headers present, no
  hello is required (`push_ws.rs:1417-1422`).

**Undocumented (D)**

- The producer backplane queue is one `mpsc(1024)` shared by every hub and topic of a process,
  and it drops the newest message when full (`push_backplane.rs:38-42`, `:114-129`). The
  per-socket queue does the opposite and evicts the oldest.
- `push.deliver` has no authorization beyond mTLS (plain `server.handle`, `push_ws.rs:1237`).
- Also undocumented:
  - the 32 KiB outbound payload cap (`:320`);
  - 32 groups per connection, with 128-byte names (`:283-284`);
  - the 64-verb budget (`:302`);
  - the bound connection receiving its own `online` frame.

**Proof gaps (E)**

- **E1.** No socket dispatches invokes to two different modules. [PH7]'s `match.report` and
  `characters.ownerOf` are refused at the front, before any dispatch happens.
- **E2.** No socket ever pipelines Remote invokes. The harness waits for each result
  (`tools/splitproof/src/pushws.rs:243`), and `invoke` never scans `buffered` for its own id.
- **E3.** Closed by existing tests, so not scheduled. The audit called lane isolation
  "unit-only", but:
  - the `deliver_batch` bytes → lanes ingress is already pinned by
    `deliver_batch_keeps_each_hubs_order_across_a_mixed_hub_batch` (`push_ws_tests.rs:1490-1519`);
  - `deliver_batch` reaches `ConnQueue::push` through `hub.deliver` (`push_ws.rs:1247-1252`,
    `:594`);
  - eviction across lanes is pinned at `ConnQueue` (`push_ws_tests.rs:698`ff).
- **E4.** Revoking a session under a live socket is proven only against a fake `Sessions`
  (`push_ws_tests.rs:2563`).
- **E5.** A Remote invoke failure arriving as a `/push` result is proven only with Local ops.
  Releasing the permit does not depend on topology (`push_ws.rs:1557-1564`), and that part is
  already proven by `:2594` and `:2644`.

### Research method

- Three parallel opus research agents covered API surface, usages and patterns; the results are
  synthesized here.
- They read the code by hand and checked behaviour in the vendored quinn 0.11.11 and axum 0.7.9
  sources. Grep was used only to locate symbols, and its counts are lower bounds.
- The 2026-10-08 audits are an input: two spec-vs-code passes and one proof-auditor pass.

### Overlapping systems: why not extend or depend on them

- **Durable bus or invalidation plane for push fan-out.** Already rejected
  (`2026-09-03-2022…:27-29`):
  - durable delivery is at-least-once with a checkpoint, which means nothing to a socket that may
    be gone;
  - invalidation is a broadcast with no payload;
  - `cmd/gateway-svc` runs `.without_db()`.

  The fix keeps the backplane.
- **Edge calls that cancel the remote handler when the client times out.** Rejected.
  - Nothing in the edge supports it.
  - The request has already been written and finished before the reply is awaited, so a cancel
    can lose the race with dispatch.
  - It would change `core/edge` for every RPC and still would not order N against N+1.
- **Serialize per peer.** Rejected: after a timeout the client cannot know whether N was
  dispatched.
- **Reorder buffer at the gateway.** Rejected. N may never arrive (a write truncated by the drop
  is never dispatched), so a buffer would need a hold timeout, which means a new clock and stalled
  delivery.
- **Drop stale batches by a per-producer sequence.** Chosen.
  - Precedents: the resolve generation in `core/remote/src/lib.rs:824-833` and `:891-900`
    (claim a sequence, apply it only if newer, under the lock), and the invalidation contract's
    "apply-only-newer" rule (`core/invalidation/src/lib.rs:140-149`).
  - A late batch is one `deliver_all` already counted as not delivered, so dropping it adds no
    new class of loss.
  - Push is best-effort, and `party` already makes `get` the authority.
- **Pong handling.** Extend the existing re-verify tick, which already writes the ping. No new
  timer, task or select arm.

### Design decisions

1. **The batch wire shape gets a mandatory header, and the bare array dies.**
   - `core/push` gains `pub struct Batch { pub producer: u64, pub seq: u64, pub envelopes: Vec<Envelope> }`
     with `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]` and
     `#[serde(deny_unknown_fields)]`. `Envelope` is unchanged.
   - `encode_batch` becomes `encode_batch(producer: u64, seq: u64, envelopes: &[Envelope])`.
     `decode_batch` returns a `Batch`.
   - There is no optional or ungated path. Binaries ship together and the wipe-era rule bans
     version-skew bridges (`.agents/shared/core-rules.md:97-104`). Every sender stamps, including
     the split-proof injector.
   - `pub const BATCH_HEADER_MAX: usize` equals `encode_batch(u64::MAX, u64::MAX, &[])` length, so
     the header overhead is a constant. A test pins the const to that encoded length.
2. **The drain stamps per chunk, and the budget measures the stamped form.**
   - `PushSender` starts exactly one drain (`push_backplane.rs:227-241`). Production has one
     sender per producing process (`cmd/party-svc/src/lib.rs:26`,
     `cmd/notifications-svc/src/lib.rs:23`).
   - The drain owns `producer: u64`, drawn once with `rand::random::<u64>()`. Add
     `rand = { workspace = true }` to `core/remote/Cargo.toml`.
   - A pid- or time-derived id is rejected: after a restart it could collide with the old one, and
     the new seq (restarting at 1) would sit under the old high-water mark until eviction.
   - The drain also owns `next_seq: u64`, starting at 1. In `send_batch` (`:381`), each chunk from
     `split_batch` takes `seq = next_seq; next_seq += 1` and is encoded with
     `encode_batch(producer, seq, chunk)`.
   - `split_batch` (`:425-461`) packs envelopes against `BATCH_BYTE_BUDGET - push::BATCH_HEADER_MAX`.
     Its envelope measurement (envelope bytes plus separators) is unchanged, because the header is
     the only stamped addition. The doc claim "the budget cannot drift from the codec"
     (`:428-431`) then stays true.
3. **The gateway gates on a per-producer lock held across enqueue.**
   - `PushHub` gains
     `origins: Mutex<HashMap<u64, Arc<Mutex<Origin>>>>`, with
     `struct Origin { hw: u64, last_seen: tokio::time::Instant }`. It uses
     `tokio::time::Instant` so a paused test clock drives eviction.
   - `deliver_batch`:
     1. decode the `Batch`;
     2. briefly lock the map to get or insert the producer's `Arc`, then release it (eviction runs
        here, only when inserting a new producer);
     3. lock the producer entry;
     4. if `batch.seq <= hw`, drop the whole batch, record it on a new
        `static STALE: Drops` ("stale backplane batch (late after a newer one)") and return
        `Ok(Vec::new())`;
     5. otherwise set `hw = batch.seq` and `last_seen = now`, then call `hub.deliver` for each
        envelope **while still holding the entry lock**.
   - Splitting check and enqueue into two critical sections would let N+1 enqueue between N's
     check and N's enqueue, which is the inversion this fix removes.
   - `<=` is correct, because one batch carries one seq and calls are `RetryMode::Never`, so the
     same seq never arrives twice legitimately.
   - **Lock order:** map → entry → `HubState` → `ConnQueue.state`. The map lock is never held
     while an entry lock is taken. `hub.deliver` releases `HubState` before `queue.push`
     (`push_ws.rs:1037-1043`), and nothing under the hub or queue locks calls `deliver_batch`.
     Poison is recovered with `unwrap_or_else(|e| e.into_inner())`.
   - Producers do not serialize each other.
   - **Eviction:** on inserting a new producer, remove entries whose `last_seen` is older than
     `ORIGIN_IDLE_EVICT = 300 s`. Why that is safe:
     - QUIC opening stream N+1 implicitly opens every lower stream, so N is accepted no later
       than N+1 arrives.
     - N's request read is then bounded by `EDGE_STREAM_GRACE` (30 s, `core/edge/src/server.rs:43`).
     - So a late N must dispatch within about 30 s of the producer's last newer batch, far inside
       300 s.
     - Do **not** argue from the edge idle timeout: the client's 5 s keepalive resets it while the
       transport is up (`server.rs:36-39`).
4. **Liveness: any inbound frame counts as a heartbeat.**
   - `PushLimits` gains `liveness_timeout: Duration`, from `PUSH_LIVENESS_TIMEOUT_MS`.
   - **Unset:** the default is 1.5 × the effective `reverify_interval`, so an operator who raises
     only `PUSH_REVERIFY_INTERVAL_MS` does not start failing startup.
   - **Set:** parsed by the existing `ms()` helper, and `from_values` fails startup when the value
     is `<= reverify_interval`. The ping is sent only on the tick, so such a value would close
     every idle healthy client.
   - The invariant is enforced by `from_values` (the environment path). `PushLimits` fields are
     public, and limits built as struct literals and passed to `Gateway::with_push_limits` are the
     caller's responsibility. Step 8 documents that on `PushLimits`.
   - `serve` keeps `last_inbound: Instant`, set at serve start and refreshed on every
     `Some(Ok(_))` from `socket.recv()`, including `Pong`.
   - On the tick, before writing the ping: if `last_inbound.elapsed() > liveness_timeout`, write a
     typed close with the new `CloseCode::LivenessTimeout` and end through the existing close
     path. The close has wire string `"liveness_timeout"`, retryable `true`, ws code `1000`
     (the same as `SessionExpired`; `1001` stays `Shutdown`'s).
   - The typed close is advisory: a truly half-open peer never receives it. It informs a live
     peer that is not reading.
   - With the defaults (60 s interval, 90 s timeout), a dead peer is dropped within two ticks
     (≤ 120 s).
   - No C# client or conformance artifact enumerates close codes, so the variant is additive.
   - **This is a semantic change, recorded here and in the Step 3 commit message:** an explicit
     `PUSH_LIVENESS_TIMEOUT_MS` at or below the interval fails startup.
5. **E4 uses account self-delete as the revocation.**
   - There is no logout op. Self-delete revokes every session in one transaction, behaves the same
     in both topologies, and works on a fresh player.
   - The invoke path's session check is uncached (`modules/gateway/src/verifier.rs:105-120`,
     `lib.rs:856-1013`), so the next invoke answers `Unauthorized` without waiting for the 60 s
     tick.
6. **E5 is scoped to "a Remote failure arrives as an `Unavailable` result and the socket keeps
   serving".** Permit release is topology-independent and already proven, so no permit-counting
   proof is added in split.

### What dies

- The bare-array batch encoding: `encode_batch(&[Envelope])` and `decode_batch -> Vec<Envelope>`.
- The four false ordering comments (Step 8).
- The "liveness ping" comment (Step 8).
- The audit's E3 "unit-only" wording: corrected above, with no test added.

## Steps

### Step 1 — Batch header and the stale-batch gate `[opus]` (core-implementer)

**(a) What:**
- `core/push/src/lib.rs`: `Batch`, `BATCH_HEADER_MAX`, the new `encode_batch` and `decode_batch`
  signatures (decision 1).
- `core/remote/Cargo.toml`: add `rand`.
- `core/remote/src/push_backplane.rs`:
  - drain state `producer` and `next_seq`;
  - `send_batch` stamping;
  - the `split_batch` budget minus `BATCH_HEADER_MAX` (decision 2).
- `modules/gateway/src/push_ws.rs`:
  - `PushHub.origins`, `Origin`, `ORIGIN_IDLE_EVICT`, `static STALE: Drops`;
  - the `deliver_batch` rewrite (decision 3).
- `tools/splitproof/src/pushws.rs:296`: `deliver_batch` takes `producer: u64, seq: u64`.
- `tools/splitproof/src/main.rs`: the [PH4] call sites (`:3662`, `:3675`) pass a random producer
  and increasing seqs.
- Existing tests that call `encode_batch` or `decode_batch` are updated to the new signatures,
  and only that (`core/push/src/tests.rs:182`, `push_backplane_tests.rs`, `push_ws_tests.rs`).

**(b) Why first:** this is the only cross-process wire change. Steps 2 and 7 build on it.

**(c) How:**
- `deliver_batch` stays synchronous and returns `Ok(Vec::new())` on a stale drop. The sender
  treats the reply as fire-and-forget, so a stale drop is not an error.
- Do **not** edit doc-comment prose beyond what a signature change forces to compile. Comment
  truth is Step 8's job.
- No `cargo fmt`; hand-format to house style.
- Compile check: `cargo build -p push -p remote -p gateway -p splitproof` and
  `cargo test -p push -p remote --no-run`, after the rollout check
  (`pgrep -x cargo; pgrep -x rustc` as its own call, then require it empty).

**(d)** `[opus]` core-implementer, `model:"opus"`, effort think hard. Commit
`feat(push,remote,gateway,splitproof): stamp backplane batches and drop late ones per producer`,
with the trailer `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`. Then one
core-reviewer pass, `model:"opus"`.

### Step 2 — Tests for Step 1 `[test-author]` (model: sonnet)

**(a) What:** `core/push/src/tests.rs`, `core/remote/src/push_backplane_tests.rs`,
`modules/gateway/src/push_ws_tests.rs`.

**(b) Why now:** Step 1 has landed and compiles. These tests pin its branches before the
split-proof relies on them.

**(c) Tests**, each executing a branch that was previously wrong or newly added:

- `core/push`:
  - round trip of a `Batch`;
  - a bare JSON array is rejected by `decode_batch` (the old shape is dead);
  - a header with an unknown extra key is rejected (`deny_unknown_fields`);
  - `BATCH_HEADER_MAX == encode_batch(u64::MAX, u64::MAX, &[]).len()`.
- `push_backplane_tests`:
  - a batch large enough for `split_batch` to cut into ≥ 2 chunks, captured from the drain's
    actual encoded calls: `producer` is constant, `seq` is strictly increasing across chunks and
    across consecutive drains, and **every encoded chunk is `<= BATCH_BYTE_BUDGET`**. That last
    assertion is the budget-drift guard; the existing boundary tests call `split_batch` directly
    and cannot see stamped bytes.
- `push_ws_tests`, driving `deliver_batch` with encoded bytes as the edge handler does:
  - seq 2 then seq 1 from one producer: the seq-1 frame is **not** in the connection queue. The
    primary assertion is the queue contents; `STALE`'s total may only be checked as `>=` a
    pre-read value, because it is process-global and tests run in parallel. Before Step 1 both
    frames were enqueued.
  - the same seq twice: the second is dropped;
  - two producers keep independent high-water marks;
  - eviction, under `#[tokio::test(start_paused = true)]` with `tokio::time::advance` past
    `ORIGIN_IDLE_EVICT`: an idle producer's entry is removed when a new producer arrives, and
    after that the idle producer's low seq is accepted again (proving the entry is gone);
  - a producer that is not idle is never evicted.

**At-risk topology:** split, the only path where `deliver_batch` sees backplane bytes. These tests
drive exactly those bytes.

**(d)** `test-author`, `model:"sonnet"`. Commit `test(push,remote,gateway): …`, with the trailer
`Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>`. Then one core-reviewer pass.

### Step 3 — Liveness timeout on `/push` `[opus]` (core-implementer)

**(a) What:** `modules/gateway/src/push_ws.rs`:
- `PushLimits.liveness_timeout`;
- `PUSH_LIVENESS_TIMEOUT_MS` (a `pub const` beside `:253-272`);
- `from_values` with decision 4's unset/set rules;
- `CloseCode::LivenessTimeout` across `:404-468`;
- `last_inbound` in `serve` (`:1481`), and the check in the tick arm (`:1550`).

**(b) Why after Step 1:** the two steps are independent but both edit `push_ws.rs`, so they are
serialized to avoid a merge between two subagents.

**(c) How:**
- No change to the mains: both forward every name through `from_values`
  (`cmd/server/src/main.rs:52`, `cmd/gateway-svc/src/main.rs:72`).
- Order in the tick arm: liveness check, then ping, then `reverify_push`, so a dead peer does not
  cost a re-verify round trip.
- No comment prose edits; those are Step 8. No `cargo fmt`.

**(d)** `[opus]` core-implementer, `model:"opus"`, effort think. Commit
`feat(gateway): close /push sockets silent past PUSH_LIVENESS_TIMEOUT_MS`. The body must name the
semantic change: an explicit value at or below `PUSH_REVERIFY_INTERVAL_MS` now fails startup.
Opus trailer. Then one core-reviewer pass.

### Step 4 — Tests for Step 3 `[test-author]` (model: sonnet)

**(a) What:** `modules/gateway/src/push_ws_tests.rs`. Config tests go in the value-taking block at
`:379-440`. Serve tests use the `front_over` fixture (`:2293`).

**(b) Why now:** Step 3 has landed.

**(c) Tests:**
- config:
  - unset timeout gives 1.5 × the default interval;
  - unset timeout with the interval set to 120 000 gives 180 s, and startup succeeds;
  - an explicit value `<=` the interval is rejected;
  - `0` and garbage are rejected.
- a client that binds and then **never reads** (so it never pongs), with `reverify_interval` 1 s
  and `liveness_timeout` 1.5 s: it is closed with `liveness_timeout`, retryable. Read the close
  with a 10 s hang guard. Before Step 3 the socket stayed open.
- a client that keeps reading (tungstenite auto-pongs), with interval 1 s and timeout 2.5 s:
  stays open across ≥ 3 ticks. The headroom is deliberate (timing doctrine).

**At-risk topology:** none. Liveness is front-local, and the same code runs in gateway-svc and in
the monolith.

**(d)** `test-author`, `model:"sonnet"`. Sonnet trailer. Then one core-reviewer pass.

### Step 5 — Pipelined invokes in the harness client `[opus]` (core-implementer)

**(a) What:** `tools/splitproof/src/pushws.rs`:
- `pub async fn send_invoke(&mut self, id: i64, method: &str, payload: serde_json::Value) -> Result<()>`
  only sends. Ids are `i64`, matching `Frame::Result` (`:36`) and `as_i64` (`:92`).
- `pub async fn await_result(&mut self, id: i64, budget: Duration) -> Result<serde_json::Value>`:
  1. If `buffered` holds a `Frame::Close`, fail at once without reading the socket.
  2. Otherwise remove a matching `Frame::Result { id, .. }` from `buffered`.
  3. Otherwise read the socket, buffering every frame that does not match. A `Close` is buffered
     and then the call fails (as at `:263-271`).
- `invoke` (`:243`) becomes `send_invoke` followed by `await_result`.

`party_checks.rs`'s `Sock::call` already goes through `invoke` (`:45-49`), so it needs no change.

**(b) Why before Step 7:** [PH12] needs pipelining.

**(c) How:** `next_frame`'s buffer-first rule stays the single source of read order.

**(d)** `[opus]` core-implementer (harness code is proof surface), `model:"opus"`. Commit
`feat(splitproof): pipelined invoke and buffered result lookup in the /push client`. Opus trailer.
Then one core-reviewer pass.

### Step 6 — Harness client tests `[test-author]` (model: sonnet)

**(a) What:** `tools/splitproof/src/pushws_tests.rs`, extending the scripted-server pattern at
`:50`.

**(b) Why now:** Step 5 has landed, and Step 7 relies on `await_result`'s buffer lookup.

**(c) Tests:**
- out-of-order results: the server sends `result{id:2}` then `result{id:1}`. `await_result(1)`
  returns 1's result, then `await_result(2)` returns 2's from the buffer. The script ends the
  server after those two frames, so a socket read would fail; the second call can only succeed
  from the buffer. The old `invoke` never scanned `buffered`.
- a buffered `Close` makes the next `await_result` fail without a read.

**(d)** `test-author`, `model:"sonnet"`. Sonnet trailer. Then one core-reviewer pass.

### Step 7 — Split-proof assertions [PH12]–[PH15] `[opus]` (core-implementer) + proof-auditor

**(a) What:** `tools/splitproof/src/main.rs`.

**[PH12] Multi-svc pipelined invoke (E1, E2).** Goes in `push_invoke_assertions` (the `:3839`
region), which runs in both topologies, so `[PH12m]` comes through `monolith_parity` (`:561`).
1. Register a fresh player and open one socket.
2. `send_invoke` five calls without awaiting:
   - id 1: `accounts.me {}`
   - id 2: `storage.put {collection, key, value, expected_version:0, acl:""}`
   - id 3: `party.create {}`
   - id 4: `characters.create {name, class:""}`
   - id 5: `groups.create {name, join_policy:"open"}` (`groupsapi::JOIN_OPEN`,
     `api/groups/api/src/lib.rs:58`)
3. `await_result` them in reverse order (5 → 1).
4. Check each answer against its owner, DB-verified:
   - `me.player_id` equals the registered id;
   - `storage.objects.owner_id` equals that id;
   - `party.parties.leader_id` equals that id;
   - `characters.characters.player_id` equals that id;
   - `groups.memberships` has a row for that id with `role = 'admin'`.

Names and keys use the `suffix` already passed into `push_invoke_assertions` (`:3619`). The claim
is "pipelined and correlated across five owning processes", not "concurrently in flight". Five
frames sit inside the 20-frame inbound burst.

**[PH13] Session revoked under a live socket (E4).** Also in `push_invoke_assertions`, both
topologies.
1. Fresh player, socket bound, `accounts.me` returns Ok.
2. Over HTTP with that bearer and `dev-key-client`: `POST /accounts/delete/begin`, then
   `POST /accounts/delete {"ticket": …}`.
3. The next invoke, `accounts.me`, returns a result with `status:"Unauthorized"` (not a close).

**[PH14] Remote failure arrives as a result (E5).** Split only, inside `rdy_dead` (`:6062`). Its
signature gains the pool, the gateway ws base and the `dev-key-client` key it needs.
1. Place it after the 503-flip poll (`:6118-6146`) and before the respawn (`:6158`).
2. On a socket opened to gateway-svc, `characters.list {}` must return
   `status:"Unavailable"` within 45 s. The budget is above the 30 s default of
   `PUSH_INVOKE_TIMEOUT_MS`, because [I-GATE] notes the first call after a kill can hang.
3. Its `err` must be neither `"rate limited"` nor `"too many in-flight invokes"`. Those are the
   refusals that would mask a dispatch that never happened (`push_ws.rs:1693-1697`).
4. Then `storage.get {collection, key, owner_id:""}` on an absent key on the same socket answers
   `NotFound`: the socket keeps serving.

This adds up to ~50 s to the kill window; record that in the assertion's comment.

**[PH15] Late backplane batch dropped by the real gateway-svc (A).** Split only, after [PH4]
(`:3623`) inside the `if split` block.
1. Bind a socket for a player.
2. Inject with `deliver_batch`, using one random producer, all on hub `ordertest`:
   1. seq 5 `ordertest.new`
   2. seq 3 `ordertest.old`
   3. seq 6 `ordertest.sentinel`
3. Read `ordertest.new`.
4. Then make **one** `await_frame` call whose predicate matches `ordertest.old` **or**
   `ordertest.sentinel`, and assert the first match is the sentinel.

Each injection awaits its edge reply, which is written after the handler runs, and all three
share one FIFO lane. So "sentinel first" means seq 3 was processed and dropped, with no timer.
This cannot be proven in the monolith (no backplane).

**(b) Why last of the code steps:** it needs Steps 1 and 5.

**(c)** Do **not** run split-proof or verifyctl here; that is Step 9. A compile check of
`splitproof` is fine after the rollout check.

**(d)** `[opus]` core-implementer, `model:"opus"`, effort think hard. Commit
`test(splitproof): prove multi-svc pipelined /push invoke, mid-socket revocation, remote failure
results and late-batch drop`. Opus trailer. Then **proof-auditor** (`model:"opus"`) and
core-reviewer.

### Step 8 — Docs and false-prose sweep `[docs]` (model: sonnet)

**(a) Exact targets:**
- **Ordering claims → "ordered per producer; a batch arriving after a newer one from the same
  producer is dropped":**
  - `core/push/src/lib.rs:117-121`
  - `core/remote/src/push_backplane.rs:13-16` and `:373-375`
  - `modules/gateway/src/push_ws.rs:1241-1246`
- **`push_ws.rs`:**
  - `:11-14` and `:416`: cap refusals are HTTP 503 before the upgrade; `capacity` is sent only for
    unwritten invoke refusals.
  - `:405`: `Unauthorized` is also sent on re-verify when the token now resolves to a different
    player.
  - `:592-593`: "enqueued to".
  - `:1539-1540`: describe the liveness check.
  - `PushLimits` doc: the liveness invariant is enforced on the environment path only.
- **`docs/reference/push-hub.md`:**
  - replace "SignalR-shaped" with what a hub is: a topic-prefix label plus an outbound lane, with
    no subscription and no per-hub authz;
  - hello is optional with both credential headers;
  - fix the shadowing direction in "Carried gaps";
  - document the batch header, per-producer ordering and the late-batch drop;
  - document the shared producer `mpsc(1024)`, which drops the newest message (the per-socket
    queue evicts the oldest);
  - document that `push.deliver` authz is mTLS only;
  - document the 32 KiB payload cap, the 32 groups / 128-byte names, the 64-verb budget, and the
    connection's own `online` frame;
  - add `PUSH_LIVENESS_TIMEOUT_MS` (default 1.5 × interval; an explicit value must exceed the
    interval) to the env table (`:228-246`);
  - add `liveness_timeout` (advisory, retryable) to the close codes;
  - add [PH12]–[PH15] to the proof list.
- **`CLAUDE.md`:** in the gateway bullet, drop "SignalR-shaped" and add the one-line hub
  description; add the new tags to the split-proof scenario list in "One test rollout at a time".
- **`docs/roadmap/feature-tracker.md`:** update the push-hub row and add a change-log entry.
- **This plan:** an Errata section for any deviation in Steps 1–7.

The earlier two plans are archives and are not rewritten (memory
[[historical-docs-are-archives]]).

**(b) Why last:** it describes landed behaviour. The false comments in Steps 1–3 stay false until
this step, by design (the docs lane owns comment prose).

**(d)** `docs-writer`, `model:"sonnet"`. Sonnet trailer. Then one core-reviewer pass.

### Step 9 — Acceptance `[inline]`

1. Run `pgrep -x cargo; pgrep -x rustc` as its own call and read it.
2. `target/debug/devctl status`: no fleet.
3. Re-check cargo/rustc, then run `cargo run -p verifyctl -- --fast` once.
4. Read the stage table from its log.

On any red stage, stop, diagnose and report options. No fix loop (memory
[[live-acceptance-report-dont-fix]]).

Then audit the trailers: `git log -<N> --format="%h %B" | grep Co-Authored` against each step's
lane.

## Reviews

- One core-reviewer pass per step before the next step is dispatched.
- Step 7 adds proof-auditor.
- Stage files by explicit path while agents run.
- Commit after each step.

## Known gaps carried forward (recorded, not scheduled)

- **Ordering semantics.** A late backplane batch is dropped, not delivered in order; push stays
  best-effort. Cross-producer ordering stays undefined in both topologies.
- **Unchanged, documented in Step 8:**
  - `push.deliver` authz stays mTLS-only.
  - The producer queue stays shared and drops the newest message.
- **No `/metrics` signal.** `STALE`, like the existing `DROPS`/`OVERSIZE`, only logs.
- **Conformance.** `tools/conformance/src/policy.rs:629-631` marks gateway `EnvValidation` as
  `na`, with a reason `PushLimits::from_values` contradicts; this needs a conformance rollout with
  a fixture.
- **Still open from earlier plans:**
  - no `Target::Players`;
  - no frame-parser fuzz target;
  - no `/push` routecheck;
  - WS invokes are aborted on stop;
  - no invoke resume.

## Review round 1 — disposition

| # | Finding | Disposition |
|---|---|---|
| 1 | Optional origin = skew bridge, `None` arm keeps the old rail | Accepted: mandatory `Batch` header, bare array dies, [PH4] stamps (decision 1) |
| 2 | `split_batch` measures unstamped bytes | Accepted: constant `BATCH_HEADER_MAX` subtracted; Step 2 asserts encoded chunks ≤ budget |
| 3 | [PH14] cannot see partial leaks; refusals also `Unavailable` | Accepted: E5 scoped to the Remote failure result; refusal strings excluded; no permit claim (decision 6) |
| 4 | Fixed 90 s default breaks a raised interval | Accepted: default derived as 1.5 × interval; explicit-value check recorded as a semantic change; env-only enforcement documented |
| 5 | E3 test already exists and cannot fail | Accepted: E3 closed by existing tests, no new test |
| 6 | [PH15] timer-based absence; `await_topic` discards | Accepted: single either-or predicate, no timer |
| 7 | [PH12] overclaims concurrency | Accepted: claim is "pipelined + correlated" |
| 8 | ids are `i64`; buffered `Close` | Accepted (Steps 5, 6) |
| 9 | Fourth ordering comment `push_backplane.rs:373-375` | Accepted (Context, Step 8) |
| 10 | Doc-comment edits in implementer lanes | Accepted: all prose moved to Step 8 |
| 11 | Eviction rationale wrong | Accepted: stream-implicit-open + read grace argument |
| 12 | Producer-id test cannot fail | Accepted: replaced by constant-producer / increasing-seq over the real drain |
| 13 | Exact delta on a global counter | Accepted: queue contents primary, `>=` on `STALE` |
| 14 | Conditionals / missing (b) | Accepted |
| 15 | [PH14] placement | Accepted: signature, position, time cost stated |
| 16 | Global mutex serializes producers | Accepted: per-producer entry lock |
| 17 | ws 1001 is `Shutdown`'s; advisory close | Accepted: 1000, advisory noted |
| 18 | No metric | Recorded as a known gap |
