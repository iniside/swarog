# Push plane — server→client delivery over a dedicated QUIC plane

**Plan written 2026-09-02-2242. Revision 2** (2026-09-02-2358), after an adversarial
review returned REJECT with twelve findings; the errata at the end records every
change and the two deliberate deferrals.

Sequence: a new capability, not a tracker row (closest neighbour is seq #3c "Push
notifications (FCM/APNs)", which is a *third-party* transport — this is our OWN
transport, and the prerequisite for the tracker's blocked social rows, gap-doc #15).

---

## Context — what exists, and why not extend it

Researched 2026-09-01/02 with four parallel subagents (edge surface, gateway
identity/lifecycle, remote fan-out, app-owned plane patterns). Every claim is cited to
code; nothing is inferred from prose.

### The three seams that already carry cross-process traffic, and why none fits

**1. The player-QUIC plane (`core/edge/src/player.rs`) — why not extend it.**
It already serves external, certless clients on `PLAYER_ALPN`, so push looks at first
like "one more stream shape" here. It is not, for four verified reasons:

- **Strictly client-initiated.** `serve_conn` is a pure `conn.accept_bi()` loop
  (`core/edge/src/player.rs:495`); `PlayerServer` never calls `open_bi`. A repo-wide
  grep for `open_uni|accept_uni|send_datagram|read_datagram` over
  `core/ modules/ api/ cmd/ tools/` returns **zero hits**.
- **Its drain contract assumes short work.** `ShutdownState::enter` counts every
  in-flight unit and `RunningServer::shutdown` waits for `idle()`, then aborts
  stragglers (`core/edge/src/server.rs:482-505`). The `closing` watch is observed by
  the accept loops only — `serve_stream` (`player.rs:537-570`) never subscribes. A
  long-lived stream holding an `InFlightGuard` makes `idle()` unreachable: shutdown
  burns the whole `EDGE_DRAIN_GRACE_MS` (5000, `core/app/src/lib.rs:29`) and then
  `abort()`s the stream — the "aborted, not drained" outcome the guards exist to
  prevent. Three tests pin that contract: `shutdown_tests.rs:58`, `:205`, `:359`.
- **Its hardening is deliberately tight because the peer is untrusted:**
  `PLAYER_IDLE_TIMEOUT_MS` 30s (`player.rs:67`), `MAX_PLAYER_FRAME` 1 MiB
  (`player.rs:47`), `stream_receive_window` clamped to that frame (`player.rs:246`),
  `PLAYER_STREAM_GRACE` 30s bounding both peer-controlled waits (`player.rs:63`).
  Push needs a long idle and a keepalive; relaxing those on the player plane would
  weaken the RPC path for traffic that does not use it.
- **Its rate limiting is per stream-open.** `RequestLimiter::allow` is charged once in
  `serve_stream` before the frame is read (`player.rs:545`), so a stream open for
  hours costs one token. Nothing on that plane describes server-originated traffic.

**2. The internal mTLS edge (`core/edge/src/server.rs`) — why not extend it.**
Direction is structural: `edge::Server` only calls `Endpoint::server`
(`server.rs:251`), `edge::Client` only `Endpoint::client` + `connect`
(`client.rs:73,87`). `RunningServer` exposes `local_addr`/`close`/`shutdown` and hands
out **no `quinn::Connection`** (`server.rs:451-471`), so an inbound connection cannot
be reused in reverse. It is also mTLS with CA-signed client leaves, which a player
cannot hold (`player.rs:1-18`). It stays what it is — and this plan *uses* it, in its
existing direction, as the domain→gateway ingress (Step 6).

**3. The durable event plane + invalidation — why not either.**
`asyncevents` gives at-least-once with a Postgres checkpoint
(`core/asyncevents/src/store.rs:101-115`); a push to a connected socket is inherently
non-replayable, so the checkpoint would advance for an offline player and the
guarantee we paid for would be fictional. `invalidation` has the right *semantics*
("freshness, not delivery") but dispatches on channel name and **discards the
payload** (`core/invalidation/src/lib.rs:483`) — a broadcast, where push is addressed.
Decisively: `cmd/gateway-svc` runs `.without_db()` (`cmd/gateway-svc/src/main.rs:163`)
and "DB ⇒ plane" is unconditional (`core/app/src/lib.rs:728-746`) — a pool on the only
public ingress would make an unreachable Postgres a front-door boot failure and put
three plane checks on its `/readyz` (`core/app/src/lib.rs:774-844`).

### Decisions taken with the user before this plan

1. **Push is a separate plane on its own connection** — the HTTP-vs-WebSocket split.
   Rejected: a long-lived stream multiplexed onto the existing player connection (it
   shares `ConnGuard`/`RequestConnGuard`/`ShutdownState`, so the drain conflict
   survives as a set of special cases).
2. **Gateway becomes bidirectional** — it gains an internal-edge listener so domain
   processes can hand it messages. Non-negotiable.
3. **No `player_id → node` directory.** The sender fans out to *every* front-door
   process; each filters against its own local map. Neutral to a future multi-gateway
   fleet instead of hard-coding today's single front.
4. **The plane lives in `core/` and imports no `api/` crate** (hard rule 1), so it
   carries an **opaque payload** — exactly as `core/bus`'s `Transport` deals only in
   "contracts, topic strings + `[u8]`" (`core/bus/src/lib.rs:531-565`) while typing is
   declared in `api/<name>/<name>events`.
5. **Absent plane = loud failure.** Never a silent no-op. Revision 2 makes this
   *reachable* (see errata E11): a delivery that reached zero fronts is distinguishable
   from a delivered one, and a `PushServer` with no listen address fails startup.
6. **Presence is the acceptance vehicle** — in-memory on the front, no storage.

### Scope boundary

IN: the push plane (transport, registry, plane handle), the gateway inbound face, the
`core/remote` fan-out sender, one real domain producer (notifications), fleet wiring,
tests, and a minimal in-memory presence emitter.

OUT, named: persistent/queryable presence; a friends graph (so presence broadcasts to
all connected players — a smoke test, not a feature); **the C# fixture stays RPC-only**
(carried gap); fixing replication of other services; `weles` port minting for a
replicated front; an api-key class check on the push bind (errata E5b).

---

## Design summary

**Stream model — no server-initiated streams, and no client writes after the bind.**
The client dials the push plane and opens ONE bidirectional stream, writes a
`PushHello` frame, and reads a `PushAck`. From then on the client **only reads**; the
server writes `PushEnvelope` frames on its send half for the life of the connection.
This sidesteps `max_concurrent_uni_streams` (never set anywhere in the repo), keeps
stream creation on the client as on every other plane, and — decisively — means there
is no inbound frame path to rate-limit or abuse (errata E7/E8).

**Identity and revocation.** `SessionVerifier::verify` returns
`Result<Option<String>, VerifyUnavailable>` (`modules/gateway/src/verifier.rs:38-41`).
Revision 1 proposed a client-renewed lease; review found it never re-verified, so
revocation was unbounded. Revision 2 instead **re-runs the verifier server-side on a
timer** (`PUSH_REVERIFY_SECS`, default 300): no protocol, no client cooperation, no
contract change, and revocation latency is bounded by the interval by construction.
The three-way result is preserved end to end — collapsing `Unavailable` into "denied"
would log every player out the moment accounts blips
(`modules/gateway/src/verifier.rs:22-27` says so outright).

**Fan-out.** A domain process's `ctx.push()` enqueues into a bounded in-process queue
drained by a background task that calls `push.deliver` on **every selectable gateway
instance** over the internal edge. `Push::send` never awaits the network (errata E10).
`core/remote` has no such primitive today: `select`/`select_excluding` return
`Option<usize>` — one slot (`core/remote/src/lib.rs:918-950`) — and the only
fire-and-forget call in the tree is in a test (`modules/gateway/src/tests.rs:2542`).

---

## Step 1 — push transport, server half `[opus]` (core-implementer)

**(a) What.** New `core/edge/src/push.rs`; exports in `core/edge/src/lib.rs:31-41`;
`PUSH_ALPN` + two TLS wrappers in `core/edge/src/tls.rs`.

**(b) Why now.** Every later step needs these types. No dependency beyond
`frame.rs`/`tls.rs`.

**(c) How.**

`tls.rs`: add `pub const PUSH_ALPN: &[u8] = b"edge-push";` beside `PLAYER_ALPN`
(`tls.rs:45`). `server_tls_public` (`tls.rs:233`) and `TrustAnchor::client_tls_public`
(`tls.rs:298`) hardcode `PLAYER_ALPN.to_vec()` at `:241`/`:304`; extract each body into
a private `*_alpn(&self, alpn: &[u8])` and add `server_tls_push()`/`client_tls_push()`.
**Do not** change the existing two signatures — `player.rs:236` and `player.rs:629`
call them.

Envelopes (serde, `#[serde(default, skip_serializing_if)]` on every optional, mirroring
`PlayerRequest` at `player.rs:105-124`):

```rust
pub struct PushHello { pub token: String }
pub enum PushOutcome { Ok, Denied, Unavailable }   // serialized as a lowercase tag
pub struct PushAck { pub outcome: PushOutcome, pub error: Option<String> }
pub struct PushEnvelope { pub topic: String, pub payload: Box<RawValue>, pub seq: u64 }
```

`PushOutcome::Unavailable` is the load-bearing third arm: a client must retry, not
re-authenticate.

Constants: `MAX_PUSH_FRAME = 64 << 10`, `PUSH_IDLE_TIMEOUT_MS = 300_000`,
`PUSH_KEEPALIVE_MS = 15_000` (server-side), `PUSH_QUEUE_DEPTH = 256`,
`PUSH_CONNS_PER_PLAYER = 4`, `PUSH_HELLO_GRACE = 10s`, **`PUSH_WRITE_GRACE = 10s`**,
`PUSH_REVERIFY_SECS = 300`, `DEFAULT_PUSH_MAX_CONNS = 1024`,
`DEFAULT_PUSH_MAX_CONNS_PER_IP = 32`.

**The registry is a separate object, not a `PushServer` field.** `core/app` empties the
shared handle with `std::mem::take` (`core/app/src/lib.rs:882`, `:906`), so anything
left on the `PushServer` the module retains is a husk (errata E1):

```rust
pub struct PushRegistry { conns: Mutex<HashMap<String, Vec<Arc<ConnSlot>>>>, /* counters */ }
impl PushRegistry {
    pub fn deliver(&self, player_id: &str, topic: &str, payload: &[u8]) -> usize; // slots reached
    pub fn online(&self) -> Vec<String>;
}
impl PushServer {
    pub fn registry(&self) -> Arc<PushRegistry>;   // obtainable BEFORE listen; survives mem::take
}
```

`PushVerifier` — the injected identity seam, same idiom as `PlayerHandler`
(`player.rs:127-133`), preserving the three-way result:

```rust
pub struct Unavailable;
pub type PushVerifier =
    Arc<dyn Fn(String) -> BoxFuture<'static, Result<Option<String>, Unavailable>> + Send + Sync>;
pub type PushLifecycleHook = Arc<dyn Fn(&str, bool) + Send + Sync>;  // (player_id, online)
```

`PushServer`: `new()`, `set_verifier` / `set_hook` (both `OnceLock` first-set-wins,
copying `PlayerServer::set_handler` at `player.rs:179-181`), `with_conn_limits`,
`with_reverify(Duration)`, `registry()`, `liveness() -> Arc<AtomicBool>` (obtainable
pre-listen, for Step 5's readiness — errata E2), `listen(self, addr, &DevCA)`.

Admission copies the player accept loop's shape (`player.rs:256-302`): stateless Retry
when `!incoming.remote_address_validated()`, then global + per-IP `ConnLimiter` (make
the existing one `pub(crate)` and **reuse it — do not clone it**), then spawn.

Per connection: `accept_bi()` once; read one frame capped at `MAX_PUSH_FRAME` under
`PUSH_HELLO_GRACE`; decode `PushHello`; call the verifier. `Err(Unavailable)` ⇒
`PushAck{Unavailable}` and close; `Ok(None)` ⇒ `PushAck{Denied}` and close;
`Ok(Some(pid))` ⇒ register, `PushAck{Ok}`, fire the hook with `online: true`, run the
writer loop. **The client sends nothing further; any further inbound frame closes the
connection.** On teardown, deregister and fire the hook with `online: false`.

**Writer loop — a `select!`, not a bare await** (errata E8):

```
loop { select! {
    _ = slot.notify.notified() => drain the queue, each write_frame bounded by PUSH_WRITE_GRACE,
    _ = reverify_tick          => re-run the verifier; Ok(Some(pid)) keeps it, anything else closes,
    _ = closing.changed()      => close,
} }
```
A bare `notify.notified()` would mean an idle connection never re-verifies, and an
unbounded `write_frame` would let a peer that grants no flow-control credit park the
task forever while the server's own keepalive holds the connection open — the exact
pathology `PLAYER_STREAM_GRACE` exists to prevent (`player.rs:52-62`), reachable with
the tool the repo already has (`player_tests.rs:250-265`).

`ConnSlot { id, queue: Mutex<VecDeque<Bytes>>, notify: Notify, dropped: AtomicU64 }`.
`enqueue` pushes and, past `PUSH_QUEUE_DEPTH`, **drops the oldest** and counts it —
the durable copy lives in the notifications inbox, and a stalled reader must never grow
our memory. Exceeding `PUSH_CONNS_PER_PLAYER` evicts that player's oldest slot.

`RunningPushServer::shutdown(grace)` — **its own drain contract, not `ShutdownState`'s**:
flip closing, stop accepting, close every registered connection with an application
code meaning "reconnect", then `endpoint.close()` + `wait_idle()` bounded by
`min(grace, 3s)` (mirroring `server.rs:501-504`). It must NOT wait for writers to go
idle.

**(d) Dispatch.** `core-implementer`, `model:"opus"`, effort high.

---

## Step 2 — push transport, client half `[opus]`

**(a) What.** `PushClient` in `core/edge/src/push.rs`; exports in `lib.rs`.

**(b) Why now.** Nothing is testable end to end without it; separated from Step 1 so
the registry/drain review is not diluted by client mechanics.

**(c) How.**

```rust
pub struct PushClient { _endpoint: quinn::Endpoint, conn: quinn::Connection, recv: Mutex<quinn::RecvStream> }
impl PushClient {
    pub async fn connect(addr: SocketAddr, trust: &TrustAnchor, token: &str)
        -> Result<(PushClient, PushAck), Error>;
    pub async fn recv(&self) -> Result<PushEnvelope, Error>;
    pub fn close(&self);
}
```

`connect` dials with `trust.client_tls_push()`, opens one bidi stream, writes
`PushHello`, reads `PushAck` and **returns it** — a denied or unavailable bind is a
*successful call* carrying that outcome, never a transport error (the pinned error
grammar, `player.rs:648-672`). It then `send.finish()`es the send half (the client
never writes again) and keeps the recv half. Client `TransportConfig` sets the same
keepalive as the server so a mobile NAT binding survives.

**(d) Dispatch.** `core-implementer`, `model:"opus"`, effort high.

---

## Step 3 — `core/push`: the plane handle `[opus]`

**(a) What.** New crate `core/push/` + workspace member; `core/lifecycle/src/context.rs`
gains a `push` field, a `push()` accessor and a `with_push()` builder.

**(b) Why now.** It is the seam modules code against; Steps 4/5/6 implement or install
it.

**(c) How.** Dependencies: `async-trait`, `thiserror`, `tracing`, `contrib` — and
**no** `api/*`, **no** `core/edge`, so `core/lifecycle` does not pull the transport in.

```rust
#[async_trait] pub trait Sink: Send + Sync {
    /// Returns how many front-door processes accepted the message. Err = the send
    /// could not even be attempted.
    async fn deliver(&self, player_id: &str, topic: &str, payload: &[u8]) -> Result<usize, Error>;
}
pub struct Push { sink: OnceLock<Arc<dyn Sink>> }
impl Push {
    pub fn new() -> Push;
    pub fn install(&self, sink: Arc<dyn Sink>);   // app::run only, AFTER App::build
    pub async fn send(&self, player_id: &str, topic: &str, payload: &[u8]) -> Result<usize, Error>;
}
pub const SINK_SLOT: contrib::Slot<Arc<dyn Sink>> = contrib::Slot::new("push.sink");
#[derive(Debug, thiserror::Error)] pub enum Error { NoSink, Backlogged, Transport(String) }
```

`Context` copies the `invalidation` shape exactly (field `:39`, accessor `:91-93`,
builder consumed only by `app::run`): an **always-present `Arc<Push>`**, never an
`Option`, so no module branches on topology.

**Absent-plane convention (decision 5), stated because the repo has four and they are
not interchangeable:** `send` with no sink returns `Error::NoSink` and logs at
`error!`. Not a panic (that would kill a request path); not a silent no-op like
`Invalidation::register` on a DB-less process. `Ok(0)` — nobody was connected anywhere
— is a distinct, logged-at-`debug` outcome, never conflated with `Err`.

**(d) Dispatch.** `core-implementer`, `model:"opus"`, effort high. Touches `Context`.

---

## Step 4 — `core/remote`: fan-out and the sender module `[opus]`

**(a) What.** `core/remote/src/lib.rs` — `Pool::deliver_all`, a `PushSender` sink, and
a `PushStub` **module**; `core/remote/Cargo.toml` gains `push`.

**(b) Why now.** Step 5 installs a sink per process. Must follow Step 3.

**(c) How.** `Instance`, `InstanceHealth` (`core/remote/src/lib.rs:703`) and
`InstanceFactory` (`:733`) are private to the crate, so the primitive cannot live
elsewhere.

```rust
impl Pool {
    /// Calls `method` on EVERY currently-selectable instance, concurrently.
    /// Returns how many accepted it. Never Err — a dead peer is a dropped message.
    pub async fn deliver_all(&self, method: &str, payload: &[u8]) -> usize;
}
```
`refresh()` (the 5s-throttled reconcile, `:827-842`), then snapshot `(addr, caller)`
for every instance whose `health.is_selectable(now)` holds (`:681-684`), cloned out
from under the std mutex exactly as `Pool::call` does (`:1086-1089`), then
`join_all` of per-instance calls each wrapped in
`tokio::time::timeout(PUSH_DELIVER_TIMEOUT)` (new const, 2s — the crate has no
per-instance timeout today). `RetryMode::Never` throughout: replaying a best-effort
message is worse than dropping it. Errors are counted and `debug!`-logged.

**`PushStub` — a `lifecycle::Module`, mirroring `remote::Stub`** (`:1443-1628`), so the
sender gets a real lifecycle owner (errata E10):
- `init`: contribute `Arc<PushSender>` to `push::SINK_SLOT`.
- `start`: spawn the pool refresh loop (`spawn_pool_refresh`, `:1355-1363`) **and** the
  drain task.
- `stop`: signal + join the drain task, then `pool.stop()` — without which every domain
  svc leaks a probing edge connection to gateway at shutdown.

`PushSender { tx: mpsc::Sender<Msg>, .. }`: `Sink::deliver` does a **non-blocking
`try_send`** and returns immediately — `Err(Full)` maps to `Error::Backlogged`, never a
stall. The drain task pops and calls `deliver_all`. This is what makes `Push::send`
safe to call inside a domain request path: no network wait, no 2s coupling.

**Note:** `modules/gateway/src/lib.rs:1207-1216` records that a live `Pool` cannot be
shared across the module/core boundary. `PushStub` is constructed in `cmd/*` roots
(Step 7b), not in a module, so it does not hit that.

**(d) Dispatch.** `core-implementer`, `model:"opus"`, effort high.

---

## Step 5 — `core/app`: install the plane `[opus]`

**(a) What.** `core/app/src/lib.rs` — `Config`, `run`'s signature, sink selection,
startup validation, readiness, `ordered_teardown`.

**(b) Why now.** Needs Steps 1–4; Step 7 depends on its signature.

**(c) How.**

- `Config` gains `push_addr` from `PUSH_EDGE_ADDR` (**default empty — the plane is off
  unless set**; unlike `EDGE_ADDR`, which defaults to `":9000"` at
  `core/app/src/lib.rs:27`, a defaulted push port would silently claim a port in every
  process), plus `PUSH_MAX_CONNS`, `PUSH_MAX_CONNS_PER_IP`, `PUSH_REVERIFY_SECS` — read
  **here, never in a module**.
- `run` gains a 5th parameter `push_server: Option<Arc<Mutex<edge::PushServer>>>`.
- **Startup validation, before Build:** `push_server.is_some() && push_addr.is_empty()`
  ⇒ `bail!`. A server that never listens under a handle modules were told to use is
  precisely the silent no-op decision 5 forbids.
- **Sink selection AFTER `app.build()`** (`core/app/src/lib.rs:766`), not at `Context`
  construction (`:751-757`): `ctx.contribute` only happens in `register`/`init`, so
  `SINK_SLOT` is empty before Build (errata E6). Order: a local sink over
  `push_server.registry()` if present; else the single `SINK_SLOT` contribution; else
  leave it sink-less. More than one contribution ⇒ `bail!`.
- Readiness: contribute a `ReadyCheck` named `"push"` over the `liveness()` handle
  taken **pre-listen**, following `core/app/src/lib.rs:822-843`, so it is in place
  before `snapshot_readiness_checks` (`:849`).
- Listen beside the player listen (`~:992-1010`) when `push_addr` is non-empty.
- **`ordered_teardown` (`:1166-1191`): drain push FIRST**, before the player edge — its
  shutdown closes connections rather than waiting for idle, so it is fast and releases
  clients before the RPC plane starts refusing them.

**(d) Dispatch.** `core-implementer`, `model:"opus"`, effort high.

---

## Step 6 — the gateway: bind, inbound face, presence `[opus]`

**(a) What.** `modules/gateway/src/lib.rs` (`Gateway`, `init`), new
`modules/gateway/src/push.rs`.

**(b) Why now.** First consumer of Steps 1–5, and the only place a verified `player_id`
exists (`modules/gateway/src/lib.rs:850-854`).

**(c) How.**

- `Gateway::with_push_edge(Arc<Mutex<edge::PushServer>>)` — a builder mirroring
  `with_player_edge` (`lib.rs:406-409`). **This does store a server handle in the
  module**, exactly as `player_edge` already does; it passes archcheck under the same
  carve-out (`tools/archcheck/src/main.rs:1189` matches `Option<` before
  `edge::Server`, which `edge::PushServer` is not). Stated plainly because revision 1
  claimed the module "never holds a server", which was false (errata E9).
- In `init`, beside `set_handler` (`lib.rs:464`): capture `registry()` **before** the
  handle is emptied, `set_verifier` over the resolved `Arc<dyn SessionVerifier>`
  **preserving its three-way result** — `Err(VerifyUnavailable)` ⇒ `Unavailable`, never
  `Denied` — and `set_hook` for presence.
- **No api-key check on the bind.** The key policy is per-operation
  (`check_api_key(.., method)`) and a bind is not an operation; inventing a synthetic
  method would fail `modules/apikeys/src/tests.rs:79`, which asserts every policy entry
  is a real `opscatalog::OPERATIONS` entry. Recorded as a deliberate gap (errata E5b).
- **Inbound edge face**, contributed unconditionally in `init`:
  `ctx.contribute(edge::EDGE_SLOT, edge::EdgeReg::new(move |server| server.handle("push.deliver", h)))`.
  `app::run` drains the slot only for an edge-serving process
  (`core/app/src/lib.rs:581-586`, `:881-903`), so the module stays topology-blind and
  the monolith simply never applies it. The handler decodes `{player_id, topic, payload}`
  and calls `registry.deliver(..)`, answering an empty ok.
- **Presence**, `modules/gateway/src/push.rs`: the lifecycle hook emits `push.presence`
  `{player_id, online}` to every *other* player in this front's registry. Deliberately
  front-local — a global view needs the friends graph and a store, both out of scope.

**(d) Dispatch.** `core-implementer`, `model:"opus"`, effort high.

---

## Step 7a — `app::run` arity sweep `[sonnet]`

**(a) What.** All 16 `cmd/*` mains: pass `None` as the new 5th argument.
**(b) Why now.** Mechanical; unblocks compilation after Step 5.
**(c) How.** Nothing else changes in this step — no new modules, no env.
**(d) Dispatch.** `model:"sonnet"`, effort low.

---

## Step 7b — front and domain roots `[opus]`

**(a) What.** `cmd/gateway-svc/src/{main.rs,lib.rs}`, `cmd/server/src/{main.rs,lib.rs}`,
every `cmd/<domain>-svc/src/lib.rs`.

**(b) Why now.** Needs Steps 4–6. Split from 7a because it is **not** mechanical: it
flips the front door to serve an internal edge for the first time and constructs
`Pool`-backed senders (errata E12).

**(c) How.**
- `cmd/gateway-svc/src/main.rs`: build `Arc<Mutex<edge::PushServer>>`, hand it to
  `Gateway::with_push_edge` and to `run`; **flip the 3rd argument from `None` to
  `Some(edge::Server::new())`** (`main.rs:161-169`). `register_process_describe` is
  unconditional for an edge-serving process (`core/app/src/lib.rs:611-617`), so
  gateway-svc starts answering `__describe` with an empty op list — harmless, and
  recorded in Step 15.
- `cmd/server/src/main.rs` (monolith): build and pass a `PushServer`; `edge_server`
  stays `None`; no `PushStub` (the sink is local).
- Each `cmd/<domain>-svc/src/lib.rs`: add `remote::PushStub::new(wiring.peer_set_or("gateway", …))`
  to `modules()`. **No fleet dependency on gateway is declared** — see Step 8.

**(d) Dispatch.** `core-implementer`, `model:"opus"`, effort medium.

---

## Step 8 — fleet, across all four authorities `[sonnet]`

**(a) What.** `tools/processctl/src/fleet.rs`, `tools/processctl/src/fleet_tests.rs`,
`weles/fleet.split.toml`, `weles/fleet.monolith.toml`.

**(b) Why now.** Nothing boots in split until ports and peer addresses exist.

**(c) How.**
- **Edge port for gateway-svc is `9013`, not 9012.** The split's edge band is 9000–9012
  and **`mail-svc` already owns 9012** (`tools/processctl/src/fleet.rs:796`, wired into
  admin's peers at `:854`). Revision 1 said "9000-9011" and picked 9012 — a guaranteed
  `EADDRINUSE` (errata E3).
- `fleet.rs` gateway spec (`:818-834`): `edge_port: Some(9013)`, `EDGE_ADDR=":9013"`,
  `PUSH_EDGE_ADDR=":9101"` beside `PLAYER_EDGE_ADDR=":9100"`.
- Monolith (`:932-990`): `PUSH_EDGE_ADDR=":9101"`.
- Every domain svc env gains `GATEWAY_EDGE_ADDR=127.0.0.1:9013`.
- **`fleet_tests.rs:93`** hardcodes the gateway row with `None` for `edge_port` —
  update it, or the test stage goes red with no plan entry explaining why (errata E4).
- **`weles/fleet.split.toml`** (gateway entry ~`:171-185`) has no `edge_port` and no
  `EDGE_ADDR`; since Step 7b flips gateway-svc to serve an edge unconditionally and
  `DEFAULT_EDGE_ADDR` is `":9000"` (`core/app/src/lib.rs:27`), leaving it would collide
  with characters-svc (`fleet.split.toml:119`). Add `edge_port = 9013`, `EDGE_ADDR`,
  `PUSH_EDGE_ADDR`, and `GATEWAY_EDGE_ADDR` on every domain entry.
  **`weles/fleet.monolith.toml`**: `PUSH_EDGE_ADDR` for parity.
- **Do not add `"gateway-svc"` to any domain svc's `dependencies`.** The validator
  requires a dependency to appear EARLIER in the vector (`FleetError::DependencyNotEarlier`,
  `fleet.rs:518-530`) and gateway is listed last (`:927`) — a cycle is structurally
  unrepresentable. Correct on the merits: push is best-effort, so a domain process must
  start and run with no front door present.

**(d) Dispatch.** `model:"sonnet"`, effort medium.

---

## Step 9 — notifications emits a push `[opus]`

**(a) What.** `modules/notifications/src/lib.rs` — after an inbox row is inserted, call
`ctx.push().send(player_id, "notifications.new", ..)`.

**(b) Why now.** Step 13's cross-process assertion needs a **real** domain producer.
Revision 1 handed `[PS3]` "a domain caller added for the proof", i.e. unwritten
production code smuggled into a test lane (errata E12). This is the genuine feature:
the inbox already fans in from durable events, and a push is the notification of it.

**(c) How.** The send happens **after** the delivery transaction commits, never inside
it — a best-effort notification must not be able to affect the transaction, and the
row must exist before a client is told to fetch it. The payload carries the row id and
topic only, never the body. `Err` is logged, never propagated.

**(d) Dispatch.** `core-implementer`, `model:"opus"`, effort medium.

---

## Step 10 — unit tests: the push transport `[test-author]`, `model:"opus"`

Covers Steps 1–2, in `core/edge/src/push_tests.rs`. Each names the previously-wrong
branch:
- bind denied ⇒ `PushAck{Denied}` on a **successful** call; nothing registered.
- **verifier `Unavailable` ⇒ `PushAck{Unavailable}`, NOT `Denied`** — the mass-logout
  branch (`modules/gateway/src/verifier.rs:22-27`).
- deliver reaches the client; deliver for an unknown player returns 0 and does not err.
- **queue overflow drops the OLDEST**: a client that stops reading, `PUSH_QUEUE_DEPTH+10`
  enqueued, then resumes and sees the newest window plus a non-zero drop count.
- **a stalled reader cannot park the writer**: with a tiny `stream_receive_window`
  (`player_tests.rs:250-265` is the tool), the write is abandoned at `PUSH_WRITE_GRACE`
  and the connection torn down — the exhaustion branch.
- **re-verification closes an idle connection** whose token has been revoked, proving
  the timer arm of the `select!` runs without any traffic.
- `PUSH_CONNS_PER_PLAYER` eviction drops the oldest slot.
- **shutdown closes promptly and does not wait for idle**, with a live connection
  attached — the branch `ShutdownState` would have got wrong.
- ALPN isolation both ways, mirroring `core/edge/src/lib.rs:532-563`.

Timing: short `with_reverify`, explicit state and hang-guards with headroom — never a
real-clock race ([[timing-sensitive-tests-doctrine]]).

---

## Step 11 — unit tests: plane handle and fan-out `[test-author]`, `model:"sonnet"`

Covers Steps 3–4, in `core/push/src/tests.rs` and `core/remote`'s suite:
`Push::send` with no sink ⇒ `Error::NoSink` (the loud-failure branch that must be
reachable); `deliver_all` skips non-selectable instances, returns 0 with an empty set,
does not err when every instance is unreachable, and honours the per-instance timeout
(fake `Caller`s, no real sockets); `PushSender::deliver` returns `Backlogged` rather
than blocking when the queue is full; `PushStub::stop` joins the drain task and stops
the pool.

---

## Step 12 — unit tests: gateway and app wiring `[test-author]`, `model:"sonnet"`

Covers Steps 5–6 and 9, in `modules/gateway/src/tests.rs` and `core/app`'s suite:
the push verifier maps `Err(VerifyUnavailable)` to `Unavailable` and `Ok(None)` to
`Denied`; the `push.deliver` edge handler decodes and reaches the registry; the
presence hook emits to others but never to the joining player; `run` bails when
`push_server` is `Some` with an empty `PUSH_EDGE_ADDR`; sink selection prefers the
local registry and bails on two `SINK_SLOT` contributions.

---

## Step 13 — split-proof assertions `[test-author]`, `model:"opus"`

`tools/splitproof/src/` — `[PS1]`–`[PS5]`, plus `[PS1m]`/`[PS3m]` on the monolith
parity re-run. The at-risk topology is split
([[verify-the-at-risk-path-not-the-safe-one]]).

- `[PS1]` register/login through gateway-svc, connect to `:9101` with the real bearer,
  assert `PushAck{Ok}`.
- `[PS2]` a bad token ⇒ `Denied`, and no frame ever arrives.
- `[PS3]` **the cross-process proof**: drive the notifications inbox from a durable
  event (Step 9's producer runs in notifications-svc) and assert the frame arrives at
  the client connected to gateway-svc. Fails if `GATEWAY_EDGE_ADDR`, the inbound face,
  or `deliver_all` is wrong.
- `[PS4]` presence: two clients bind; the second's arrival delivers
  `push.presence{online:true}` to the first; its disconnect delivers `online:false`.
- `[PS5]` **best-effort proof**: with **no** client bound anywhere, the domain
  operation still succeeds and the inbox row is present — a front door with nobody
  listening must never affect a domain call. (The unreachable-*gateway* case is proven
  in Step 11 without a fleet, where it can be forced deterministically.)
- `[PS1m]`/`[PS3m]`: the same on the monolith, where the sink is **local** rather than
  fanned out — stated explicitly, because the monolith has no `GATEWAY_EDGE_ADDR` and
  no `PushStub`, so this pair proves plane parity, not fan-out parity.

Extend the fleet-drift preflight if it enumerates ports.

---

## Step 14 — acceptance `[inline]`

Respecting one-rollout-at-a-time (`pgrep -x cargo; pgrep -x rustc`, then
`cargo run -p devctl -- status`): `cargo test -p edge`, `cargo test -p push -p remote`,
then `cargo run -p verifyctl -- --fast`. Report failures with output; do not fix in a
loop ([[live-acceptance-report-dont-fix]]).

---

## Step 15 — documentation `[docs]`, `model:"sonnet"`

Exactly: `CLAUDE.md` (a third plane exists; `app::run` arity; gateway is no longer
"dials only"), `docs/reference/module-reference.md` (how a module sends a push),
`docs/roadmap/feature-tracker.md` (new row + "Last update" stamp), and a new
`docs/reference/push-plane.md` (envelopes, the three-way ack, re-verification, the
drop-oldest policy, operator env). Against landed code only. Record the carried gaps:
the C# fixture is RPC-only; presence is front-local with no store; the bind performs no
api-key class check.

---

## Known risks

1. `register_process_describe` is unconditional for an edge-serving process, so
   gateway-svc answers `__describe` with an empty manifest — new wire behaviour,
   verified harmless (no peer fetches the gateway's manifest).
2. **A domain svc now knows a gateway address**, the first inversion of peer wiring.
   With a second front door, Step 8's env must become a set; `PeerAddr.addrs` is
   already a `Vec` (`core/opsapi/src/lib.rs:481-484`), so the model accommodates it.
3. **Each of the 14 domain svcs now runs a health probe against gateway-svc's edge.**
   That is 14 new inbound probe connections on the front door, an inversion the fleet
   has never carried. Watch it in Step 14.
4. The presence emitter is front-local; with more than one gateway it reports only
   same-front players.

---

## Errata — revision 1 → 2

- **E1** (fatal) Registry moved off `PushServer` onto a separate `Arc<PushRegistry>`:
  `core/app` `mem::take`s the shared handle (`core/app/src/lib.rs:882`, `:906`), so the
  module's copy was a husk and every delivery would have silently reached 0.
- **E2** (fatal) Added `set_hook` and `liveness()`: Step 6's presence and Step 5's
  readiness both depended on hooks revision 1 never defined.
- **E3** (fatal) Gateway edge port 9012 → **9013**; `mail-svc` owns 9012
  (`fleet.rs:796`). The "9000-9011" range claim was false.
- **E4** (fatal) Step 8 now names `fleet_tests.rs` (hardcodes the gateway row) and both
  `weles/fleet.*.toml` — without them `verifyctl` goes red and `weles up` collides on
  `:9000`.
- **E5a** `PushVerifier` keeps the three-way result; collapsing `Unavailable` into a
  denial would log every player out on an accounts blip.
- **E5b** The bind performs **no** api-key check: the policy is per-operation and a
  synthetic method would fail `modules/apikeys/src/tests.rs:79`. Recorded as a gap.
- **E6** Sink installed **after** `App::build`, not at `Context` construction —
  `SINK_SLOT` is empty until `init` runs. Revision 1 contradicted itself here.
- **E7** The lease is gone. Revision 1 never re-verified, so revocation was unbounded,
  and it wrongly claimed widening the module-private `SessionVerifier` would cost a
  public-api/contract-golden change. Server-side re-verification replaces it, which
  also removes the whole inbound-frame path.
- **E8** Writer loop is a `select!` (notify / re-verify tick / closing) with each write
  bounded by `PUSH_WRITE_GRACE`. Revision 1 could be parked forever by a peer granting
  no flow-control credit, while lease checks living in that loop never ran.
- **E9** Corrected the false claim that the module "never holds a server" — it does,
  under the same carve-out `player_edge` uses.
- **E10** `Push::send` no longer awaits the network: `PushSender` is a bounded queue
  drained by a background task owned by a `PushStub` module with a real `stop`.
  Revision 1 coupled up to 2s of dead-peer timeout into a domain request path and
  leaked every pool at shutdown.
- **E11** `Sink::deliver` returns `Result<usize, _>` so "reached zero fronts" is
  distinguishable from "delivered"; `push_server` with an empty `PUSH_EDGE_ADDR` now
  fails startup. Decision 5 was otherwise unreachable in every shipping process.
- **E12** Added test Steps 11–12 (Steps 3–9 had none), split Step 7 into a mechanical
  sweep and an `[opus]` wiring step, gave `[PS3]` a real producer (Step 9) instead of
  test-lane production code, and named `[PS5]`'s mechanism.
- Minor citation corrections: `player.rs:67` (idle), `:246` (window), `:629`
  (client_tls_public call), `:537-570` (`serve_stream`), `:545` (`limiter.allow`),
  `verifier.rs:38-41`, `remote:733` (`InstanceFactory`), `opsapi:481-484`.
