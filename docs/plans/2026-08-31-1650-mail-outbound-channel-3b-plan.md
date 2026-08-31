# Seq #3b — outbound email channel (`mail`, the 14th fortress)

Revision 2 · 2026-08-31 · sequence row `docs/roadmap/feature-tracker.md:44`

Revision 2 addresses a REJECT verdict from the plan review; the punch list and what
each item changed are recorded in *Review response* at the end.

Delivers the backend's first outbound-to-the-world channel: a durable outbox with
per-row retry, a provider registry (`log` dev sink + real `smtp`), retention, and an
operator surface. It is the hard prerequisite for seq #4 (production self-registration:
email verification + password reset).

---

## Context — the overlapping systems, and why a new fortress

`CLAUDE.md`'s *Research before planning* rule requires this section to lead with the
existing systems this could have been built into, and why each was rejected.

### Why not extend `notifications` (the tracker's current answer)

`docs/roadmap/feature-tracker.md:203` records the outbound email channel as "owned by
the notifications module as an outbound channel". That row predates seq #3a's landing
and is wrong on four counts, each of which is a fortress-shaped seam:

1. **Different key.** `notifications.messages.player_id` is a `uuid` and every op,
   index and authz check is player-scoped. Mail is addressed to a *literal address*
   and — see finding A — this backend stores no address for any player. A mail channel
   keyed by `player_id` cannot deliver a password reset, because the recipient of a
   reset is by definition someone who cannot authenticate.
2. **Different consumer.** #3b's only consumer is `accounts` (#4). Putting the channel
   in `notifications` forces `accounts` to depend on the inbox module's contract for a
   feature the inbox is not part of.
3. **Different failure surface.** `notifications-svc` today makes no outbound network
   call. An SMTP socket, operator credentials, per-recipient backoff and a parked-row
   state machine change what a `notifications-svc` outage means and what its `/readyz`
   asserts.
4. **Different error model** — the reason recorded in the 3a/3b/3c split note itself
   (`feature-tracker.md:246-249`): "outbound I/O to the world with operator secrets and
   a different error model".

Step 13 corrects `feature-tracker.md:203`.

### Why not a sync `mailapi::Mailer` capability

The obvious shape — `accounts` `require`s `dyn Mailer` and calls `send` — was rejected:

- **It cannot be transactional in the split.** The outbox row must be committed in the
  *sender's* transaction, or a crash between commit and send silently drops a
  verification mail. A remote call from `accounts-svc` to `mail-svc` cannot join
  `accounts`' transaction. The durable plane is the only transactional hand-off that
  behaves identically in both topologies.
- **It costs a dependency edge and its whole tail.** `accounts` would gain
  `requires("mail")`, `cmd/accounts-svc` a `remote::Stub`, the fleet a dependency
  ordering, and `accounts-svc` — which today "dials no peer"
  (`cmd/accounts-svc/src/lib.rs`) — its first outbound peer.
- **The result is not needed synchronously.** A verification mail is fire-and-forget;
  the caller has nothing to do with a send outcome it cannot wait for.

Consequence: **`mail` ships no `api/mail/api` crate at all.** `audit` is the shipped
precedent — `api/audit/api` does not exist and `api/audit/rpc/src/lib.rs` is a single
`pub use adminrpc::register_admin;`.

This removes exactly **four** registration points, not more: `opscatalog-gen`,
`csharp-client-gen`'s `PROVIDERS`, `apikeys`' `DEV_CLIENT_POLICY`, and a `public-api`
baseline for an api crate. **It does NOT remove the contract-golden**, which is driven by
`topiccheck::defined_topics()` (`tools/topiccheck/src/golden.rs:16-18`) — the *events*
crate, not the api crate — and enforces that every defined `(topic, version)` has at
least one `golden_samples()` (`golden.rs:402,437`). Step 8 registers it.

### Why not reuse the durable plane's retry for the send itself

`core/asyncevents` already has exponential backoff (1s→5m), a failure counter, and a
pause-at-20 state machine. It is the wrong instrument here, in order of weight:

1. **Its backoff state is per *subscription*, not per item** — `asyncevents.subscriptions`
   is one row per subscription. One undeliverable address would stall the cursor and,
   after `PAUSE_AFTER` failures, pause the subscription: one bad recipient stops all
   mail. Correct for ordered durable delivery; wrong for independent sends.
2. **An SMTP round-trip inside a delivery transaction is exactly the shape the plane's
   own bounds exist to kill** (`core/asyncevents/src/worker.rs:170-179`). The handler
   budget is `ASYNCEVENTS_HANDLER_TIMEOUT` (10s default), and 3a's errata #14 records
   that a module may not read it and `bus::Delivery` carries no deadline — a handler
   cannot size itself against its own budget.
3. **The plane's exactly-once is exactly-once for a *DB effect*.** An SMTP call cannot
   join the delivery transaction, so a redelivery re-sends.

So the plane carries the *request* (enqueue, transactional, at-least-once with an
idempotency key) and the module owns the *send* (claim, attempt, back off, park). The
state machine is **copied** from `worker.rs`, not consumed.

### Why not `config` for the SMTP credentials

`modules/config/src/lib.rs:6-7` says it outright — "Secrets stay in env; only
non-secret operational knobs go here" — and four mechanisms make it structurally
unsafe: values are plaintext `text` in a shared table; the write trigger appends the
**full value** to the durable log as `config.changed`, which `audit` sinks into
`audit.log`; `ConfigSnapshot::snapshot()` ships every setting across the internal edge
unfiltered; and the admin portal renders and edits values. SMTP credentials are env
only, allowlisted into the fleet's typed env map.

### Why the module owns its drain loop rather than reacting to `scheduler.fired`

`audit` and `notifications` prune by reacting to `scheduler.fired{name}`. Schedules are
seconds-granularity and each fire is one delivery — right for a daily sweep, wrong for
"send this the moment it is enqueued". `mail` copies `scheduler`'s own-loop shape
(`modules/scheduler/src/lib.rs`): a `start`-spawned `tokio::time::interval`, a shared
per-pass budget, `catch_unwind` supervision, grace-then-abort in `stop`. Retention,
which *is* a daily sweep, uses the `scheduler.fired` shape (Step 5) — the two cadences
have different owners on purpose.

---

## Findings that shape the design

**A. There is no email address anywhere in this backend.** `modules/accounts/src/lib.rs:103-161`
creates five tables (`players`, `identities`, `sessions`, `refresh_tokens`, `oauth_states`)
and none has an `email` column. What looks like an address today is the `subject` of a
`(provider='dev', subject=<email>)` identity row (`lib.rs:262`), validated only by
`MAX_EMAIL_BYTES = 320` (`lib.rs:59`) — no syntax check, no normalization, so
`Alice@x.com` and `alice@x.com` are two different players. Federated and guest players
have no address in any form. **Therefore `mail` takes a literal `to: String`, never a
`player_id`**; storing and verifying addresses is seq #4's work in `accounts`. A
`player_id`-keyed contract would need `mail` to `require` an address lookup from
`accounts` while `accounts` sends through `mail` — a sync cycle between two fortresses,
which in a split is two processes dialing each other.

**B. The 14th DB-backed process does not fit the Postgres session budget.**
`tools/processctl/src/fleet.rs`: `USABLE_PG_SESSIONS = 97` (`:113`),
`HARNESS_RESERVE = 16` (`:101-106`), so `PG_SESSION_BUDGET = 81` (`:117`). Thirteen
DB-backed services each reserve `SPLIT_SERVICE_POOL_MAX (2) + PLANE_DEDICATED_SESSIONS (4) = 6`,
plus `SCHEDULER_FIRE_SESSIONS (1)` — **79 of 81**. `mail-svc` needs 6 more: 85 > 81, and
`FleetSpec::new` (`:352`) fails closed. `SPLIT_SERVICE_POOL_MAX` is already at
`core/app`'s migrate floor of 2, so the pool cannot absorb it. **`weles` is worse**:
`weles/fleet.split.toml` pins `DATABASE_POOL_MAX_CONNECTIONS = "3"` per DB service and
has no budget validation at all — 14 × (3 + 4) + 1 = **99 > 97 usable**. Step 1 resolves
both.

**C. A provider name and its sender ship in the same commit.** `accounts` learned this
the expensive way (`feature-tracker.md:275-277`): listing `apple` before its verifier
existed handed callers a permanent, operator-unfixable 503. The *mechanism* does not
transfer — `mail`'s provider name comes from `MAIL_PROVIDER` at boot, not from an
anonymous caller over the wire, so there is no 400-vs-503 trichotomy to build and none
is planned. The *rule* does transfer: `smtp` joins `KNOWN_PROVIDERS` in Step 4, the step
that ships its sender, so a boot-time failure never names a provider this build cannot
construct.

**D. `mail` defines a topic it consumes.** This inverts "publisher owns the event,
consumer owns the subscription". It is a deliberate, recorded deviation: the topic is a
*command* ("send this"), not a *fact*, and the alternative — one topic per sending
module, each with its own subscription in `mail` — would mean editing `mail` for every
new sender, the Open/Closed violation this architecture exists to prevent. `topiccheck`
accepts it: a topic that is defined and subscribed satisfies the drift check regardless
of which crate defines it. In this rollout the topic has **no in-tree producer**
(finding A — nothing has an address to send to); Step 9 proves it end to end by
appending the event from the harness through `asyncevents.append_event`, the same SQL
entry point `config`'s row trigger uses.

---

## Contract shape (settled here, not during implementation)

`api/mail/events/src/lib.rs`:

```rust
pub const MAX_ADDRESS_BYTES: usize = 320;      // RFC 5321 total-address max, accounts' MAX_EMAIL_BYTES
pub const MAX_SUBJECT_BYTES: usize = 200;      // notifications' MAX_TITLE_BYTES
pub const MAX_BODY_BYTES: usize = 65_536;
pub const MAX_KIND_BYTES: usize = 64;
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;

pub mod providers {
    pub const LOG: &str = "log";
    pub const SMTP: &str = "smtp";
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendRequested {
    pub idempotency_key: String,
    pub to: String,
    pub subject: String,
    pub body: String,
    pub kind: String,
}

pub static SEND_REQUESTED: LazyLock<EventType<SendRequested>> =
    LazyLock::new(|| bus::define("mail.send_requested", 1, HistoryPolicy::Days(7)));
```

`api/mail/rpc/src/lib.rs` — the `audit` shape, two re-exports and nothing else:

```rust
pub use adminrpc::{register_admin, register_admin_submit};
```

**Rendering is the sender's job.** `mail` transports a subject and a body; there is no
template registry, no interpolation, no localization. A template engine inside the
transport would have to know every sender's data shape, which is the coupling the
contract crate exists to avoid.

### The delivery contract, stated plainly

**Enqueue is exactly-once per `idempotency_key`. Delivery to the recipient is
at-least-once.** A process that dies after the SMTP server accepted `DATA` and answered
`250`, but before the status `UPDATE` commits, leaves the row `pending`; the lease
expires and the row is re-claimed, and the recipient receives the message twice. This is
inherent to any outbox that is not in a distributed transaction with the mail relay, and
it is the single most user-visible property of this design. It is written here, repeated
in Step 13's gap list, and it is why seq #4's verification links must be idempotent and
its reset tokens single-use.

### Schema

`modules/mail/src/lib.rs` — `SCHEMA_DDL`:

```sql
CREATE SCHEMA IF NOT EXISTS mail;
CREATE TABLE IF NOT EXISTS mail.outbox (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    idempotency_key text        NOT NULL UNIQUE,
    recipient       text        NOT NULL,
    subject         text        NOT NULL,
    body            text        NOT NULL,
    kind            text        NOT NULL,
    state           text        NOT NULL DEFAULT 'pending',
    attempts        int         NOT NULL DEFAULT 0,
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    last_error      text,
    provider        text,
    sent_at         timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT mail_outbox_state_check
        CHECK (state IN ('pending','sent','parked','cancelled')),
    CONSTRAINT mail_outbox_recipient_len_check CHECK (octet_length(recipient) <= 320),
    CONSTRAINT mail_outbox_subject_len_check   CHECK (octet_length(subject)   <= 200),
    CONSTRAINT mail_outbox_body_len_check      CHECK (octet_length(body)      <= 65536),
    CONSTRAINT mail_outbox_kind_len_check      CHECK (octet_length(kind)      <= 64),
    CONSTRAINT mail_outbox_key_len_check       CHECK (octet_length(idempotency_key) <= 128)
);
CREATE INDEX IF NOT EXISTS mail_outbox_due_idx
    ON mail.outbox (next_attempt_at) WHERE state = 'pending';
CREATE INDEX IF NOT EXISTS mail_outbox_parked_idx
    ON mail.outbox (created_at) WHERE state = 'parked';
CREATE INDEX IF NOT EXISTS mail_outbox_recent_idx
    ON mail.outbox (created_at DESC, id DESC);
```

Three indexes, each with a named consumer, because 3a's errata #12 records what a missing
supporting index costs (seq scan → handler timeout → unbounded retry):
`mail_outbox_due_idx` serves the claim; `mail_outbox_parked_idx` serves the parked-count
gauge and the bulk requeue; `mail_outbox_recent_idx` serves the admin page's keyset
listing **and** the retention sweep's `created_at < $1` range predicate, since Postgres
scans a btree in either direction and a separate ascending index would be maintenance
cost with no reader of its own.

The five byte caps are duplicated between `mailevents::MAX_*_BYTES` and the SQL CHECKs.
Couple them the way `apikeys` does — a `COLUMN_CAPS` table in `store.rs` pairing each
constraint name to its Rust constant (`modules/apikeys/src/store.rs:99-109`), with the
Step 10 test that walks `SCHEMA_DDL` for every `_len_check` and fails on an unmapped one
(`modules/apikeys/src/store_tests.rs:620-634`).

**States are `pending | sent | parked | cancelled`. There is deliberately no `sending`
state**, because a process that dies mid-send would strand rows in it forever. The claim
is a committed `UPDATE` that bumps `attempts` and pushes `next_attempt_at` out by a
lease; a crash makes the row due again when the lease expires, at the cost of one burnt
attempt — fail-closed toward parking rather than toward an unbounded resend loop.

Claim SQL (one row per iteration, `SKIP LOCKED` so replicas are a consumer group by
construction — `core/asyncevents/src/worker.rs:199-207` is the in-repo precedent):

```sql
WITH due AS (
    SELECT id FROM mail.outbox
     WHERE state = 'pending' AND next_attempt_at <= now()
     ORDER BY next_attempt_at
     LIMIT 1
     FOR UPDATE SKIP LOCKED
)
UPDATE mail.outbox m
   SET attempts = m.attempts + 1,
       next_attempt_at = now() + make_interval(secs => $1),
       updated_at = now()
  FROM due
 WHERE m.id = due.id
RETURNING m.id::text, m.recipient, m.subject, m.body, m.kind, m.attempts
```

**Every status write is a CAS on `(id, state='pending', attempts=$N)`**, where `$N` is
the value returned by the claim — not on `attempts` alone. The `state` leg is what stops
an operator `cancel` landing during the SMTP dialogue from being silently overwritten
back to `sent`; a CAS miss is a named outcome, logged and counted in
`mail_send_cas_misses_total`, never an error. Because a cancel cannot recall a message
the relay already accepted, **`cancel` is best-effort against an in-flight send** — the
admin page says so in the confirmation, and Step 13 records it.

Backoff, the shape from `core/asyncevents/src/worker.rs:138-141` (the `clamp` and
`saturating_pow` are overflow guards, not style) with **named constants**, because a
literal beside the interval it governs is the defect CLAUDE.md's Fix-the-Authority
section names:

```rust
const MAIL_BACKOFF_MIN_SECS: f64 = 1.0;
const MAIL_BACKOFF_MAX_SECS: f64 = 300.0;

fn backoff_secs(attempts: i32) -> f64 {
    let exp = (attempts - 1).clamp(0, 30) as u32;
    (MAIL_BACKOFF_MIN_SECS * f64::from(2u32.saturating_pow(exp))).min(MAIL_BACKOFF_MAX_SECS)
}
```

### Environment surface

The split is the rule, not a preference: a boolean dev gate takes a default and warns; a
value with a domain that is **present and unusable kills the process** (the
`ASYNCEVENTS_HANDLER_TIMEOUT` / `NOTIFICATIONS_RETENTION_DAYS` convention).

| var | shape | unset | present-but-unusable |
|---|---|---|---|
| `MAIL_PROVIDER` | `log` \| `smtp` | channel **undrained**, see below | FAIL STARTUP naming the known names |
| `MAIL_FROM` | address | FAIL STARTUP when a provider is set | FAIL STARTUP |
| `MAIL_SEND_TIMEOUT_MS` | int ms, default 10000 | default | FAIL STARTUP (`0` included) |
| `MAIL_MAX_ATTEMPTS` | int, default 20, range 1..=1000 | default | FAIL STARTUP |
| `MAIL_RETENTION_DAYS` | int, default 30, range 1..=3650 | default | FAIL STARTUP |
| `MAIL_SMTP_HOST` / `_PORT` / `_USERNAME` / `_PASSWORD` / `_TLS` | `_TLS` ∈ `starttls\|implicit`, `_PORT` default 587 | required when `MAIL_PROVIDER=smtp` | FAIL STARTUP |

**The disabled channel must not be silently fail-open.** The durable subscription is
contributed *unconditionally* — it has to be, because `topiccheck`'s harness builds the
module set under a bare environment and an env-gated subscription would report
`UNSUBSCRIBED (SEAM)` in both profiles. So with `MAIL_PROVIDER` unset, requests are
still enqueued and checkpointed as delivered, and nothing ever sends them. A boot warn
that scrolled past is not a signal. Therefore, with no provider configured, the module
contributes a readiness check that **fails immediately and permanently**, naming
`MAIL_PROVIDER`, so `/readyz` is red on a process that is accepting mail it cannot
deliver. A deployment that genuinely wants no mail leaves `mail` out of its module list;
that is what the module registry is for.

`MAIL_SMTP_PASSWORD` is read once at `register`, held in the validated provider config,
and never written to argv, a log line, an admin cell, or `run/` state — the
`EPIC_CLIENT_SECRET` treatment (`modules/accounts/src/providers.rs:147-156`).

Constants: `DRAIN_INTERVAL = 1s`, `DRAIN_BATCH = 16`, `DRAIN_DEADLINE = 30s` (one budget
per pass, scheduler's model), `CLAIM_LEASE = 3 × MAIL_SEND_TIMEOUT_MS`,
`DRAIN_STALL_MAX = 2 × DRAIN_DEADLINE`, `STOP_GRACE = 4s` (deliberately under
`MODULE_STOP_GRACE_MS = 5000`), `PRUNE_BATCH = 256`, `PRUNE_BUDGET = 5s`.

---

## Step 1 — make room for a 14th DB-backed process  `[opus]` (core-implementer)

**(a) What.** `tools/processctl/src/fleet.rs` (`USABLE_PG_SESSIONS` and the
`HARNESS_RESERVE` doc table), a new `require_pg_session_floor()` preflight called from
`tools/splitproof`'s preflight, `devctl up`, **and `weles up`**, and
`docs/reference/platform-notes.md`.

**(b) Why now.** Finding B: `FleetSpec::new` fails closed at 85 > 81, so `mail-svc`
cannot enter the fleet until the budget admits it. Every later step that touches the
fleet depends on this landing first.

**(c) How.** Raise the dev cluster to `max_connections = 150` and set
`USABLE_PG_SESSIONS = 147` (150 − 3 `superuser_reserved_connections`), giving
`PG_SESSION_BUDGET = 131` against a 14-service need of 85 and weles' 99. Do **not** shave
`HARNESS_RESERVE` — its terms are itemized and three of them are the real mechanisms'
consts. The raise is only safe if a box that never applied it fails loudly instead of
exhausting connections mid-rollout, so add the self-check
("didn't-forget tooling must self-check"): query `SHOW max_connections` on the configured
`DATABASE_URL` before any process is spawned, compare against `USABLE_PG_SESSIONS + 3`,
and abort naming the required value and the full remedy — **`ALTER SYSTEM SET
max_connections = 150;` followed by a server restart**, because `max_connections` is
postmaster-context and `pg_reload_conf()` will not apply it. `weles` gets the same
preflight rather than an exemption: it is the one supervisor whose per-service
`DATABASE_POOL_MAX_CONNECTIONS = "3"` puts it over a stock cluster today, and it must
stay free of workspace-crate imports, so the check is a copied ~20-line function, not a
shared dependency. Record the requirement in platform notes beside the per-OS `psql`
guidance.

**(d) Dispatch.** `[opus]` — core-implementer, effort *think hard*. The authority is the
budget derivation, not the number: the preflight is what stops the const from becoming a
claim nothing verifies.

## Step 2 — contract crates  `[sonnet]`

**(a) What.** `api/mail/events/` (`mailevents`) and `api/mail/rpc/` (`mailrpc`) exactly as
specified in *Contract shape*; root `Cargo.toml` `members` + `[workspace.dependencies]`
entries for both.

**(b) Why now.** Every later step imports these; nothing here depends on anything else.

**(c) How.** `mailevents` deps are `bus`, `serde`, `serde_json` only — copy
`api/wallet/events/Cargo.toml`, including the `#[doc(hidden)]` golden sample the contract
tests read. `mailrpc` deps are `adminrpc` only — copy `api/audit/rpc/Cargo.toml` verbatim,
adding `register_admin_submit` to the re-export (it exists: `api/admin/rpc/src/lib.rs:47`)
because the Mail page is editable. No `api/mail/api` crate.

**(d) Dispatch.** `[sonnet]` — two small crates from two named templates, no design
decision left open.

## Step 3 — the module: schema, store, config, provider registry, ingress  `[opus]`

**(a) What.** `modules/mail/{Cargo.toml,src/lib.rs,src/store.rs,src/service.rs,src/config.rs,src/providers.rs,src/projection.rs}`
and `cmd/mail-svc/{Cargo.toml,src/lib.rs,src/main.rs}`.

**(b) Why now.** The outbox row and the enqueue path are what everything else acts on;
`cmd/mail-svc` ships in the same step because `archcheck` rule 12 fails a `modules/<name>`
with no `cmd/<name>-svc` root.

**(c) How.**
- `config.rs`: `MailConfig::from_vars(&BTreeMap<String,String>) -> anyhow::Result<MailConfig>`
  plus `from_env()` reading only the keys `mail_env_keys()` names. Take the variables as
  data so every failing branch is provable without mutating process env
  (`modules/accounts/src/providers.rs:218-219`). Set-but-empty is an error, never a
  silent default; per-field validation precedes cross-field completeness.
- `providers.rs`: `#[async_trait] trait Sender { fn name(&self) -> &'static str; async fn send(&self, m: &Outgoing) -> Result<(), SendError>; }`
  with `enum SendError { Rejected(anyhow::Error), Infra(anyhow::Error) }` — the
  `VerifyError` taxonomy (`providers.rs:45-57`). `Rejected` is permanent and parks the
  row; `Infra` backs off and retries. `KNOWN_PROVIDERS = &[LOG]` in this step; `SMTP`
  joins it in Step 4 (finding C). Registration panics on a duplicate name,
  `Providers::insert`'s convention. There is **no** `Configured`/`KnownButUnconfigured`/
  `Unknown` trichotomy: the name arrives from env at boot, and an unknown value is a
  startup failure naming the known names.
- `store.rs`: `enqueue_tx(conn, &NewMail) -> Result<Enqueued>` performing the
  claim-before-effect INSERT with `ON CONFLICT (idempotency_key) DO NOTHING RETURNING id`,
  and on a lost claim re-reading **on the same connection** and discriminating like
  `wallet::store::existing_ledger_tx` — identical `(recipient, subject, body, kind)` is
  `Enqueued::Duplicate`, a different payload under the same key is `Enqueued::Conflict`.
  A bare `DO NOTHING` would silently discard a corrected message, the defect 3a's errata
  #19 fixed. Also the `COLUMN_CAPS` table pairing each `_len_check` constraint to its
  `mailevents::MAX_*_BYTES` constant.
- `projection.rs`: subscription `mail.send-requested.v1` on `mailevents::SEND_REQUESTED`
  at `StartPosition::Genesis` — the topic is born in this rollout, so no retained history
  can predate the subscription, and `Genesis` then also covers a request appended between
  contract registration and first subscribe. One interaction with the wipe-is-the-migration
  rule, worth knowing before an operator does it: dropping schema `asyncevents` while
  keeping schema `mail` replays up to 7 days of retained requests, which the outbox's
  `idempotency_key` absorbs — only because `mail` survived. Validate the payload **before
  any statement runs in the delivery transaction**: an invalid payload after a failed
  statement would hit 25P02 on the checkpoint UPDATE, which returns through `?` BEFORE
  `record_failure` — so the subscription records no failure, gets no backoff and never
  pauses: it re-delivers the same event on every pass forever, making no progress, while
  `/readyz` stays green because passes keep completing. Invalid payload ⇒ log, `mail_enqueue_rejected_total`,
  `Ok(())`. `Enqueued::Conflict` ⇒ `mail_enqueue_conflicts_total`, `Ok(())` (a producer
  bug must not pause the plane). Infrastructure error ⇒ `Err`, so the plane backs off and
  pauses.
- `lib.rs`: `SCHEMA_DDL` as specified, `requires() -> vec![]`, `register` reading the
  config, `init` contributing the subscription and — when no provider is configured — the
  permanently-failing readiness check described in the env section. **No
  `opsapi::{SLOT,BINDING_SLOT,LOCAL_SLOT}`, no `DESCRIBE_SLOT`, and no `EDGE_SLOT` yet**:
  the first three are for `#[http]` ops, which `mail` has none of, and the `EDGE_SLOT`
  closure has nothing to register until Step 6 adds the admin faces.
- `cmd/mail-svc`: copy `cmd/notifications-svc` verbatim — `metrics::Metrics::new()` plus
  the module, no `remote::Stub` (nothing is required), player edge `None`.

**(d) Dispatch.** `[opus]` — core-implementer, effort *think hard*. Authority-first: the
25P02 ordering and the conflict discrimination are expensive to retrofit.

## Step 3b — register the topic with the two gates that scan for it  `[sonnet]`

**(a) What.** `tools/topiccheck/src/main.rs` (`defined_topics()`),
`tools/topiccheck/src/golden.rs` (`event_samples_by_crate()`), `tools/topiccheck/Cargo.toml`,
`cmd/server/{Cargo.toml,src/lib.rs}`, and `tools/checkmodules/{Cargo.toml,src/lib.rs}`.

**(b) Why now — this is the step the plan originally misplaced.** The moment Step 2 added
a `define(` site, `defined_topics_matches_every_define_site_on_disk` went red and the
workspace test suite with it: the scan finds every `define(` on disk and requires the
hand-maintained `defined_topics()` list to match. Registering it at Step 8, as revision 2
had it, leaves a blocking stage red across five steps. It cannot move any earlier either —
pulling it into Step 2 cascades into a missing golden sample and an `ALLOW_UNSUBSCRIBED`
entry for a topic nothing subscribes to yet, and that allowance would then be stale the
moment Step 3 lands, which `stale_allowances` fails on. Step 3 adds the subscription, so
this is the first point where the topic is both defined and subscribed and no allowance is
needed.

**(c) How.** Register `mail` in the monolith's module list (`cmd/server/src/lib.rs`) and
`mail-svc` in `tools/checkmodules`' Split profile FIRST — `topiccheck::observe()` builds
each profile's module set from those two lists, so a subscription that exists in
`modules/mail/src/projection.rs` is invisible to the gate until the module is in a boot
list, and the topic reports `UNSUBSCRIBED (SEAM)` in both profiles. Nothing in those two
registrations depends on the fleet, the ports or the admin page. Then add
`mailevents::SEND_REQUESTED` to `defined_topics()` — a hand-enumerated
list whose own doc calls itself "the one conscious edit point" — and `mail` to
`event_samples_by_crate()`, whose samples `golden.rs:402,437` require for every defined
`(topic, version)`. Do **not** add an `ALLOW_UNSUBSCRIBED` entry: the subscription exists
as of Step 3, and a sanctioned-sinkless allowance for a topic that has a sink is exactly
what `stale_allowances` rejects. Verify with `cargo test -p topiccheck` green and
`cargo run -p topiccheck -- --durability-strict` exiting 0.

One consequence, expected and owned by Step 7: with `mail` in the monolith's list, a
manually booted `cmd/server` reports `/readyz` red until the dev fleet env carries
`MAIL_PROVIDER=log` and `MAIL_FROM`, because the no-provider readiness check is permanent
by design. Do not add a default provider or make the check conditional to hide it.

**(d) Dispatch.** `[sonnet]` — four named list entries in four named files. The judgment-
bearing gate work (conformance's `ADMIN_SUBMIT_MODULES`, the `mail()` policy entry, the
CapCase probes) stays in Step 8, where the admin page it describes exists.

## Step 4 — the drain worker and the SMTP sender  `[opus]`

**(a) What.** `modules/mail/src/worker.rs` and `modules/mail/src/smtp.rs`, the `smtp` arm
of `MailConfig`, `SMTP` joining `KNOWN_PROVIDERS`, the `lettre` pin in the root
`Cargo.toml`, plus `start`/`stop`, readiness and metrics in `modules/mail/src/lib.rs`.

**(b) Why now.** The worker consumes the outbox and the `Sender` trait from Step 3; the
SMTP sender lands in the same step as its `KNOWN_PROVIDERS` entry (finding C) and needs a
driver to be reachable from.

**(c) How — the worker.** Copy `modules/scheduler/src/lib.rs`'s loop skeleton:
- A `start`-spawned task under `AssertUnwindSafe(run_loop(..)).catch_unwind()`, a `watch`
  stop channel honored *between* rows, and `stop_tasks()` with `STOP_GRACE` then `abort()`
  **followed by `t.await`** so the connection drop completes rather than detaching.
- Each pass: one `DRAIN_DEADLINE` budget shared by up to `DRAIN_BATCH` rows, claiming one
  row at a time so budget exhaustion never strands over-claimed rows; each send bounded
  independently by `MAIL_SEND_TIMEOUT_MS`. On exhaustion, log and end the pass — leased
  rows are simply due again.
- Success: `state='sent', sent_at=now(), provider=$p, last_error=NULL, body=''`.
  **Blanking the body on success is a security requirement, not tidiness**: a rendered
  body is a password-reset link or a verification token, and a `sent` row keeps it for
  `MAIL_RETENTION_DAYS` (30) otherwise. A delivered secret stops being archived the
  moment it is delivered; a `parked` row keeps its body because it still has to be sent.
  **Blank it on `sent` and on NO other state.** `store::body_is_comparable` reads exactly
  that: it drops `body` from the duplicate-vs-conflict comparison only for a `sent` row,
  so blanking a `cancelled` row — equally defensible on secret-residency grounds, since a
  cancelled reset link is still a secret — would make a durable replay of that request
  compare `"" != body`, return `Conflict`, drop the message, and report the operator's own
  cancel as a producer bug.
  `SendError::Rejected`: `state='parked'` with `last_error`. `SendError::Infra`: stay
  `pending` with `next_attempt_at = now() + backoff_secs(attempts)`, parking once
  `attempts >= MAIL_MAX_ATTEMPTS`. Every arm CAS-guarded on
  `(id, state='pending', attempts=$N)` — the `state` leg is load-bearing (see *Contract
  shape*), the `attempts` leg is the ABA guard from `worker.rs:364-374`.
- Readiness: `httpmw::ReadyCheck::new("mail", ..)` backed by a `Liveness` with three
  atomics (`dead`, `stopping`, `last_ok_secs`) over a **coarse monotonic** clock, the
  stamp seeded at loop entry, `set_stopping()` before signalling. `DRAIN_STALL_MAX` is
  derived from `DRAIN_DEADLINE`, never an independent literal.
- Metrics, `OnceLock<T>` + `let _ = metrics::register(Box::new(c.clone()));` inside the
  initializer (`core/asyncevents/src/retention.rs:61` is the idiom, and `register`
  returns a `prometheus::Result` that the idiom discards):
  `mail_send_attempts_total`, `mail_send_errors_total`, `mail_send_cas_misses_total`,
  `mail_enqueue_rejected_total`, `mail_enqueue_conflicts_total`, `mail_outbox_parked`
  (gauge, served by `mail_outbox_parked_idx`), `mail_outbox_oldest_pending_age_seconds`
  (gauge, computed as `min(next_attempt_at)` so `mail_outbox_due_idx` covers it — a
  `min(created_at)` form would scan). Refresh both gauges once per pass from the pass's
  own connection, not on a separate timer. A live-but-ineffective loop never flips `dead`,
  which is why counters exist beside the readiness stamp (`retention.rs:65-70`). Do not
  ship a `_state` twin of any gauge name (`plane_metrics.rs:11-16`).

**(c) How — the SMTP sender.** `lettre` with `default-features = false` and
`tokio1-rustls-tls` + `smtp-transport` + `builder`, following the
`reqwest`/`rustls-acme`/`quinn` precedent of selecting `ring` explicitly. Two checks are
part of this step: `cargo tree -i aws-lc-rs` must come back empty, and
`cargo tree -e features --workspace --target all -i tokio` must not gain tokio's
`process` feature — resolver-2 unifies features workspace-wide, so an SMTP crate arming
`process` trips `weles-async-island`'s workspace-wide ban through the `weles` binary. If
either fails, stop and report rather than working around it. Construction is **pure** (no
I/O in `init`, constraint 8): a bad host is a startup failure from the config parse.
Timeouts follow `modules/gateway/src/proxy.rs`'s reasoning — an SMTP dialogue is
multi-round-trip, so bound connect and each I/O step rather than assuming one
whole-request deadline covers it.

**(d) Dispatch.** `[opus]` — core-implementer, effort *think hard*.

## Step 5 — retention  `[sonnet]`

**(a) What.** `modules/mail/src/projection.rs` (a second subscription and a
`PruneHandler`), `MAIL_RETENTION_DAYS` in `config.rs`.

**(b) Why now.** After the worker, because a sweep that deletes rows a claim is holding
must know the states exist. Before the fleet step, so the schedule is registered once.

**(c) How.** Copy `modules/notifications/src/projection.rs`'s prune verbatim, including
the parts that are load-bearing rather than decorative: subscription
`mail.prune-on-scheduler.v1` via `on_tx_raw` on `schedulerevents::FIRED.topic()`, filtered
to `name = "mail-prune"`; the batched `ctid` + `FOR UPDATE SKIP LOCKED` delete loop with
`PRUNE_BATCH = 256`; the `created_at` **watermark**, without which every batch inside the
one still-open delivery transaction re-walks the tuples the transaction already deleted
and the loop goes quadratic; and the `PRUNE_BUDGET` exit that resumes on the next fire.
Delete only `state IN ('sent','cancelled')` — a parked row is an operator's unfinished
business and must survive retention. `MAIL_RETENTION_DAYS` parses with the
`retention_days_from_env` shape (unset ⇒ 30; present-but-unusable ⇒ FAIL STARTUP; range
1..=3650). Add the `mail-prune` schedule row to the scheduler seed alongside
`audit-prune` and `notifications-prune`.

**(d) Dispatch.** `[sonnet]` — a named file copied with two named changes (the state
filter and the topic name).

## Step 6 — the operator surface: the "Mail" admin page  `[opus]`

**(a) What.** `modules/mail/src/admin.rs`, the `adminapi::SLOT` contribution, and the
`edge::EDGE_SLOT` contribution wrapping `mailrpc::register_admin` +
`mailrpc::register_admin_submit` (this is the step that introduces `EDGE_SLOT` for
`mail` — before it, the closure would be empty).

**(b) Why now.** Parked rows need an operator verb, and the page is the surface Step 9
asserts through `admin-svc` in the split.

**(c) How.** `ADMIN_ITEM_ID = "mail"`, `ADMIN_LABEL = "Mail"`, `ADMIN_SLUG = "mail"`,
`ADMIN_SECTION = "Platform"` (beside "API Keys"). The portal derives the route from
`slugify(LABEL)`, not the item id (`modules/admin/src/lib.rs:1518`), so build every
self-link from `ADMIN_SLUG` even though the two agree here — 3a shipped a page whose
every link 404'd on exactly this.

`UILayout/GameOps Admin.dc.html` contains **no mail or outbox page** (its only "Queue" is
matchmaking), so this page is composed from the mockup's shipped idioms rather than
translated from a panel: a four-card KPI row (`PENDING`, `PARKED`, `SENT 24H`,
`OLDEST PENDING`) over a table, matching the dark card styling the mockup uses
throughout. Structure follows `modules/notifications/src/admin.rs`: one
`build_content(svc, params)` shared by the local render and the remote `AdminData`, one
`apply_submit(svc, values)` shared by both write paths, `block_in_place` for the
synchronous `RenderFn`. Columns `WHEN / TO / KIND / STATE / ATTEMPTS / LAST ERROR`,
`PAGE = 50` with the mandatory "this table is capped" note. **Never render the body or
any credential in a cell.**

Actions: `requeue` (parked → pending, `attempts = 0`, `next_attempt_at = now()`),
`requeue-all-parked` (the same, `WHERE state='parked'`, bounded to
`MAX_BULK_REQUEUE = 1000` rows per submit with the count reported back) — because a
misconfigured relay answering `530 5.7.0 Authentication required` parks *every* queued
message on its first attempt, and one-row-at-a-time recovery on a 50-row page is not a
recovery path; `cancel` (pending → cancelled, labelled best-effort against an in-flight
send); and a send-test form whose idempotency key is minted at **render** time into a
hidden input from `OsRng` and validated on submit, so a double-click cannot send twice.

`admin_data` must never `Err` on an unrecognized param — the portal forwards every page's
params to every provider, so an `Err` degrades an unrelated page to an error card in the
split; render an error card instead. Map rejections to `Conflict`/`Other` locally and
`conflict`/`invalid`/`internal` remotely — never `NotFound`, which the edge makes
indistinguishable from `UnknownMethod` and which silently degrades the page to read-only.

**(d) Dispatch.** `[opus]` — core-implementer, effort *think hard*. Admin UI work is
`[opus]` or above by standing rule.

## Step 7 — fleet and topology registration  `[sonnet]`

**(a) What.** `tools/processctl/src/fleet.rs` + `fleet_tests.rs`; `cmd/admin-svc/src/{lib.rs,main.rs}`;
`weles/fleet.split.toml`; `weles/master/src/{fleet_toml_tests.rs,manifest_tests.rs}`;
and the three stale-count comments named below.

**(b) Why now.** Every earlier step is code that exists; this makes both topologies
contain it. It must precede Step 9, which boots the fleet.

**(c) How.** `mail-svc` takes **HTTP 8094 / edge 9012** (the next free pair; 8093/9011 is
today's high-water mark). In `fleet.rs`: `service("mail-svc", 8094, Some(9012), vec![])`,
a `MAIL_OVERRIDEABLE_ENV` allowlist naming every `MAIL_*` key, the same keys added to
`MONOLITH_OVERRIDEABLE_ENV`, `("MAIL", 9012)` in **admin-svc's** peer loop, and
`"mail-svc"` in `admin.dependencies`. The development profiles set `MAIL_PROVIDER=log`
and `MAIL_FROM=dev@localhost`.

**`cmd/gateway-svc` gets nothing** — no `AddrSpec`, no `remote::Stub`, no peer entry.
`mail` publishes no HTTP op, so the gateway has nothing to dispatch to it, and
`cmd/gateway-svc/src/addrs.rs:24-28` is explicit that an edge peer with no address kills
the process: adding a spec for a peer the gateway does not stub would make gateway-svc die
under managed boot when mail-svc is not yet up. `cmd/gateway-svc/tests/boots.rs` also
needs no edit — it derives its expectations from `modules()` and `opscatalog::OPERATIONS`.

`weles/fleet.split.toml` gets a `[[service]]` block **carrying `MAIL_PROVIDER=log` and
`MAIL_FROM=dev@localhost` in its `env`**, so `weles up` and `devctl up split` do not
disagree about whether the channel drains, plus the `MAIL_EDGE_ADDR` peer block under
admin-svc. `weles/master/src/fleet_toml_tests.rs:868` asserts the service count as a
literal (`14` → `15`); `manifest_tests.rs`'s `full_fleet_env_goldens` asserts a byte-exact
env map per service and `fleet.len() == goldens.len()`, so mail-svc needs its full golden
tuple written out.

Three comments become false the moment this lands and are corrected here, because a
comment asserting behaviour the code lacks is a correctness defect:
`cmd/admin-svc/src/main.rs:15-17` ("nine peers", named by count and list),
`tools/splitproof/src/main.rs:3144` (`// (http 8080-8093, edge 9000-9011, player 9100)`),
and `weles/master/src/manifest_tests.rs:88` ("ALL 14 split services").

**(d) Dispatch.** `[sonnet]` — enumerated edits against named files with no design
decision left open. The judgment-bearing registrations were split out into Step 8.

## Step 8 — the verification gates  `[opus]`

**(a) What.** `tools/conformance/src/checks.rs`
(`ADMIN_SUBMIT_MODULES`), `tools/conformance/src/policy.rs` (the `mail()` entry and the
`basis` prose at `policy.rs:78`), and `modules/mail/src/conformance.rs`.

**(b) Why now.** These are the blocking stages that fail *because* Steps 3–6 landed, and
each requires a judgment the mechanical lane should not make. Separating them from Step 7
is what keeps that step `[sonnet]`.

**(c) How.**
- `ADMIN_SUBMIT_MODULES` (`tools/conformance/src/checks.rs:111`) is diffed against
  `modules/*/src` **before any assertion runs** (`checks.rs:118-137`); Step 6 makes `mail`
  a fourth implementor, so a miss is a drift failure. Add it, and update the `basis` prose
  at `policy.rs:78` — it enumerates the implementors and says "in ALL THREE implementors",
  which becomes false with a fourth.
- `modules/mail/src/conformance.rs`: a CapCase probe per input-capped field that drives
  `mail`'s **real** validator, not a restatement of it. 2a's lesson is exactly this — a
  proof audit deleted both input-cap guards from a production handler and
  `conformancecheck` still printed OK, because `InputPolicy::Validated { basis }` is prose
  nothing executes.
- The `mail()` entry in `policy.rs`: a concrete stance per convention, with an
  `EnvValidation` fixture per `MAIL_*` variable. Use `"   "` rather than `""` for a blank
  case — `set_var("")` removes the variable on Windows. Every `NotApplicable` carries a
  defensible reason; a bare one hides a known gap.

**(d) Dispatch.** `[opus]` — core-implementer, effort *think hard*. This step is where a
plausible-but-wrong stance silently disables a gate.

## Step 9 — split-proof assertions `[ML1]`–`[ML5]`  `[test-author]`, `model:"opus"`

**(a) What.** `tools/splitproof/src/main.rs` (a `mail_assertions(..)` block called from
the split pass and the monolith parity pass) and `tools/processctl/src/fleet.rs` for any
`PROOF_MAIL_*` constant.

**(b) Why now.** It runs against the landed, registered fleet and is the only proof that
the durable ingress works **across processes**.

**(c) How.**
- `[ML1]` cross-process enqueue and send: append a `mail.send_requested` event from the
  harness with `SELECT asyncevents.append_event(...)` — the same entry point `config`'s
  row trigger uses, so this is a real producer, not a test hook — then poll `mail.outbox`
  via sqlx until the row exists and reaches `state='sent'` with `provider='log'`. The
  producer is the harness and the consumer is `mail-svc`: a monolith-only implementation
  cannot pass this.
- `[ML2]` idempotency: append the same key twice with identical content; assert exactly
  one row. Append a third time with a different subject; assert still one row and that
  `mail_enqueue_conflicts_total` moved.
- `[ML3]` the Mail page renders through `admin-svc` in the split (remote
  `admin.adminData`), status 200, with `[ML1]`'s row in the body.
- `[ML4]` the send-test form submits through `admin-svc` (remote `admin.adminSubmit`) and
  creates a `mail.outbox` row — the remote-write face.
- `[ML5]` monolith parity: `[ML1]`–`[ML4]` re-run against `cmd/server` on the same front.
- Prove `[ML3]` is not vacuous the way `[NT2]` was: temporarily change `ADMIN_LABEL`,
  observe the named failure, revert byte-identically before committing.

**(d) Dispatch.** `[test-author]` at `model:"opus"` — the harness shape is novel (a
harness-authored durable event, a remote admin submit), the stated reason to escalate
this lane above its `sonnet` default.

## Step 10 — module unit tests  `[test-author]`, `model:"sonnet"`

**(a) What.** Four files, decided now rather than by feel: `modules/mail/src/tests.rs`
(store, enqueue, dedup, DDL/`COLUMN_CAPS` mapping), `config_tests.rs` (the env table),
`worker_tests.rs` (claim, backoff, CAS, parking, liveness), `projection_tests.rs` (the
three delivery arms and the prune loop).

**(b) Why now.** It starts from the landed, compiling diff of Steps 3–6, which is what
keeps this lane cheap.

**(c) How.** Each test names the branch that would otherwise be unproven:
- `MailConfig::from_vars` — one case per env-table row, including set-but-empty,
  `MAIL_SEND_TIMEOUT_MS=0`, an unknown `MAIL_PROVIDER`, `smtp` without a host, and
  `MAIL_RETENTION_DAYS` out of range. Drive `from_vars`, not process env.
- Every name in `KNOWN_PROVIDERS` builds a sender — the
  `providers_tests.rs:294-320` instrument that turns finding C into an assertion.
- `backoff_secs` at attempts 1, 2, 20, 31, 10_000 (the `clamp`/`saturating_pow` guards).
- Claim exclusivity: two concurrent claims against one due row yield one winner and one
  empty result — concurrency, not speed, no sleeping on a real clock.
- Lease expiry: a claimed row with `next_attempt_at` in the past is claimable again; set
  the timestamp explicitly rather than waiting.
- **The `state` leg of the CAS**: claim a row, set it `cancelled` behind the worker's
  back, then run the success write — the row must stay `cancelled` and
  `mail_send_cas_misses_total` must move. Without the `state` leg this test flips a
  cancelled row to `sent`, which is the defect it exists to pin.
- `Enqueued::Duplicate` vs `Enqueued::Conflict` on the same key, and the `sent` carve-out
  in `classify_existing` (pure, zero-I/O): a `sent` row with a differing body is
  `Duplicate`, a `parked` or `cancelled` row with a differing body is `Conflict`. The
  second assertion is what pins Step 4's blank-on-`sent`-only contract.
- The preflight verdicts Step 1 left uncovered: `processctl::check_pg_session_floor` at,
  below and above the threshold with the rendered message naming both the `ALTER SYSTEM`
  line and the restart, plus `weles::pgfloor::{check_pg_session_floor,
  fleet_session_reservation, fleet_dsn}` driven through their injected env closure.
- Parking: `SendError::Rejected` parks on attempt 1; `Infra` parks only at
  `MAIL_MAX_ATTEMPTS`, proven with a fake sender counting calls.
- Prune: deletes `sent`/`cancelled` past retention, **leaves `parked` untouched**, and the
  batch loop terminates — the statement-level-trigger-in-a-rolled-back-transaction
  instrument 3a used for the batch cap.
- `deliver_or_skip`'s three arms with a **decoy faulting subscription** as the instrument:
  an invalid payload and a conflict must leave the decoy's checkpoint advancing; an
  infrastructure error must pause. 3a shipped this arm with zero coverage, and swapping it
  to `Ok(())` — which silently loses events — passed every test at the time.
- `stalled_from(..)` as a pure predicate: never-seeded sentinel, stopping, over-age.
- The no-provider readiness check fails, with `MAIL_PROVIDER` named in its message.

**(d) Dispatch.** `[test-author]` at `model:"sonnet"` — these follow patterns already in
`modules/notifications/src/tests.rs` and `modules/accounts/src/providers_tests.rs`.

## Step 11 — the SMTP sender's loopback fixture  `[test-author]`, `model:"opus"`

**(a) What.** `modules/mail/src/smtp_tests.rs`.

**(b) Why now.** After Step 10 so the fake-sender scaffolding exists, and separate from it
because the fixture is a protocol server rather than a pattern copy.

**(c) How.** Bind a `tokio::net::TcpListener` on `127.0.0.1:0` and speak the minimum SMTP
dialogue (`220` greeting, `EHLO` capabilities, `MAIL FROM`, `RCPT TO`, `DATA`, `250`,
`QUIT`), with an `Arc<AtomicUsize>` hit counter and a captured transcript — the
`serve_counting_jwks` instrument from `modules/accounts/src/oidc_tests.rs`, which makes
"did the I/O happen?" an assertion rather than an absence of errors. Assert: `250` maps to
`Ok`; `550` maps to `SendError::Rejected` (permanent, parks); a dropped connection and a
connect refusal map to `SendError::Infra` (retries); recipient, sender and subject appear
in the transcript; and the configured send timeout fires against a server that accepts the
connection and never greets. Keep the fixture plaintext — this test proves the dialogue
and the error mapping; TLS negotiation against a real server stays outside the automated
proof, recorded as a gap in Step 13 rather than implied as covered.

**(d) Dispatch.** `[test-author]` at `model:"opus"` — a protocol fixture is the
novel-harness case the lane's default is escalated for.

## Step 12 — acceptance  `[inline]`

**(a) What.** `cargo run -p verifyctl -- --fast`, then `--all --strict`.

**(b) Why now.** After every code and test step, before documentation asserts anything.

**(c) How.** One rollout at a time: check for a live `cargo`/`rustc` first, confirm no
active fleet, then run exactly one. Redirect to a file and capture `$?` explicitly — a
piped `| tail` reports the pipe's exit status, which twice during 3a made a failing stage
look green. Three blessings are expected and each is read before it is accepted:
`--bless-contract-golden` (the new topic's golden), `--bless-input-golden` (the admin
form's new input fields), and `--bless-public-api` (the `mailevents` baseline).

**(d) Dispatch.** `[inline]` — this is the gate, not a code change.

## Step 13 — documentation  `[docs]`, `model:"sonnet"`

**(a) What.** `docs/roadmap/feature-tracker.md` (row 44 → landed with the real commit
range; **row 203's "owned by the notifications module" corrected**; a dated decisions-log
entry recording the new-fortress decision, the consumer-defined command topic, and the
session-budget raise), `README.md` (14 fortresses), `CLAUDE.md` (the module list and a
`mail` entry), and this plan's errata section. `docs/reference/platform-notes.md` is
**Step 1's** responsibility, not this step's.

**(b) Why now.** Last, against landed code, so nothing here is a prediction.

**(c) How.** The job is as much deleting false prose as adding true prose. The known-gap
list is complete or it is silence implying coverage:
1. **Delivery to the recipient is at-least-once** — the crash window between a `250` and
   the status commit re-sends (see *Contract shape*).
2. TLS negotiation is unproven by any automated test (Step 11).
3. The command topic has no in-tree producer until seq #4.
4. `mail` stores no address; nothing resolves a `player_id` to a recipient (finding A).
5. An operator sending mail through the **remote** admin submit produces no `admin.action`
   row, because `admin` emits that only where the form's module is co-hosted — in the
   split, `mail` lives in `mail-svc` and the portal in `admin-svc`. For the backend's first
   outbound-to-the-world channel this is the action most worth logging; the closure is a
   `mail.operator_sent` topic with an `audit` raw sink, deliberately deferred rather than
   forgotten.
6. `cancel` cannot recall a message the relay already accepted.

**(d) Dispatch.** `[docs]` — docs-writer at `model:"sonnet"`, with every file named above,
because an unnamed file is never found.

---

## Non-goals (deliberate, recorded)

- **Templates, interpolation, localization** — the sender renders; `mail` transports.
- **A `mailctl` operator CLI** — the parked-row verbs live on the admin page, which has to
  exist anyway; a second operator surface for the same actions is not earned.
- **`mail.sent` / `mail.failed` durable events** — nothing consumes them yet, and a
  defined topic with no subscriber is what `topiccheck`'s sinkless check exists to flag.
  (`mail.operator_sent` is the one that *is* wanted, and is gap 5 above, not a non-goal.)
- **Bounce and complaint handling, DKIM/SPF, suppression lists** — a production mail
  program, not a channel.
- **Address storage and verification** — seq #4, in `accounts` (finding A).

---

## Review response (revision 1 → 2)

The plan review returned REJECT with 22 findings. Every one is addressed:

- **Blocking gates that revision 1 never registered** (findings 1, 2, 4): `topiccheck`'s
  `defined_topics()`, the contract-golden's `event_samples_by_crate()`, conformance's
  `ADMIN_SUBMIT_MODULES` + `mail::conformance` + the `policy.rs:78` basis prose. All now
  own Step 8, and `--bless-contract-golden` / `--bless-input-golden` were added to Step 12.
  Revision 1's claim that a module without `#[http]` ops skips the contract golden was
  simply wrong — the golden is driven by the events crate.
- **A gateway `AddrSpec` that would have killed gateway-svc** (finding 3): removed. An
  edge peer with no address is a process-death condition, and the gateway has no reason to
  dial `mail`.
- **`cancel` silently overwritten by an in-flight send** (finding 5): every status write is
  now CAS-guarded on `state` as well as `attempts`, with a counter, a test that pins it,
  and an explicit best-effort statement.
- **Unbounded outbox growth** (finding 6): Step 5 adds retention on the
  `audit`/`notifications` shape, with parked rows exempt.
- **A dead `EDGE_SLOT` in Step 3** (finding 7): moved to Step 6, where the faces exist.
- **The undisclosed duplicate-send window** (finding 8): now a first-class contract
  statement and gap 1.
- **`ALTER SYSTEM` without a restart, and `weles` left out of the preflight** (finding 9):
  both fixed in Step 1; weles' own 99 > 97 arithmetic is in finding B.
- **Gauges with no supporting index** (finding 10): `mail_outbox_parked_idx` added, and the
  age gauge derives from `min(next_attempt_at)` so the due index covers it.
- **A disabled channel that accepts mail it cannot send while `/readyz` stays green**
  (finding 11): the no-provider state now fails readiness, and the reason the subscription
  must stay unconditional (topiccheck's bare-env harness) is recorded.
- **A trichotomy with no consumer** (finding 12): dropped from the design and the tests;
  finding C now states the rule that does transfer and the mechanism that does not.
- **Step 7 doing judgment work at `[sonnet]`** (finding 13): the gates split into Step 8
  at `[opus]`.
- **Three stale-count comments** (finding 14): named in Step 7.
- **Hardcoded backoff literals in a plan that bans them** (finding 15): named constants.
- **Byte caps duplicated with no drift mapping** (finding 16): `COLUMN_CAPS` plus the
  DDL-walking test, `apikeys`' shape.
- **No recovery path for a mass-park** (finding 17): `requeue-all-parked`, bounded.
- **No audit trail for an operator send in the split** (finding 18): gap 5, with its
  closure named.
- **An incomplete gap list** (finding 19): Step 13 now carries six.
- **Findings 20–22 and the four vagueness flags**: the `metrics::register` idiom, the
  `Genesis`-plus-wipe interaction, weles' env parity, the test file split, platform-notes
  ownership, and the UILayout answer (there is no mail page in the mockup — the layout is
  specified here instead).

One finding was checked and left as-is: the review's note that `admin-svc` doc prose
names its peers by count is correct and Step 7 already carried it.

---

## Errata (corrections found while implementing)

1. **Step 2 — `HistoryPolicy::Days(7)` does not exist.** The *Contract shape* section
   named a variant `core/bus` does not have: `HistoryPolicy` (`core/bus/src/lib.rs:632-638`)
   is `MinRetention { days: u32 }` or `KeepForever`. Landed as
   `HistoryPolicy::MinRetention { days: 7 }`, which is what the plan meant. Same class as
   3a's `Status::InvalidArgument` — a plan naming an API that is not there.
2. **Step 1 — the preflight compares the caller's own reservation, not
   `USABLE_PG_SESSIONS + 3`.** The step's text said the latter, which would have refused
   `devctl up monolith` (25 sessions) on a cluster offering 97. Landed as: each entry point
   sums the `PoolBudget`s of the fleet it is about to spawn (plus `HARNESS_RESERVE` where
   the harness runs alongside), and the verdict is `usable >= that sum`. Recorded in
   `344399e`'s commit message.
3. **Step 1 — the probe reads three settings, not one.** `max_connections` alone would
   have let the refusal message assert a `superuser_reserved_connections` value it never
   read. It now reads `superuser_reserved_connections` and PostgreSQL 16+'s
   `reserved_connections` in the same round-trip and reports the observed values.
4. **Step 1 — weles needed its own anti-drift pin.** Every other hand-copied
   weles↔processctl constant is pinned by `verifyctl`'s `weles-wire-contract` stage; the
   step did not say so, and the first implementation shipped an unpinned copy. Four
   constants are now pinned there.
5. **Step 1 has no test coverage and the plan has no step that covers it.** Step 10's
   `[test-author]` scope is the `mail` module only. The preflight's pure verdicts
   (`processctl::check_pg_session_floor`, `weles::pgfloor::{check_pg_session_floor,
   fleet_session_reservation, fleet_dsn}`) are zero-I/O and `pub` precisely so they can be
   driven without a cluster; **Step 10 is extended to cover them**, and this is the plan
   defect that made the omission possible — a step touching testable production code needs
   a named test step, and Step 1's was missing.
6. **Observed, unexplained:** one `cargo test -p processctl --lib` run during Step 1
   reported `46 passed; 1 failed` without naming the test; five subsequent runs were 47/47.
   The suite forks and holds `flock`s and carries an explicit `fork_flock_serial` guard for
   that interaction. Not reproduced, not diagnosed, recorded rather than called clean.

7. **Step 2 — the durable payload carries a secret, and the topic's retention was too
   long.** `SendRequested.body` holds the rendered message, which for seq #4 is a
   password-reset link. It lands in `asyncevents.events`, and `tools/eventctl` prints a
   poison event's `payload:` verbatim to stderr. Landed as `MinRetention { days: 1 }`
   (minimum secret residency — retention is checkpoint-coupled, so a longer window buys
   nothing for delivery), with the rendered-body shape kept deliberately (a template
   reference would put every sender's data shape inside the transport) and recorded in
   the crate doc as a named accepted risk. **Step 4 was amended** to blank `body` when a
   row reaches `sent`.
8. **Step 2 — `providers::{LOG, SMTP}` did not belong in the events crate.** No field of
   `SendRequested` holds a provider name, so the constants would have entered
   `mailevents`' public-api baseline and made an internal config rename a BREAKING diff.
   They move to `modules/mail/src/providers.rs` in Step 3, beside `KNOWN_PROVIDERS`.
9. **Step 2 — `define(` must be a single line.** `tools/topiccheck/src/tests.rs:281`
   scans the tree for `define("topic", …)` and PANICS when the literal is not on the
   `define(` line, so a wrapped call turns a blocking stage red with a message about a
   missing string literal. Not a rule the plan knew; recorded here for the next contract
   crate.
10. **The plan misplaced the topiccheck registration by five steps.** Revision 2 put
   `defined_topics()` and the contract golden in Step 8. Adding a `define(` site in Step 2
   turns `defined_topics_matches_every_define_site_on_disk` — and with it the blocking
   workspace test stage — red immediately, and it stays red until that registration lands.
   **New Step 3b** carries it, placed at the first point where the topic is both defined
   and subscribed so no `ALLOW_UNSUBSCRIBED` entry (which `stale_allowances` would then
   reject) is needed. The tree is knowingly red between Step 2 and Step 3b.
11. **Process defect, mine:** the plan-errata commit `4287b92` was made with `git add -A`
   while a subagent was concurrently editing the tree, so an unrelated (correct, reviewed)
   doc-comment fix to `api/wallet/events/src/lib.rs` was swept into a `docs(plans)` commit.
   The commit boundary rule is per unit of work; staging by path is the fix, and history
   was left alone rather than rewritten for tidiness.
12. **Step 3 — the 25P02 consequence in this plan was wrong, and the module copied it.**
   The plan said a failed checkpoint UPDATE "aborts the whole worker pass, starving every
   other subscription". `core/asyncevents/src/worker.rs:437-446` catches the `Err`, logs
   it, and breaks only THAT subscription's quantum; the loop over subscriptions continues.
   The real consequence is stronger and is what the text now says: the success arm returns
   through `?` before `record_failure`, so nothing is recorded, nothing backs off, nothing
   pauses, and the event hot-loops forever with `/readyz` green. Third instance in this
   rollout of a plan asserting plane behaviour without reading the authority — the ordering
   requirement was right for the wrong stated reason.
13. **Step 3 deviations, recorded here rather than only in a commit message.** The provider
   registry is a `ProviderKind` enum, not a name→sender map with `Providers::insert`'s
   panic-on-duplicate (one configured provider per process makes a duplicate
   unrepresentable; `KNOWN_PROVIDERS` and `from_name` are the two lists that can now drift,
   pinned by Step 10's "every known name builds a sender"); `mail_env_keys()` is a private
   `MAIL_VARS` const, because a `pub fn` with no caller is surface without an authority;
   `MAIL_FROM` set without `MAIL_PROVIDER` is a startup failure (accounts'
   `EPIC_REDIRECT_URI`-without-secret precedent), a row the env table did not have.
14. **Step 3 — `mail_outbox_created_at_idx` was redundant on arrival.** The plan mandated
   four indexes; `mail_outbox_recent_idx (created_at DESC, id DESC)` already serves the
   retention sweep's range predicate. Dropped, and the schema block above corrected.
15. **Step 3b was mis-scoped: a subscription is invisible to `topiccheck` until its module
   is in a boot list.** The step registered the topic in `defined_topics()` and the contract
   golden, which closed the define-site scan, but `topiccheck::observe()` builds each
   profile's module set from `checkmodules::monolith_modules()` (which calls
   `server::modules`) and `split_process_modules()` — neither of which knew about `mail` —
   so `mail.send_requested` reported `UNSUBSCRIBED (SEAM)` in both profiles and
   `--durability-strict` still exited 1. The `cmd/server` and `checkmodules` registrations
   moved from Step 7 into Step 3b; the fleet, ports, weles and admin-svc registrations
   stayed in Step 7. The implementing agent found this by tracing rather than reaching for
   the `ALLOW_UNSUBSCRIBED` entry the step forbade, which would have been a stale allowance
   the moment Step 7 landed.
