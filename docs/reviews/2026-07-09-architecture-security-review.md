# Architecture and security review — 2026-07-09

> **ADDENDUM (2026-07-10).** Do not read the findings below as a list of open
> risks — most are closed:
>
> - **Findings 1, 2, 6 — closed structurally** by durable event log v2
>   (2026-07-10): the push plane this review examined (`core/outbox`, the relay,
>   `POST /events`, `EVENTS_SUBSCRIBERS`) was deleted, not hardened. Events are
>   now an XID-ordered shared Postgres log with consumer-owned pull
>   subscriptions; there is no event ingress endpoint and no per-process event
>   routing config to drift. archcheck bans the retired push vocabulary.
> - **Finding 10 — was already closed** when re-checked on 2026-07-10:
>   `tools/checkmodules::monolith_modules()` calls `server::modules()` from
>   `cmd/server`'s lib directly (no hand-mirrored list), and the Split profile
>   calls every real `cmd/<name>-svc` lib.
> - **Findings 5, 8, 9, 11, 12 and the tripwire slice of 7 — closed** by the
>   hardening rollout of 2026-07-10
>   (`docs/plans/2026-07-10-1400-security-review-hardening-plan.md`): startup
>   unwind (`fix(lifecycle,app)` 2f6a063), gateway collision detection
>   (`feat(gateway)` 499bba3), `Caps` deleted (`refactor(lifecycle,…)` a07a246),
>   dev defaults fail-closed incl. admin startup bail (`feat(accounts,…)`
>   29cbb8a), archcheck foreign-schema SQL tripwire (`feat(archcheck)` 58d02f6),
>   rustls-pemfile removed (`chore(edge)` c6d0f72).
> - **Finding 3 (shared CA) — deliberately deferred** to the mini-orchestrator /
>   multi-host milestone; acceptable for the single-trusted-machine deployment.
> - **Finding 4 — partially closed**: admin fail-open is gone (startup fails
>   without `ADMIN_USER` unless explicit `ADMIN_OPEN=1`). CSRF protection and
>   hashed API-key storage remain open by deliberate trust-model decision
>   (sessions-token model, local portal).
> - **Finding 7 (runtime DB-role isolation) — deliberately deferred**; the
>   archcheck tripwire covers accidental drift.

## Scope

Review of the Rust workspace as a modular monolith with a supported split-process
deployment. The review covered:

- crate and module dependency boundaries;
- lifecycle, registry, contribution slots and generated RPC seams;
- monolith versus `cmd/<name>-svc` composition roots;
- durable event delivery and split topology configuration;
- gateway authentication and authorization;
- admin and API-key surfaces;
- Git history related to fortress and abstraction-leak refactors;
- the architecture checkers and the live split proof.

This document records findings only. No implementation changes were made as part of
the review.

## Executive assessment

The architecture is genuinely modular and the split is real. This is not merely a
monolith whose packages have service-shaped names: the same contracts resolve to
local implementations in `cmd/server` and remote stubs in split processes, and the
full live split proof passes.

The strongest parts are the pure contract crates, generated transport glue,
topology-blind module implementations, the registry swap, durable outbox/inbox
semantics, and the mechanical architecture gates.

It is not yet production-hardened. The main remaining risks are the unauthenticated
event ingress when deployed beyond a trusted loopback, manually duplicated event
topology, cluster-wide CA trust, fail-open admin configuration, partial-start
cleanup, and outbox behavior under a large or poisoned backlog.

## Findings

### 1. Event ingress is unauthenticated

**Context-dependent severity:** medium on a single trusted development machine;
high when service HTTP listeners are reachable from other hosts, containers or
untrusted local processes.

`asyncevents::Plane::router` mounts `POST /events` on the normal process HTTP
listener. The endpoint accepts caller-provided `X-Event-Id`, `X-Event-Topic` and a
raw payload without authenticating the sender. The relay also posts over ordinary
HTTP without a signature or credential.

Relevant code:

- `core/asyncevents/src/lib.rs`, `Plane::router` and `handle_inbound`;
- `core/outbox/src/lib.rs`, `Relay::post`;
- `core/app/src/lib.rs`, where the plane router is merged into the main router.

Possible effects include forged domain events, arbitrary inbox growth and
pre-claiming an event ID before legitimate delivery. Event handlers have material
effects: inventory grants and wipes, rating/leaderboard updates and audit writes.

The current split scripts use `127.0.0.1` subscriber URLs, so cryptographic event
authentication is not urgent as long as the deployment is deliberately restricted
to one trusted machine. However, this restriction is currently a deployment
assumption rather than a property enforced by the event listener.

Recommended progression:

1. For local development, bind event ingress explicitly to loopback or a local IPC
   transport.
2. For multi-host/container deployments, use authenticated delivery, preferably
   service-identity mTLS; a signed HMAC envelope is a smaller interim option.
3. Keep event ingress on a listener distinct from public/application HTTP.

### 2. The semantic event graph is duplicated in deployment scripts

**Severity:** high reliability/architecture risk as the system evolves.

Addresses belong to deployment configuration, but the scripts currently also own
domain knowledge such as:

```text
character.created -> inventory, audit
match.finished     -> rating, leaderboard, audit
```

This knowledge already exists in module `on_tx` registrations, then appears again
as hand-written `EVENTS_SUBSCRIBERS` values in `run.ps1`, `run.sh`,
`split-proof.ps1` and `split-proof.sh`.

`topiccheck --durability-strict` proves that a durable subscriber exists when it
builds the monolith module set. It does not prove that the producing split process
has been configured with every subscriber URL. A missing URL can therefore pass the
current static checkers.

The failure mode is made dangerous by the outbox contract: a row with no targets is
treated as successfully delivered and marked sent. A configuration omission can
therefore become silent, permanent event loss.

Recommended short-term fix:

- export the observed topic/subscriber graph from `topiccheck`;
- add a blocking `splitcheck` that compares it with one split topology manifest;
- generate `EVENTS_SUBSCRIBERS` values from that manifest;
- do not mark a contract event sent when it has no targets, unless that topic is
  explicitly allowlisted as intentionally sinkless;
- expose missing-target state through readiness.

Recommended long-term fix:

- introduce one topology manifest containing service placement and addresses;
- derive event routes from recorded module subscriptions rather than spelling the
  domain graph in scripts;
- use the same manifest to generate process wiring, peer addresses and verification
  inputs where practical.

### 3. Internal mTLS authenticates cluster membership, not service identity

**Severity:** high for a hostile or compromise-oriented production threat model;
acceptable for the current trusted experimental cluster.

Every internal process loads the shared CA private key and mints a generic client or
server leaf. Servers verify that callers chain to the common CA, but do not learn or
authorize a concrete caller service identity.

Consequences:

- compromise of one process or the shared CA key enables impersonation of any peer;
- there is no caller-service-to-method authorization;
- the blast radius is the entire internal RPC surface.

Relevant code: `core/edge/src/tls.rs`, especially `DevCA`, `leaf`, `server_tls` and
`shared_dev_ca`.

For a hardened deployment, the CA signing key should not be distributed to
applications. Each workload should receive a separately issued certificate with a
service identity in SAN/SPIFFE form, followed by caller-to-method authorization.

### 4. Admin is fail-open and mutating forms lack CSRF protection

**Severity:** high if admin HTTP can be reached outside a trusted development
environment.

When `ADMIN_USER` is empty, the admin portal is completely open and only emits a
warning. Mutating `POST /admin/:slug` forms rely on Basic Auth but do not validate a
CSRF token or request origin.

The API-key admin page also renders complete key secrets, and the backing table
stores keys in plaintext. The remote admin-data surface transports the complete
table as read-only content.

Relevant code:

- `modules/admin/src/lib.rs`, `Admin::init`, `item_post` and `check_auth`;
- `modules/apikeys/src/admin.rs`, `build_table`;
- `modules/apikeys/src/store.rs`.

Recommended hardening:

- fail startup without admin credentials unless an explicit development-only
  bypass is enabled;
- add CSRF tokens or strict Origin/Sec-Fetch-Site validation;
- store API keys as a prefix plus cryptographic hash;
- reveal a new key only once and never return full secrets through admin fan-out.

### 5. Partial startup does not unwind already-started components

**Severity:** medium reliability issue.

`App::start` fails on the first module error but does not stop modules that already
started. After modules start, failures in event-plane startup, CA loading, QUIC
binding, player binding or HTTP binding return early without performing the normal
ordered teardown.

Relevant code:

- `core/lifecycle/src/app.rs`, `App::start` and `App::stop`;
- `core/app/src/lib.rs`, `run` between module start and HTTP serving.

The current binaries exit after such a failure, so the operating system cleans up
most resources. The lifecycle contract is nevertheless incomplete for embedded
tests, external leases and future long-lived supervisors.

Recommended fix: track started modules/components and unwind them in reverse order
on every startup error, ideally through an explicit runtime state guard.

### 6. Outbox draining uses an unbounded batch and holds locks across network I/O

**Severity:** medium-to-high operational risk under backlog or slow subscribers.

`Relay::pending` selects every unsent row for an origin without a limit. The relay
keeps the transaction and row locks open while performing sequential remote HTTP
deliveries, each with a timeout.

Possible consequences:

- long-running transactions and delayed vacuum;
- one pool connection occupied for a long time;
- memory proportional to the complete backlog;
- latency proportional to rows times subscribers;
- a slow or poisoned target amplifying recovery time.

Relevant code: `core/outbox/src/lib.rs`, `drain_once`, `pending`, `deliver` and
`Relay::post`.

Recommended fix: bounded batches and a short claim/lease transaction, delivery
outside the claim transaction, followed by idempotent completion updates.

### 7. Schema isolation is convention and static checking, not runtime authority

**Severity:** medium architecture risk.

Every module receives the shared unrestricted `PgPool`. `archcheck` catches
cross-schema foreign keys and several source patterns, but cannot stop dynamic SQL
or an ordinary query against another module's schema.

Relevant code: `core/lifecycle/src/context.rs`, where `Context` exposes `PgPool`.

This is a reasonable experimental modular-monolith compromise. Strong runtime
fortresses would require a database role/pool per module, a restricted search path
and grants only to the module's own schema plus narrowly defined shared
infrastructure.

### 8. Gateway route and method collisions are not rejected

**Severity:** medium future-extension risk.

Gateway route construction collects bindings, local invokers and peer addresses
into hash maps. Duplicate keys overwrite earlier entries, while HTTP route matching
uses the first matching route. A new module can accidentally create order-dependent
behavior through a duplicate method ID or verb/path pair.

Relevant code: `modules/gateway/src/lib.rs`, `RouteTable::build` and
`RouteTable::find`.

Recommended fix: fail startup or a checker on duplicate RPC methods, normalized
HTTP verb/path pairs, local invokers and provider peers.

### 9. The lifecycle `Caps` declaration is a manual drift point

**Severity:** low-to-medium future-maintenance risk.

`Module` already supplies default no-op implementations of `register`, `migrate`,
`start` and `stop`, but each implementation must separately set a `Caps` bit before
the phase is called. Forgetting the bit silently disables real lifecycle code.

Current modules were checked and are internally consistent. The API is still
unnecessarily error-prone. Calling every phase unconditionally would be safe because
the defaults are already no-ops.

Relevant code: `core/lifecycle/src/module.rs` and `core/lifecycle/src/app.rs`.

### 10. Monolith and checker module sets remain manually synchronized

**Severity:** medium architecture-verification gap.

`tools/checkmodules::monolith_modules` is shared by `topiccheck` and
`requirecheck`, which removed one source of drift. It still manually mirrors the
module vector in `cmd/server`. `archcheck` proves every module has a corresponding
service binary, but does not prove every module is present in the monolith or in the
checker harness.

Relevant code:

- `tools/checkmodules/src/lib.rs`;
- `cmd/server/src/main.rs`;
- `tools/archcheck/src/main.rs`.

Recommended fix: use one module catalog to construct the monolith and checker
harnesses, or add a metadata/source-level equality assertion.

### 11. Development defaults are deliberately permissive

**Severity:** low in the current experiment; high if the same defaults reach a real
deployment.

Examples:

- `ACCOUNTS_DEV_AUTH` defaults on;
- `INVENTORY_DEV_GRANT` defaults on;
- admin auth defaults open;
- development CA behavior is enabled when explicit CA paths are absent.

The code logs warnings and API-key bypasses are more explicit, but warnings are not
a production policy. A future production profile should fail closed and require a
single explicit development mode to enable these conveniences.

### 12. Dependency audit warning

`cargo audit --ignore RUSTSEC-2023-0071` reported no active vulnerability but did
report `RUSTSEC-2025-0134`: `rustls-pemfile 2.2.0` is unmaintained. This is currently
an advisory maintenance warning rather than a demonstrated exploit.

## Assessment of the modular-monolith split

The current split works and preserves the intended seams:

- local and remote implementations share capability contracts;
- modules do not import foreign implementations;
- pure API crates are separated from transport glue;
- module implementations do not read split topology;
- the gateway is the single public operation front door;
- durable events cross process boundaries;
- every current fortress has a service composition root;
- monolith and split behavior are exercised by the same functional scenarios.

The phrase "one script compiles the monolith into microservices" should be used with
care. The repository currently contains two explicitly maintained assembly graphs:
the monolith vector and a set of service binaries plus deployment scripts. The
scripts build and boot both shapes, but do not derive the split automatically from a
single module topology declaration.

## Git-history assessment

The abstraction-leak refactors were substantive and remain visible in the current
architecture:

- `c866024`: pure `<name>api` separated from `<name>rpc` transport glue;
- `cae6408`: topology-blind edge exposure through `EDGE_SLOT`;
- `b3a6ce7`: messaging moved from a pseudo-domain module into an app-owned event
  plane;
- `7418320`: engine-neutral `AnyTx`/`Delivery` seam removed `sqlx` from `bus`;
- `fa6ae5e`: gateway peer topology moved out of the module into composition-root
  contributions;
- `6f2855d`: checker rules added to prevent regression;
- `74ee76b`: every fortress must have a service binary.

The remaining topology problem is not that domain modules know deployment details.
They do not. The remaining leak is that the composition topology is duplicated
across several manually synchronized lists and scripts rather than represented once
and mechanically projected.

## Verification performed

The following completed successfully on 2026-07-09:

- `cargo test --workspace`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- `cargo run -q -p archcheck`;
- `cargo run -q -p requirecheck -- --strict`;
- `cargo run -q -p topiccheck -- --durability-strict`;
- `pwsh -File .\split-proof.ps1`.

The split proof built and booted the twelve-process split, exercised real auth,
API-key policy, cross-process synchronous calls, durable events, admin fan-out,
metrics, rate limiting and player QUIC, then passed the monolith parity leg.

`cargo audit --ignore RUSTSEC-2023-0071` completed with the one unmaintained-crate
warning described above.

## Suggested order of future work

1. Make targetless durable event delivery fail closed and add split-topology
   validation.
2. Enforce loopback-only event ingress for the current deployment model; add
   authenticated event transport before multi-host use.
3. Harden admin authentication, CSRF behavior and API-key storage/display.
4. Replace the shared CA-key model before treating internal peers as mutually
   untrusted workloads.
5. Add startup unwind and bounded outbox draining.
6. Single-source the monolith, checker and split topology catalogs.
7. Add global operation/route collision checks and consider DB-role isolation.
