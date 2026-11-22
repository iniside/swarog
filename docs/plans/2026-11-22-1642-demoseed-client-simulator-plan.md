# Plan: demoseed as a concurrent client simulator over every public operation

## Context

**Requested outcome (owner, 2026-11-22):** "zrobic tak, aby demoseed dzialal przez api, ktore jest wystawione publicznie przez gateway, czyli to bedzie taki troche symulator klientow, ktorzy robia requesty [...] do kazdego modulu tak jak robiloby to klient, dzieki temu bedziemy mogli caly backend przetestowac i jednoczesnie zaseedowac jakies przykladowe dane."

**Owner decisions (2026-11-22, answers 1-6):**
1. This plan replaces Step 24b (24b0-24b4) of `docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md`.
2. An error does not end the run: the run continues and lists every error at the end. Coverage self-check against `opscatalog::OPERATIONS`: a public op no scenario called is an error.
3. Destructive ops run on throwaway players; the seeded data stays.
4. Modules with no public route (mail, config, apikeys, rating, scheduler, audit) fill only as side effects of events. No new public routes.
5. Monolith only for now, run by hand. No verifyctl stage, no devctl flag, no split run.
6. Many clients in parallel, as far as the backend allows.

**Standing rules kept:** demoseed writes only through the public gateway API, never SQL (`memory/demoseed-is-the-one-seeder.md`, plan `2026-11-01-1950-demoseed` Exclusions). The seeded part stays deterministic and convergent (a re-run creates nothing new). No new module, event, contract or route.

**Explicit exclusions:** no split run (decision 5); no change to any module, api crate, gateway or devctl; no negative/abuse testing beyond the expected statuses below (the request is "like a client"); no load-test metrics (latency histograms); party push frames and the player-QUIC plane are not driven (they are not in `OPERATIONS`, which lists HTTP ops only).

### Overlap map (why not extend / depend on X)

| Candidate | What it does | Decision |
|---|---|---|
| `tools/demoseed` | Sequential, convergent seeder over 17 of the 52 ops; stops on the first error (`lib.rs:477-513`, `classify` `:300-318`, `Gateway::call` `:395-460`) | **Extend** — it already holds keys, bearer, 429 backoff, the pure deterministic `plan()` and 30 unit tests. |
| `tools/splitproof` | Proof harness; drives party/deletion/notifications over HTTP **plus SQL and sockets** (`main.rs:1131-1169,2905-2975`, `party_checks.rs`) | Do not depend on it: it asserts DB state and owns a fleet lifecycle. Copy request shapes only. |
| `opscatalog` | Generated list of the 52 HTTP ops, zero deps (`opscatalog/Cargo.toml`, `src/lib.rs:27-40`) | **Depend on it** for the coverage check — the generated catalog is the authority, not a hand list (`memory/didnt-forget-scripts-must-self-check.md`). archcheck classifies `tools/*` as `Kind::Other` (`tools/archcheck/src/main.rs:290-329`); `modules/apikeys` and `cmd/gateway-svc` already depend on it. demoseed's Cargo comment "no workspace-crate dependencies" is relaxed for this one leaf crate. |
| `api/*/api` typed contracts | Request/response types | Do not depend: the simulator checks the wire shape like an independent client; local structs keep it from compiling against the server's own types. |
| `eventctl seed-mockup` | Event plane dev fixture | Unchanged (Step 24b text). |

### Verified facts (research 2026-11-22, three researcher reports)

- **Concurrency limits (monolith).** No HTTP limiter in the monolith unless `RATE_LIMIT_RPS` is set (`core/app/src/lib.rs:264-272,386,1161-1195`; only `cmd/gateway-svc/src/main.rs:178` sets 20/40). Accounts: 2 argon2 permits, 32 login slots; slot 33 gets 503 "too many concurrent login attempts" (`modules/accounts/src/lib.rs:864,1234-1235`). Guests skip argon2 (`modules/accounts/src/guest.rs`, prose lead — Step 3 confirms on the handler).
- **Error bodies** are plain text, not JSON (`modules/gateway/src/lib.rs:2053-2063`).
- **Accounts.** `createGuest` POST `/accounts/guest` 201 `{player_id, token, refresh_token, access_expires_in_secs, device_secret}`; `loginFederated` `{provider:"guest", credential:<device_secret>}` 200 Session (`epic`/`google` → 503 locally); `refresh` `{refresh_token}` 200 Session, a replay outside 30 s grace revokes the family (never replay); `link` `{provider:"guest", credential:<own device_secret>}` 200 MeView, idempotent (`modules/accounts/src/lib.rs:483-530`); `beginDelete` 200 `{ticket, expires_at}`; `deleteAccount` `{ticket}` 200 `{player_id, handle, deleted_at}`, after which the bearer gets 401 (`tools/splitproof/src/main.rs:2905-2975`). `me` → `{player_id, display_name, handle, identities}`.
- **Characters.** create 201 `{id, player_id, name, class, created_at}`, list 200 array, delete `DELETE /characters/{id}` 204.
- **Inventory.** `listCharacter` `GET /inventory/character/{id}` 200 array; a new character gets the starter item asynchronously through `character.created` (`modules/inventory/src/projection.rs:103-152`) — poll.
- **Wallet.** `listCurrencies` → `gold`, `gems` with `WALLET_DEV_SEED`; `myBalances` → `[]` for a guest; a registered (non-guest) player may hold a configured starter currency from `player.registered` — check shape only (no client credit exists).
- **Leaderboard/match.** `topScores` `GET /leaderboard` 200 `[{player, wins}]`, top 100, fed by `match.finished` asynchronously — poll. `match.report` needs `dev-key-server` (202).
- **Friends** (`api/friends/api/src/lib.rs:113-195`): decline/accept by addressee (204), `remove` DELETE `/friends/{id}` 204, `list`/`pending` POST `{cursor, limit}` → `{items, next_cursor}`.
- **Groups** (`api/groups/api/src/lib.rs:119-202`): `members` (members only), `pending` (admin only), `leave` 204 (last admin with other members → 409; sole member deletes the group).
- **Party** (`api/party/api/src/lib.rs:118-189`): all POST; create 201, invite `{target_handle}` 201 `InviteView{invite_id,...}`, invites 200, accept `{invite_id}` 200, decline 204, get 200, heartbeat 200, set_ready `{state}` 200, promote/kick `{player_id}` 200, leave 200; one party per player; 60 s lease.
- **Notifications** (`api/notifications/api/src/lib.rs:80-105`): rows only from event consumers; clients can trigger `friend.requested` (addressee) and `friend.accepted` (requester) (`modules/notifications/src/projection.rs:239-286`); asynchronous (NOTIFY, 1 s poll floor) — poll. markRead 204 idempotent, delete 204.
- **Storage** (`api/storage/api/src/lib.rs:142-196`): get POST `/storage/{c}/{k}/get` `{owner_id}` ("" = own; foreign only if `public_read`), list POST `/storage/list` `{collection, cursor, limit}`, delete 204.
- **Body keys are not uniform:** most are snake_case parameter names, but `register` sends `displayName` (`tools/demoseed/src/lib.rs:516`) and `match.report` sends `ReportId`/`Winner`/`Loser` (`:498-502`). Every body key a step adds is confirmed against its handler/contract before coding.

### Invariants

- **I1 One status authority, one route authority.** Every call goes through one `Gateway::call(op, path_params, caller, body, expect)` that names the `opscatalog` method (e.g. `"friends.accept"`) and takes the verb and path template from that op's `OPERATIONS` row (filling `{id}`/`{collection}`/`{key}` from `path_params`), so the op name and the route can never disagree. It checks the status against the expected status the caller passes, and returns a typed outcome; it never `bail!`s on a status. 429 is retried with backoff (existing); every other unexpected status is recorded as an error. The existing seeding "409 = already exists" stays only where the convergent seed passes 409 as an accepted status for that call.
- **I2 Errors never abort.** A recorded error marks the dependent steps of that scenario as skipped (recorded with the cause); independent scenarios keep running. Transport errors (refused connection) are recorded the same way. The only early exit: the base URL is unreachable at start (one `/healthz` probe).
- **I3 Coverage from the catalog, at run time, on the declared success only.** At the end, every `OPERATIONS.method` must have at least one call in THIS run whose status equals that op's declared `OPERATIONS.success` (Step 1b); any missing one is an error "not covered" (an op called but only failed is reported as failed, not uncovered; a convergent 409 is neither an error nor coverage). The run-time check is the authority; no hand-written per-phase op list claims coverage. To hold on a re-run, every op the convergent seed can answer with a non-success status (409) or calls only when something is missing (`accounts.register`, `storage.put`, `match.report`, `inventory.grant`, `characters.create`, `groups.create/join/invite/decide/respond`, `friends.accept`) is also called unconditionally, with a fresh identity, by the throwaway scenarios (Step 3). An op name not in `OPERATIONS` is a programming error caught by a unit test over the op-name constants.
- **I4 Shape checks on success.** A success body is deserialized into a local serde struct with the fields listed in Verified facts; a mismatch is an error for that op.
- **I5 Seed vs throwaway.** Seeded players (`demo-NN@demo.local`) get only non-destructive calls. Destructive ops run on guest players created by the run and deleted at the end of their scenario.
- **I6 Bounded parallelism.** One `tokio::sync::Semaphore` sized by `--concurrency` (default 16, below the 32 login slots) bounds in-flight requests; scenarios run as tasks in a `JoinSet`. Ordering inside one scenario stays sequential.
- **I7 Eventual reads poll.** Event-fed reads (notifications, leaderboard, `listCharacter` starter item) poll with a deadline (`--event-timeout`, default 10 s, 250 ms interval); timing out is an error naming the op and what it waited for.
- **I8 Exit code.** 0 only when there are no errors and full coverage; otherwise 1 after printing the report.

## Steps

Steps 1-3 share `tools/demoseed/src`, so they run in order. Each implementer writes its own unit tests in `tools/demoseed/src/tests.rs` (or a sibling `*_tests.rs` the step creates), to the bar "revert the change and the test fails". Verification for every code step, under `safe-verification`: liveness check for cargo/rustc as its own call first, then `cargo build -p demoseed`, `cargo clippy -p demoseed --all-targets -- -D warnings`, `cargo test -p demoseed`, `cargo tree -i aws-lc-rs` (must stay empty). No fleet, no `cargo fmt` (`memory/cargo-fmt-is-not-safe-here.md`; hand-format). Each step is followed by a fresh `core-reviewer` (`model:"opus"`) on its commit plus the cumulative diff; one round.

### Step 1 — Harness: op-named calls, error report, coverage, parallelism `[opus]`

(a) `tools/demoseed/Cargo.toml` (add `opscatalog = { path = "../../opscatalog" }` — or the workspace form the other users use; `serde` with `derive`, workspace; update the dependency comment), `tools/demoseed/src/lib.rs`, new `tools/demoseed/src/client.rs` (Gateway, Caller, call outcome), new `tools/demoseed/src/report.rs` (error list, coverage, summary render), `tools/demoseed/src/main.rs`, tests.

(b) First: Steps 2 and 3 call only the new `call` API and record into the new report.

(c) How:
- `Gateway::call(op: &'static str, path_params: &[(&str, &str)], caller, body, expect: &[u16]) -> CallResult`; verb and path template come from `opscatalog::OPERATIONS` (I1); a missing or extra path param is a programming error (panic), pinned by a test, where `CallResult` is `Ok{status, body: Value}` or `Failed` (recorded). It records `(op, status matched)` into a shared `Report` (`Arc<Mutex<..>>` or a channel — implementer's choice) and an error `{scenario, op, method, path, expected, got, body excerpt ≤400 chars}` on mismatch (I1, I2). 429 retry stays as `MAX_ATTEMPTS`/backoff in `lib.rs` today. `classify` is replaced by the expect-list check; its tests move to the new semantics.
- A `typed<T: DeserializeOwned>` helper turns an `Ok` body into `T` or records a shape error (I4).
- Op-name constants: one module `ops` with a `const` per op name used; a unit test asserts every constant is in `opscatalog::OPERATIONS` (I3).
- Coverage at the end (I3) and exit code (I8): `main.rs` prints the report (counts created/existing per kind as today, then errors grouped by op, then uncovered ops) and returns 1 on any error.
- Parallelism (I6): `--concurrency <n>` (default 16); `run()` keeps today's phase order (players → per-player → matches → friendships → groups) but each phase runs its items as `JoinSet` tasks under the semaphore. Cross-phase dependency stays a barrier (a phase starts after the previous joins). A player whose seed phase failed is skipped in later phases with a recorded cause.
- Startup probe: `GET /healthz`; unreachable → print and exit 1 (I2).
- Port every existing call site to op names and expect lists; seeded flows keep their convergence (409 accepted where today's code treats it as AlreadyExists).
- `USAGE` gains `--concurrency`.
- Tests: route lookup (verb + filled path for an `{id}` op and for `/storage/{collection}/{key}/get`; missing param panics), expect-list check (expected, unexpected, 429 retry path via the pure decision fn), coverage computation (all called / one missing / one called-but-failed), report render lists errors and returns failure, op constants ⊂ `OPERATIONS`, `parse_args` for `--concurrency` (0 and non-number rejected).

Commit: `feat(demoseed): record errors per op, check catalog coverage and run in parallel (demoseed-client-simulator step 1)`.

### Step 1b — Coverage counts only the declared success status `[opus]`

Owner decision 2026-11-22 (Step 1 review finding 2): an expect list mixed "not an error" with "proves the op works", so a re-run covered `accounts.register` (and `storage.put`, `match.report`, `groups.join/invite/decide/respond`, `friends.accept`) with a 409 alone, and the lists were a hand copy of `#[http(success=…)]`.

(a) Touched: `opscatalog/src/lib.rs` (`OpInfo` gains `pub success: u16`), `tools/opscatalog-gen/src/main.rs` (`Row` + `render` emit `op.success` from `opsapi::Operation::success`, `core/opsapi/src/lib.rs:298`), `tools/opscatalog-gen/src/tests.rs`, regenerated `opscatalog/src/generated.rs` (`cargo run -p opscatalog-gen`; `verifyctl` `codegen-freshness` diffs it, `tools/verifyctl/src/stages/codegen.rs:58`), `tools/demoseed/src/{client,report}.rs` and their tests. Other `OpInfo` readers (`modules/apikeys/src/tests.rs`, `cmd/gateway-svc/tests/boots.rs`) must still compile.
(b) Before Step 2, because Steps 2 and 3 write their expect lists and re-run coverage on this rule.
(c) How: coverage (I3) counts a call only when its status equals that op's `OPERATIONS.success`. A status in the caller's expect list that is not the declared success (a convergent 409) is "not an error, not coverage". A call's expect list must contain the op's declared success unless the call deliberately expects a refusal (e.g. `me` → 401 after delete); a test pins that `success` is filled for every row (no 0).
(d) Tests: coverage ignores an expected 409; a declared-success status covers; the generator emits `success` (golden/unit in `opscatalog-gen`). Verification adds `cargo test -p opscatalog-gen`, `cargo check -p apikeys -p gateway-svc --tests` (compile only, no Postgres).

### Step 2 — Seeded players: every non-destructive read and write `[opus]`

(a) `tools/demoseed/src/lib.rs` (and a new `tools/demoseed/src/seed.rs` if the implementer moves the seeding flows out of `lib.rs` as one purposeful unit), `plan()` additions, tests.

(b) After Step 1 (uses `call`/report). Before Step 3, which adds the throwaway scenarios to the same `run()`.

(c) How — added to the seeded run, deterministic content from `plan()`, all convergent:
- accounts: `me` (exists). characters: `list` (exists). inventory: `listCharacter` for each seeded character, polling until the starter item appears (I7).
- wallet: `listCurrencies` (expect `gold` and `gems` present — flag hint in the error text when empty) and `myBalances` (shape only) once per player.
- leaderboard: after the match phase, `topScores` polled until the plan's top winner's player id is present (I7).
- friends: when this run's `request` returned state `pending`, the target's `pending` must list the edge as `incoming` before the accept (on a re-run the edge is already accepted and absent from `pending`, `modules/friends/src/service.rs:643-645`, so no presence check then); `list` for both sides afterwards (shape + the friend present). The plan gains a few deliberately pending requests (no accept) so the admin view shows pending edges; pending pairs are disjoint from accepted pairs **as unordered pairs** (a reverse request auto-accepts), count chosen in `plan()`, pinned by a test.
- notifications: only for friendships this run accepted, the requester's `list` is polled until a `friend.accepted` row exists (I7); older friendships get a shape-only `list` (rows expire after 30 days, `modules/notifications/src/projection.rs:55`, and subscriptions start `AfterRegistration`). `markRead` on the first unread row of each seeded player that has one (idempotent).
- groups: `members` for each group (as the owner), `pending` for each group (as the owner); the plan gains one `request` group with a pending, undecided join (re-run: the join answers 409, accepted for that call, `modules/groups/src/service.rs:477`). Every expect list on a seeded re-call (`groups.invite`, `groups.decide` on an existing member) is confirmed against the handler.
- storage: a second save per player with `acl:"public_read"`; `get` own save, `get` another player's public save by `owner_id`, `list` own collection.
- Tests: plan determinism still holds with the new parts; pending friend pairs are disjoint from accepted ones as unordered pairs (and contain no reverse of each other); the public-save reader is never the owner; markRead selection is stable.

Commit: `feat(demoseed): read back every seeded kind and seed pending edges and public saves (demoseed-client-simulator step 2)`.

### Step 3 — Throwaway client scenarios: destructive and session ops `[opus]`

(a) New `tools/demoseed/src/scenarios.rs`, `lib.rs` `run()` wiring, `USAGE` (`--clients <n>`, default 8), tests.

(b) After Step 2; closes coverage of the remaining ops.

(c) How — `--clients` independent scenario tasks run concurrently with each other (I6), after the seeded phases; each uses guest players it creates and deletes (I5). A scenario that loses a step records it and skips its dependents (I2). Per scenario, in order:
1. Session: `createGuest` ×3 (A, B, C) → A `loginFederated` (`guest`, A's `device_secret`) → A `refresh` with the newest refresh token (once, never replayed) → A `link` (`guest`, own secret; expect 200) → `me` for A, B, C (handles).
2. Characters/inventory: A `inventory.grant` (`coin`, qty 1; 200) → A `characters.create` → `listCharacter` polled for the starter item → `characters.delete` (204) → `characters.list` does not contain it.
3. Storage: A `put` → `get` → `delete` (204) → `get` expects 404.
4. Friends: A requests B, B `decline` (204); A requests C, C `accept` (204), A polls `notifications.list` for `friend.accepted` (I7), A `remove` (204); B polls `notifications.list` for `friend.requested`, `markRead` it, then `notifications.delete` on it (204).
5. Groups: A creates an `open` group, B joins, B `leave` (204), A `leave` (204, sole member deletes the group). A creates an `invite` group, invites C, C `respond` `accept` (204), C `leave`, A `leave`. A creates a `request` group, B `join` (pending), A `decide` approve (204), B `leave`, A `leave`.
5b. Match: `match.report` (server key) with a fresh per-scenario `ReportId`, A wins over B (202).
6. Party: A `create` (201) → A `invite` B and C (201) → B `invites` shows it → B `accept` (200) → C `decline` (204) → A `heartbeat` → B `setReady` `ready` → A `promote` B → B `kick` A → B `get` → B `leave`.
7. Deletion, last: for A, B, C, D `beginDelete` → `deleteAccount` (200) → `me` expects 401 (recorded under `accounts.me`, expected status 401).
- Register: each scenario also `register`s a fresh account D (email unique per run and scenario, e.g. `sim-<run nonce>-<n>@demo.local`; 201) → `login` → `me`, and D is deleted in item 7 with A, B, C. Every op I3 lists as re-run-sensitive is thus called unconditionally here with a fresh identity and its declared success.
- The handler facts the researchers left as hypotheses are confirmed against code before coding: guest `loginFederated` path (`modules/accounts/src/guest.rs:132-153`), `link` with own guest secret → 200, party accept when already in a party.
- Tests: pure scenario-step ordering (deletion last, refresh token used once) if extracted as data. Coverage itself is proven at run time (I3), not by a hand list; the live run in Step 5 (run twice) is its acceptance.

Commit: `feat(demoseed): drive throwaway client scenarios over the remaining public ops (demoseed-client-simulator step 3)`.

### Step 4 — Docs `[docs]` (sonnet)

(a) Exact files: `.agents/shared/gamebackend.md` **Commands** (build `demoseed` before `devctl up monolith`, run `target/debug/demoseed` with `--concurrency`/`--clients`, monolith only, exit code meaning); `.agents/skills/mockup-implementation/SKILL.md:245,305-307,330` (keep the run lines correct); `docs/todo/demoseed.md` (Last updated; nothing closed unless the steps closed it); `docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md` Step 24b: an errata note dated 2026-11-22 that this plan replaces 24b0-24b4, that the 24b3 pending mail is dropped (owner decision 4: no public mail route), and that the accept-entry cleanup moves to Step 5 here; the `demoseed` crate header doc in `tools/demoseed/src/lib.rs`.
(b) Last code-facing step, against landed code.

### Step 5 — Owner live run and accept-entry cleanup `[inline]`

Owner runs, in order: `cargo build -p demoseed` → `cargo run -p devctl -- up monolith` → `target/debug/demoseed` twice (the second run must create no new seeded data and still report full coverage). Failures are reported, not fix-looped (`memory/live-acceptance-report-dont-fix.md`). If register/login time out under `--concurrency 16` (2 argon2 permits, 30 s request timeout `tools/demoseed/src/lib.rs:23`), that is reported with the measured time, not patched in place. Then, under `safe-verification`, on the same fleet: `admindiff crawl` + `diff`, and delete only the `data difference` entries in `tools/admindiff/accept/*.txt` that the crawl no longer produces (the former 24b4). Rows that vary per run because of the throwaway guests (deletion receipts, audit rows, tombstones until the 5-minute purge sweep) are reported to the owner, never accepted as new entries; that commit gets a `proof-auditor` (accept files are a gate). The uncommitted `tools/admindiff` changes in the working tree today belong to other work and are not touched.

## Not checked / open for implementers

- Body-key spelling for every new body (see Verified facts) — each step confirms against the handler before coding.
- The `listCharacter` and leaderboard event delay was not measured; the 10 s default deadline is a guess, adjustable by flag.
- Party accept of a player already in a party: inferred 409 from constraint mapping; the scenario avoids that state.
