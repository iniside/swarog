# Push hub — server→client delivery over `GET /push`

The push hub is a WebSocket transport on the gateway's existing HTTP front door,
carrying a SignalR-shaped hub model. It is best-effort, not durable: there is no
checkpoint, no redelivery and no message id. Audience: someone implementing a
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
{"type": "message", "topic": "notifications.new", "payload": "<base64>"}
```

**`payload` is base64-encoded (standard alphabet)** — the producer's bytes are
opaque to every layer between it and the client, so they travel in the one
JSON-safe encoding that survives non-UTF-8 content. This is a client contract
documented nowhere else.

The connection ends with a typed close naming why and whether retrying helps:

```json
{"type": "close", "code": "session_expired", "retryable": true, "reason": "..."}
```

`code` is one of `unauthorized`, `api_key`, `unavailable`, `handshake_timeout`,
`protocol`, `capacity`, `replaced`, `session_expired`, `shutdown`. Only
`unauthorized`, `api_key`, `protocol` and `replaced` are non-retryable — a
retry there reproduces the same refusal (`replaced` is deliberately
non-retryable even though a retry would succeed: the evicted device
reconnecting would evict the next-oldest, and two devices over the cap would
evict each other forever). The client's contract is `retryable`, not the
`code` string itself.

Live, an authenticated connection may send:

```json
{"type": "join", "group": "<name>"}
{"type": "leave", "group": "<name>"}
```

An unrecognized `type` on a LIVE connection is ignored (a newer client
speaking a verb this build lacks stays connected); only the handshake frame
must match this build's grammar exactly.

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

Each connection has a bounded queue (`PUSH_QUEUE_DEPTH`, default 64) that
drops its OLDEST pending frame when full, and counts the drop — never
backpressure, and never a `tokio::mpsc` (which cannot drop from the head). The
durable copy of anything that matters lives in the producing module's own
tables (e.g. the notifications inbox); a push message only ever says
"something changed."

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
| `PUSH_QUEUE_DEPTH` | 64 | Frames one connection may have pending before drop-oldest kicks in. |
| `PUSH_MAX_FRAME_BYTES` | 32768 (32 KiB) | Inbound message/frame cap applied to the upgrade itself. |
| `PUSH_HANDSHAKE_TIMEOUT_MS` | 10000 | How long a socket may stay unauthenticated after the upgrade. |
| `PUSH_WRITE_TIMEOUT_MS` | 10000 | Bound on one outbound write, so a peer that stops reading can't pin the task. |
| `PUSH_REVERIFY_INTERVAL_MS` | 60000 | How often the bind-time bearer is re-checked (and a liveness ping is written). |
| `PUSH_MAX_STALE_MS` | 900000 (15 min) | How long a connection may run while re-verification answers `unavailable`. |
| `PUSH_PRESENCE` | off | Whether a bind/last-disconnect broadcasts `push.presence` (1/0, true/false, on/off, yes/no). |
| `TRUSTED_PROXY_CIDRS` | empty | Shared with `core/app`'s rate limiter — the proxy set the per-IP cap resolves a client address against; an `X-Forwarded-For` from an untrusted peer is ignored. |

## Carried gaps

- **The C# fixture is RPC-only** — it does not exercise `/push`.
- **`/push` checks API-key presence and validity, but not policy** — the key
  mode used here is `KeyCheck::PresenceOnly`, since a fixed route has no wire
  method to match a policy against.
- **Per-message traffic is unmetered by the HTTP rate limiter**, which charges
  only the upgrade once; the per-connection queue depth and the aggregate
  connection caps above are the only bounds on what flows after that.
- **A fixed `/push` route is invisible to `routecheck`** (it only models
  `#[http]`-declared ops), so a future op landing at the same path would
  shadow this route with no gate noticing.
