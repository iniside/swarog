# Push hub — server→client delivery over WebSocket, SignalR-shaped

**Revision 4 (2026-09-04) — deliberately cut back.** Revisions 1–3 specified at
implementation resolution (lock protocols, wake semantics, per-field constants) and each
review round found more detail-level defects, most of which a compiler would have caught in
seconds. This revision keeps only what the compiler and the test-author **cannot** produce
on their own: the decisions, the step order, and the hand-maintained authorities that go red
silently. Implementation detail is the implementer's job, with a compiler.

History: supersedes [`2026-09-02-2242-push-plane-plan.md`](2026-09-02-2242-push-plane-plan.md)
(a bespoke third QUIC plane, abandoned). Two review rounds on revisions 1–3 produced 39
findings; the ones that survive as constraints are folded into the steps below rather than
kept as errata.

---

## The decision

**A WebSocket route on the gateway's existing HTTP front door, carrying a SignalR-shaped hub
model, with a backplane that fans out over the internal mTLS edge we already have.**

Rejected alternatives and why, in one line each:
- *A third QUIC plane* — a new public port, new ALPN, hand-written clients in Rust and C#, and
  a drain contract the existing one does not have.
- *WebTransport* — its Rust crates are experimental, .NET has no production client, and the
  spec is still a draft. The seam below admits it later as a second transport.
- *Extending the player-QUIC plane* — it is strictly client-initiated and its guards assume
  short work; a long-lived stream breaks its drain.
- *A durable-plane or invalidation-based fan-out* — one is at-least-once with a checkpoint
  (meaningless for a socket that may be gone), the other is a payload-less broadcast; and
  `cmd/gateway-svc` runs `.without_db()`, so it hosts neither.
- *A `player_id → node` directory* — unnecessary: every front filters locally, which keeps the
  design neutral to a multi-gateway fleet.

**From SignalR we take:** a server-minted connection id that does **not** survive a reconnect;
a handshake as the first message; `Target::Player | Group | All` addressing, where `Player` is
a fan-out over that player's devices; groups as ephemeral, in-process, host-owned sets the
client rebuilds after a reconnect; a one-way message with no id; ping and a typed close; and
the lifetime-manager split (answer locally if this process owns the connection, else fan out).

**We do not take:** negotiate (and with it sticky sessions), the invocation/completion/streaming
message family (that is what our RPC seam is for), or a Redis backplane.

---

## Constraints the implementation must honour

These are the review findings worth keeping. Each is a decision or a trap, not a detail.

1. **Bounds are mandatory, and they need an injection path.** Caps on total connections, per
   client IP, per player, per-connection queue depth, inbound frame size, and the handshake /
   write / re-verify / max-stale deadlines. `core/app` does not thread config into modules —
   env is parsed in **`cmd/gateway-svc/src/main.rs` and `cmd/server/src/main.rs`** and carried
   to the module either through the typed `ProcessWiring` (the `CREDENTIAL_ADMISSION_TIMEOUT_MS`
   precedent: parsed at `cmd/server/src/main.rs:18`, stored via
   `ProcessWiring::with_admission_budget`, applied by the builder at
   `cmd/server/src/lib.rs:37-39`) or as an explicit `gateway_svc::modules` parameter.
   **Never in a `cmd/*/src/lib.rs`** — `tools/checkmodules` calls those with an empty wiring
   (`tools/checkmodules/src/lib.rs:29`), so an env read there would make routecheck/topiccheck/
   requirecheck model the developer's ambient shell.
2. **The per-IP cap must resolve the client IP against the trusted-proxy set**, which lives in
   `core/app` and must be threaded in alongside the caps. An untrusted peer's `X-Forwarded-For`
   is ignored — otherwise a forged header defeats the cap entirely. **Do not copy
   `modules/admin/src/lib.rs:366`**, which reads `TRUSTED_PROXY_CIDRS` from env inside a module;
   `httpmw::parse_cidrs` being importable does not make that the right seam.
3. **One credential authority.** `/push` uses the same bearer path as ops, under the same
   admission budget applied at the call site. `crate::authenticate` — a second bearer rail with
   no timeout, used only by the conformance probe — is **deleted**, and that probe re-pointed.
4. **The api key is required for presence and validity, not for a policy match** — and this
   mode is added **inside** the existing authority, not beside it. A fixed route has no wire
   method, so `admit`/`admit_inner`/`check_api_key` grow a presence-only mode sharing the one
   timeout and the one `AdmissionDenial` mapping. A separate `push_admit()` would re-create the
   second rail constraint 3 exists to delete. Nothing is added to `DEV_CLIENT_POLICY` (it is
   pinned against the op catalog by `modules/apikeys/src/tests.rs:79`).
5. **`Unavailable` never collapses into a denial.** An accounts outage must close with a
   retryable code, not an auth rejection — otherwise a blip logs every player out at once.
6. **Re-verification is not optional.** Without a periodic re-check, `/push` becomes the only
   public surface where a revoked session survives. An expired token and a revoked session are
   indistinguishable, so the close is **retryable** and a genuinely revoked session is stopped
   by the 401 at re-bind.
7. **Ordering must not diverge by topology.** The backplane sends an ordered batch in one call
   rather than parallel per-message calls; otherwise frames invert in split and never in the
   monolith.
8. **The notification carries no row id.** The send happens inside the durable handler (there is
   no post-commit hook), so a rollback plus at-least-once redelivery would push an id that never
   commits. `notifications.new` means "refetch the list".
9. **Drop-oldest, not backpressure**, on the per-connection queue — the durable copy is in the
   inbox, and a stalled reader must never grow our memory. (A `tokio` `mpsc` cannot do this.)
10. **The upgraded socket outlives the HTTP drain** — `WebSocketUpgrade::on_upgrade` spawns a
    **detached** task and drops its `JoinHandle`, so the module cannot abort axum's task and
    "abort on stop" is not available for it. The connection work must run in a task the module
    spawns *inside* that callback, holding its own cancel handle; `stop` flushes a typed close
    within a bounded grace, re-derived against `MODULE_STOP_GRACE_MS`, then cancels.
11. **Groups are not authorization**, membership dies with the connection, and the client rejoins
    after a reconnect.
12. **The nudge can never fail a durable handler.** `modules/notifications/src/service.rs:268`
    is the single insert authority, and its own doc (`:258-267`) records the rule: a handler
    returning `Err` backs off and **pauses the whole subscription**. So the push call is a
    non-blocking try-send that drops-and-counts when the queue is full and returns no error
    upward — otherwise a gateway outage takes every player's inbox writes offline until an
    operator runs `eventctl`. It must also never await a network round-trip: it runs on the
    delivery transaction's connection.

---

## Hand-maintained authorities that will go red

Not compiler-catchable, and each has bitten a previous revision.

| Authority | Why it breaks |
|---|---|
| `tools/archcheck/src/main.rs` `SLOT_OWNER_FILES` + `tools/archcheck/src/tests.rs` | a new `contrib::Slot::new` outside the fixed-size allow-list; the test pins its length |
| `tools/checkmodules` + `cmd/gateway-svc/tests/boots.rs` | call `gateway_svc::modules`, whose signature grows in **Step 4** |
| `tools/processctl/src/fleet.rs` + `fleet_tests.rs` | the gateway row pins `edge_port: None` |
| `weles/fleet.split.toml` | no `EDGE_ADDR`, so `weles up` would bind the default `:9000` and collide |
| `weles/master/src/manifest_tests.rs` | pins each service's exact composed env set |
| `tools/conformance/src/policy.rs` | the gateway's `InputByteCaps` `NotApplicable` reason becomes false once it owns attacker-supplied fields |
| `tools/splitproof/Cargo.toml` | needs its own `tokio-tungstenite`; it is in the tree only transitively |

**One authority that will *not* go red, and therefore needs a deliberate edit:**
`tools/routecheck/src/main.rs` hardcodes gateway-svc as edge-less (`:228`, the union at
`:344-366`, the doc comments at `:53-59` and `:218-228`). Because gateway-svc contributes no
`opsapi::SLOT` ops, its `edge_methods` feeds no assertion, so nothing breaks — and left stale
the gate would permanently model the gateway's internal edge as empty, hiding a future
duplicate or unserved wire method there. Step 6 updates it on purpose, not under compiler
pressure.

Untouched, and stated so it is not re-litigated: `topiccheck`, `public-api`, `contract-golden`,
`codegen-freshness`, `admincheck`, the C# goldens, `PG_SESSION_BUDGET`.

---

## Steps

Each step names what it touches and what it must not break. Signatures, lock choices and
constants are the implementer's, subject to the constraints above.

**Step 1 — `core/push`: the hub model.** `[opus]` core-implementer.
A new foundation crate: `ConnId`, `Target`, `Message`, the batch codec, the `Sink` trait, and
the `Push` handle with its `SINK_SLOT`. Depends on `contrib` + serde only — never `api/*`,
never `core/edge`. The codec lives here because `core/remote` encodes and `modules/gateway`
decodes; two definitions would drift. Absent sink ⇒ a typed error logged loudly, never silent,
never a panic. Also edits the archcheck allow-list and its length test.

**Step 2 — `Context` and `app::run`.** `[opus]` core-implementer.
`ctx.push()` as an always-present handle (the `invalidation` shape), installed **after**
`App::build` from the `SINK_SLOT` contribution — contributions do not exist before then.
**At most one**: zero is legal and leaves Step 1's logged-error sink (most split processes
contribute none, and none contributes one until Step 4 lands); two is a loud startup failure.
One installation authority, not two.

**Step 3 — `core/remote`: the backplane sender.** `[opus]` core-implementer.
A fan-out-to-all-resolved-instances call beside the existing round-robin (it must live inside
the crate — the instance types are private), plus a small `Module` that owns a bounded queue and
its drain task and stops them. The drain sends **one ordered batch per pass** (constraint 7) and
must distinguish "pool never resolved" from "pool resolved to no addresses" so startup does not
discard the first messages. It must not copy `remote::Stub`'s `PEER_SLOT` contribution.

*Errata (2026-09-03, review rounds 1–2 on `55809eb`/`95f8188`):* this step originally said
*selectable* instances and *"resolved, nothing alive"*. Both were reversed during review and the
text above now describes the code. A probe verdict may not decide WHETHER a best-effort message
is attempted — `probe_peer`'s 1s dial permanently condemns a front that handshakes in 1.2s but
serves `push.deliver` perfectly, and an unattempted send is more fail-closed than the request
path this borrows from. Health instead sets the per-instance DEADLINE (`FANOUT_CALL_TIMEOUT` vs
the much shorter `FANOUT_UNHEALTHY_TIMEOUT`), which keeps the unconditional attempt without
letting one listed corpse — `join_all` completes on the slowest, batches are awaited
sequentially — tax every batch to every live front until the queue overflows. Consequently
`FanoutState` has no "resolved, nothing alive" variant: that is `Ready(n)`, and only a resolve
returning zero addresses (`Empty`) short-circuits.

**Step 4 — gateway: the WebSocket transport.** `[opus]` core-implementer.
Enable axum's `ws` feature; add `GET /push` ahead of the gateway's fallback; handshake with the
bearer from either the header or the first frame (never the query string); the connection
registry and its per-connection tasks; the limits builder and its two composition-root call
sites. Carries constraints 1–6, 9, 10.

*Errata (2026-09-04, implementation + review round 1 on `5e19621`):* three decisions this
step made that the text above does not describe.
- **The presence-only key mode is a parameter, not a second body.** The first landing added
  `admit_push`/`admit_push_inner` beside `admit`/`admit_inner` — substantively one authority
  (one key check, one `verify_bearer`, one `AdmissionDenial` map) but structurally a copy,
  with the budget wrapper in three places and nothing pinning the two bodies equal. It was
  folded back into constraint 4's shape: `keys::KeyCheck::{Policy,PresenceOnly}` threaded
  through `check_api_key` → `admit_inner` → `admit`, and `/push` calls `admit`. The one
  extraction that stayed is `verify_bearer`, the free function `admit_inner` and the
  re-verify tick share (constraint 3's deleted `authenticate` is what it replaces).
- **`reverify_push` is a SECOND budget site,** deliberately: there is no key check to share
  a deadline with, and re-running one would consult the apikeys store once per tick per
  connection for a decision already made at admission.
- **The `PUSH_*` PARSE policy lives in `gateway::PushLimits::from_values`,** not in the two
  mains. Env is still read only in `cmd/server/src/main.rs` and `cmd/gateway-svc/src/main.rs`
  (one line each, over the `pub const` knob names), but the nine knobs' fail-startup rules
  are one value-taking function rather than a 50-line block copied into both roots with
  nothing detecting drift — and, unlike `admission_budget_from_value`, it is reachable in a
  test without mutating process env. Its policy diverges from that precedent: garbage FAILS
  STARTUP here rather than falling back to the default.

**Step 5 — gateway: the hub.** `[opus]` core-implementer.
`Target` resolution over the registry, client-driven group join/leave, and the presence emitter
(default off — it is O(connections) per event and exists as the acceptance vehicle, not a
feature). Carries constraint 11.

**Step 6 — the inbound face and the fleet.** `[opus]` core-implementer.
The gateway contributes a `push.deliver` edge face unconditionally via `EDGE_SLOT`;
`cmd/gateway-svc` starts serving an internal edge; `notifications-svc` gets the sender stub and
the gateway peer address. Every authority in the table above is updated here except archcheck
(Step 1). No domain service declares a fleet dependency on the gateway — push is best-effort and
the fleet forbids the cycle anyway.

**Step 7 — notifications: the first producer.** `[opus]` core-implementer.
All three insert paths funnel through one helper that inserts and then nudges. Carries
constraint 8.

**Step 8 — unit tests: model and backplane.** `[test-author]`, `model:"sonnet"`.
Covers Steps 1–3. Must execute, at minimum: `Push::install` panicking on a second call;
the latched `NoSink` log (first `error!`, then `debug!`) and its `Err`; `encode_batch`/
`decode_batch` preserving order; and `app::select_push_sink` over 0, 1 and 2 sinks — the
two-sink `bail!` is otherwise reachable only by booting a real fleet.
Plus the branches Step 3 (`55809eb`, `95f8188` and its round-2 follow-up) made once-wrong, all
in `core/remote` and all reachable with the existing `Pool::with_factory` fake-instance fixture
plus a paused clock:
- `Pool::fanout_state` answering `Unresolved` before any resolve versus `Empty` after a resolve
  that returned no addresses — reachable only by driving `refresh_once`, whose resolver-`Err`
  early return never stamps `applied_gen` and is the trap.
- `push_backplane::split_batch` at the byte boundary — an element that exactly fits, an element
  that cannot fit alone (dropped and counted, not carried forward), and order preserved across
  the resulting chunks.
- `Pool::deliver_all` ATTEMPTING an instance that is not `is_selectable`, with the short
  `FANOUT_UNHEALTHY_TIMEOUT` rather than the full one. Nothing else pins this, so a later reader
  "restores" the health gate and silently re-opens the condemned-slow-front bug.
- The `resolve_waited` latch: the second batch must not wait again. A regression to the
  per-batch wait is invisible except as a 5s-per-batch throttle.
- `send_batch` returning `false` on a stop observed DURING `deliver_all`, and `drain_loop`'s
  `record_stop_loss` counting the queue behind it — the counted-exit property. Asserting only
  that the drain ends would pass from the abort path this replaced.

**Step 9a — `core/app`: the upgrade survives the layer stack.** `[test-author]`, `model:"opus"`.
The plan's one unproven claim — that the whole-request timeout bounds response-*start* and not
the upgraded socket. It must run against the **real** `app::run` assembly; `modules/gateway`'s
`oneshot` harness carries no upgrade extension, so no upgrade can happen there at all.

**Step 9 — unit tests: the WebSocket hub.** `[test-author]`, `model:"opus"`.
Covers Steps 4–5 and 7, against a real bound server. From Step 4's review, these branches
are known-unexecuted and must be included: the **post-grace abort collection** (a connection
whose task attaches its handle after `closing` is set and then fails to drain — the leak that
review found); the `Notified::enable` window (a slot released between the `live()` check and
the await); `KeyCheck::PresenceOnly` vs `Policy` inside the one `check_api_key` (a key whose
policy names nothing is admitted on `/push` and `Forbidden` on an op); and
`PushLimits::from_values` over unset → default, `"0"` → error naming the var, garbage →
error, valid → parsed, malformed CIDR → error (a value-taking parser, so no test touches
process env). Must also execute, at minimum, the branches
behind constraints 2, 5, 6, 9 and 10 — a forged `X-Forwarded-For`, an unavailable verifier, a
revoked session on the re-verify tick, queue overflow, and a **typed close** observed by the
client (asserting merely that connections end would pass from the cancel alone). Plus the
branch that is classically wrong in a cap: the **release** path — a connect→disconnect→reconnect
cycle at the boundary, for both the global and the per-player counter, since a missing or
doubled decrement is invisible to a test that only connects.

**Step 10 — split-proof assertions.** `[test-author]`, `model:"opus"`.
`[PH1]`–`[PH6]` plus a monolith parity pair. The load-bearing one is the cross-process proof: a
durable event in notifications-svc produces a frame at a client connected to gateway-svc. The
monolith pair proves hub parity, not fan-out parity — say so, since it exercises the local sink.

**Step 11 — acceptance.** `[inline]`.
One rollout at a time. `cargo tree -i aws-lc-rs` must be empty, then the touched crates' tests,
then `verifyctl --fast`. Report failures; do not fix in a loop.

**Step 12 — documentation.** `[docs]`, `model:"sonnet"`.
`CLAUDE.md` and `.agents/shared/gamebackend.md` (the `core/*` enumeration, the Layout tree, the
split port list, the gateway description), `README.md`'s layout tree, the stale port comment in
`tools/splitproof`, the two weles comments asserting the gateway serves no edge, the roadmap
tracker, and a new `docs/reference/push-hub.md`.

---

## Carried gaps

The C# fixture stays RPC-only. Presence is front-local, default off, with no store — a global
view needs a friends graph and persistence, both out of scope. Group membership and in-flight
messages are lost on reconnect; the client rejoins. Ordering holds within one connection, not
across a reconnect. `/push` checks api-key presence but no policy. Per-message traffic is
unmetered by the HTTP rate limiter, which charges the upgrade once — the per-connection queue and
the aggregate caps are the only bounds. A fixed `/push` route is invisible to `routecheck`, so a
future `#[http]` op at the same path would shadow it with no gate noticing.
