# Push hub — server→client delivery over `GET /push`

The push hub is a WebSocket transport on the gateway's existing HTTP front door,
carrying a SignalR-shaped hub model. Server→client delivery is best-effort, not
durable: there is no checkpoint, no redelivery and no message id. Client→server,
the same socket can call player ops (`invoke`/`result`, below). Audience: someone implementing a
`/push` client, or operating this in production. Model: `core/push`
(`ConnId`/`Target`/`Message`/`Sink`, transport-free). Transport + registry:
`modules/gateway/src/push_ws.rs`. Backplane: `core/remote/src/push_backplane.rs`.

## Connecting and the frame grammar

`GET /push` upgrades to a WebSocket. A browser cannot set headers on a
WebSocket dial, so the bearer (`Authorization`) and API key (`X-Api-Key`) are
read from the HTTP headers when present, and from the connection's first
frame otherwise. **Never from the query string** — it lands in access logs,
proxy history and browser history, and this transport does not parse one.

The client's first frame must be:

```json
{"type": "hello", "token": "<bearer>", "api_key": "<key>"}
```

(either field may be omitted if it already arrived as a header). Any other
frame type as the first frame is a protocol violation and closes the
connection.

On success the server replies:

```json
{"type": "ack", "connection_id": 42}
```

Thereafter the server delivers:

```json
{"type": "message", "hub": "notifications", "topic": "notifications.new", "payload": "<base64>"}
```

`hub` is the topic's prefix before its first `.` (`""` when there is none); it
is derived by the gateway and never travels on the backplane.

**`payload` is base64-encoded (standard alphabet)** — the producer's bytes are
opaque to every layer between it and the client, so they travel in the one
JSON-safe encoding that survives non-UTF-8 content. This is a client contract
documented nowhere else.

The connection ends with a typed close naming why and whether retrying helps:

```json
{"type": "close", "code": "session_expired", "retryable": true, "reason": "..."}
```

`code` is one of `unauthorized`, `api_key`, `unavailable`, `handshake_timeout`,
`protocol`, `capacity`, `replaced`, `session_expired`, `shutdown`, `bad_frame`,
`frame_too_large`. Only `unauthorized`, `api_key`, `protocol`, `replaced` and
`bad_frame` are non-retryable — a
retry there reproduces the same refusal (`replaced` is deliberately
non-retryable even though a retry would succeed: the evicted device
reconnecting would evict the next-oldest, and two devices over the cap would
evict each other forever). The client's contract is `retryable`, not the
`code` string itself.

Live, an authenticated connection may send:

```json
{"type": "join", "group": "<name>"}
{"type": "leave", "group": "<name>"}
{"type": "invoke", "id": 7, "method": "storage.get", "payload": {"...": "..."}}
```

An unrecognized `type` on a LIVE connection is ignored (a newer client
speaking a verb this build lacks stays connected); only the handshake frame
must match this build's grammar exactly. The exception is a frame whose `type`
is `invoke` and that is malformed (see below), which closes `bad_frame`.

## Invoke — calling a player op over the socket

`invoke` carries `id` (an `i64` the client picks), `method` (the op's wire
method, e.g. `storage.put`) and an optional `payload` JSON value (absent means
`{}`). The server answers with one `result` frame bearing the same `id`:

```json
{"type": "result", "id": 7, "response": {"status": "Ok", "err": "", "value": {"...": "..."}}}
```

`response` is the player-QUIC envelope verbatim (`status`, `err`, `value` on
success). Text and binary frames are both parsed. Up to
`PUSH_INVOKE_MAX_IN_FLIGHT` invokes run concurrently, so results arrive in
completion order, not request order — correlate by `id`.

**Which ops.** Exactly the `#[http]`-bound player ops — the same allow-list as
player-QUIC, matched by exact wire method. Any other method (including a
wire-only op) answers `NotFound` "unknown operation".

**Admission, per invoke.** Each invoke is admitted with the bearer and API key
the connection bound with, through the same `handle_player` path as player-QUIC:
the API key's **policy** is checked for that method, then one `Sessions` lookup
validates the bearer, then the op is dispatched Local or Remote. A session
revoked after the bind is therefore refused on the very next invoke, without
waiting for the re-verify tick. A payload that is not valid JSON answers `Invalid`.

**What happens to every inbound frame, in order:**

1. It is charged to a per-connection token bucket (`PUSH_INBOUND_RPS` /
   `PUSH_INBOUND_BURST`) before any byte is parsed. Over budget, the frame is
   dropped silently — an over-budget `invoke` gets no `result`, so the client
   must stay under the rate.
2. Its `type` is probed. A non-`invoke` frame (the group verbs) is charged to
   `MAX_VERBS_PER_INTERVAL` (64 per re-verify interval; over it, dropped) and
   applied.
3. An `invoke` is charged to the per-client-IP limiter shared by every
   connection from that IP (`PUSH_INVOKE_IP_RPS` / `PUSH_INVOKE_IP_BURST`),
   then parsed. A frame that is not `{id: i64, method: string, payload?}`, or
   whose `method` exceeds 128 bytes, closes `bad_frame` (non-retryable).
4. If the connection already has `PUSH_INVOKE_MAX_IN_FLIGHT` invokes
   outstanding, it is refused; otherwise it is dispatched.

**Refusals are results, not drops.** An invoke refused by the per-IP limiter
gets `Unavailable` "rate limited"; one refused by the in-flight cap gets
`Unavailable` "too many in-flight invokes". A client that lets
`PUSH_INVOKE_MAX_IN_FLIGHT` such refusals sit unread and then keeps being
refused is closed `capacity` (retryable).

**Failure results.** An invoke still running after `PUSH_INVOKE_TIMEOUT_MS`
gets `Unavailable` "invoke timed out" (the op may or may not have committed). An
op that panics gets `Internal` "invoke failed"; the connection stays open.

**Payload ceiling.** The cap is `PUSH_MAX_FRAME_BYTES` (default 128 KiB) on the
whole ESCAPED frame, not the logical payload: a JSON string field containing
quotes or backslashes grows 2x, and up to 6x under `\u` escaping, so a payload
that fits HTTP or player-QUIC (1 MiB) may not fit here. A frame over the cap
closes `frame_too_large` (retryable); send large requests over HTTP or QUIC.

**Graceful stop does not drain invokes.** On shutdown the `shutdown` close is
written and in-flight invokes are aborted; their results are never sent, so a
mutation's outcome is ambiguous to the client. Reconnect and re-read. There is
no invoke resume or replay after a dropped socket either.

## `notifications.new` — the nudge, not the change

`notificationsapi::PUSH_NEW_TOPIC` (`"notifications.new"`) is a hint: "you
have new mail, refetch your inbox list." It carries no row id and cannot: the
send happens inside the durable event handler, before the checkpoint update
and the commit — there is no post-commit hook to move it behind. An id would
sometimes name a row a rollback plus at-least-once redelivery never commits.

This also means a nudge can lose a race with its own commit: a client that
calls `POST /notifications/list` the instant it receives the frame may read a
snapshot from before that transaction commits, see nothing, and never be
nudged again for that event (redelivery only happens when the handler fails,
and this one succeeded). **A nudge is a hint to re-read, not a promise the
read will differ** — a client must tolerate an empty refetch, fall back to its
ordinary poll, and must never treat the nudge itself as an unread-count
increment.

## Connection identity

`connection_id` is minted by the server per connection (`push::ConnId`),
process-local, and does not survive a reconnect — a reconnect is a new
connection with a new id. Nothing addressable (`Target`) is keyed by it.

## Groups

A `join`/`leave` adds or removes the connection from an ephemeral, per-process
set (`push::Target::Group`). Membership dies with the connection: it is not
restored on reconnect, and the client is responsible for rejoining every group
it cares about after one. **A join is not permission-checked** — any
authenticated connection may join any group name, so a group addresses an
audience, never a privilege. Nothing only some players may see may be
addressed by group alone.

## Presence

`PUSH_PRESENCE` (default off) makes a player's first bind and last unbind
broadcast `push.presence` (`{"player_id": ..., "online": bool}`) to
`Target::All`. It is front-local only: a multi-gateway fleet gets one
independent presence view per front, with no shared store, because a global
view needs a friends graph (to narrow the broadcast audience) and persistence,
both out of scope here. The transition itself is decided atomically under the
same registry guard that performs the bind/unbind, but the two announcements
(`online`/`offline`) are enqueued independently afterwards — a disconnect
racing the same player's reconnect can still deliver them to an observer in
either order, and the frames carry no sequence number or timestamp a client
could use to reorder them.

## The outbound queue

Each connection has two kinds of lane, never a `tokio::mpsc` (which cannot drop
from the head):

- a **reply lane** (the `ack` and every `result`), which is never evicted: its
  contents are bounded by the invoke admission above;
- **event lanes**, one per hub, for `message` frames. At most 16 lanes exist,
  including the shared lane `""`; frames of any hub beyond the first 15 named
  ones share lane `""`.

`PUSH_QUEUE_DEPTH` (default 64) is the TOTAL across event lanes. When it is
reached, the OLDEST frame of the LONGEST lane is evicted (ties go to the lowest
lane index) and the drop is counted — never backpressure — so a flood on one hub
cannot evict another hub's frames until that hub's lane is the longest. The
writer takes a pending close first, then the reply lane, then the event lanes
round-robin. The durable copy of anything that matters lives in the producing
module's own tables (e.g. the notifications inbox); a push message only ever
says "something changed."

## Re-verification

A live connection's bind-time bearer is re-checked on every
`PUSH_REVERIFY_INTERVAL_MS` tick (default 60s). An expired token and a revoked
session are indistinguishable to this check, so the close is `session_expired`
and marked **retryable** — the client is expected to refresh its credential
and reconnect (re-binding, since connection identity and group membership do
not survive a reconnect anyway). A verifier that cannot answer at all
(`unavailable`) keeps the connection running rather than closing it, up to
`PUSH_MAX_STALE_MS` (default 900s / 15 minutes) since the last successful
check — an accounts outage must not log every player out at once.

## Operator environment (`PushLimits::from_values`)

All of the following are read once, in `cmd/server`'s and `cmd/gateway-svc`'s
`main.rs`, and passed to the module — never read by the module itself. A name
that is **absent or blank** keeps the build's default; a name that is
**present but unusable** (unparseable, or `0` for a cap/deadline where zero
cannot mean "disabled") **fails startup**, naming the offender — this is
deliberately stricter than `admission_budget_from_value`'s precedent of
silently falling back to a default.

| Env var | Default | Bounds |
|---|---|---|
| `PUSH_MAX_CONNECTIONS` | 10,000 | Sockets this process serves at once (authenticated or still handshaking). |
| `PUSH_MAX_CONNECTIONS_PER_IP` | 64 | Sockets one resolved client IP may hold. |
| `PUSH_MAX_CONNECTIONS_PER_PLAYER` | 8 | Sockets one player may hold across devices; the oldest is evicted (`replaced`) past this. |
| `PUSH_QUEUE_DEPTH` | 64 | Event frames one connection may have pending, total across its hub lanes, before the longest lane's oldest frame is evicted. |
| `PUSH_MAX_FRAME_BYTES` | 131072 (128 KiB) | Inbound message/frame cap applied to the upgrade itself; over it the connection closes `frame_too_large`. Applies to the escaped frame. |
| `PUSH_HANDSHAKE_TIMEOUT_MS` | 10000 | How long a socket may stay unauthenticated after the upgrade. |
| `PUSH_WRITE_TIMEOUT_MS` | 10000 | Bound on one outbound write, so a peer that stops reading can't pin the task. |
| `PUSH_REVERIFY_INTERVAL_MS` | 60000 | How often the bind-time bearer is re-checked (and a liveness ping is written). |
| `PUSH_MAX_STALE_MS` | 900000 (15 min) | How long a connection may run while re-verification answers `unavailable`. |
| `PUSH_INBOUND_RPS` | 10 | Per-connection refill rate (frames/s) of the bucket every inbound frame is charged to before parsing. |
| `PUSH_INBOUND_BURST` | 20 | Capacity of that per-connection bucket. |
| `PUSH_INVOKE_MAX_IN_FLIGHT` | 16 | Invokes one connection may have dispatched and not yet answered; also the unread-refusal count that closes `capacity`. |
| `PUSH_INVOKE_TIMEOUT_MS` | 30000 | Bound on one invoke's dispatch, after which it answers `Unavailable`. |
| `PUSH_INVOKE_IP_RPS` | 20 | Per-client-IP refill rate (invokes/s), shared by every connection from that IP. |
| `PUSH_INVOKE_IP_BURST` | 40 | Capacity of that per-IP bucket. |
| `PUSH_PRESENCE` | off | Whether a bind/last-disconnect broadcasts `push.presence` (1/0, true/false, on/off, yes/no). |
| `TRUSTED_PROXY_CIDRS` | empty | Shared with `core/app`'s rate limiter — the proxy set the per-IP cap resolves a client address against; an `X-Forwarded-For` from an untrusted peer is ignored. |

## Carried gaps

- **The C# fixture is RPC-only** — it does not exercise `/push`.
- **The handshake checks API-key presence and validity, but not policy** — it
  uses `KeyCheck::PresenceOnly`, since the fixed route has no wire method to
  match a policy against. Policy is enforced per `invoke`; a connection that
  never invokes is never policy-checked.
- **The HTTP rate limiter charges only the upgrade once.** After it, inbound
  frames are bounded by the per-connection bucket (and, for invokes, the per-IP
  limiter); outbound frames are bounded only by the queue depth and the
  aggregate connection caps above. The per-IP invoke limiter's table-full
  refusals are counted in `http_rate_limit_table_saturated_total{limiter="push_invoke"}`.
- **After `frame_too_large` the protocol-level 1009 close can be lost** (the
  peer may see 1006); the typed JSON `close` frame still arrives.
- **No fuzz target covers the `/push` frame parser.**
- **A fixed `/push` route is invisible to `routecheck`** (it only models
  `#[http]`-declared ops), so a future op landing at the same path would
  shadow this route with no gate noticing.
