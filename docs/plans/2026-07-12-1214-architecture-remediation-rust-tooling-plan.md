# Architecture remediation and Rust tooling plan

**Status:** REVIEWED — independent think-hard punch list incorporated; remaining work pruned for minimum sufficient fixes

**Date:** 2026-07-12

**Scope:** close every finding from the 2026-07-12 architecture review and replace the fragile run/verify shell orchestration with owned Rust processes.
**Non-goals:** no production data migration machinery, no topology changes, no event-contract mutation, no changes under `experiments/`, and no runtime implementation as part of this planning task.

## Outcome and safety rules

The target is not merely “green tests.” The resulting system must preserve the documented modular-monolith/split equivalence, fail closed at boot for invalid wiring, keep account/event/session effects atomic, avoid killing unrelated OS processes, and ensure that a fix in one layer does not silently weaken another.

Implementation must obey these rollout constraints:

1. At most one `cargo test`, `verifyctl`, or `splitproof` run may execute at once. Before every such run, check `Get-Process | Where-Object { $_.ProcessName -match '^cargo$|^rustc$' }`; wait rather than starting a second rollout. If a run appears stuck, inspect orphaned test processes and `pg_stat_activity` before retrying.
2. Each step below gets its focused tests before the next risky step. Do not bundle unrelated fixes into one patch merely to reduce commit count.
3. Do not bless public API or contract goldens to make an unexplained diff green. Inspect and record the exact intended diff first.
4. All DB-shape changes use the repo's wipe-and-reseed policy. This plan intentionally introduces no migrations, backfills, dual writes, or compatibility bridges.
5. `run.*` and `verify.*` are removed only after the Rust replacements pass parity checks. Until then they may be reduced to thin forwarders, never independently maintained implementations.
6. Dev tooling uses a trusted-local-operator threat model. Its acceptance criteria are backend coverage, accurate outcomes, bounded accidental-failure handling, exact owned-process cleanup, and no secrets in argv/logs/state. Same-account adversarial security, custom cryptography, reparse-point defenses, and daemon-grade control protocols are out of scope unless they directly reproduce a backend-test failure.
7. A remaining step may introduce a new subsystem only when the failure cannot be closed at an existing authority. Prefer a hidden expected-state field and conditional SQL over server-side form state, a hard capacity plus the existing background reaper over request-path coordination, and representative boundary tests over combinatorial matrices.

## Git-history evidence: where prior fixes created the next failure mode

The plan deliberately does not repeat the recent patch-on-patch pattern:

- `addc824` added the scheduler's dedicated connection, aggregate tick budget, and bounded stop. Those protections stay; step 12 changes only fairness inside that budget and reaping after abort.
- `7ca0b51` added retention staleness and its error counter. The new finding is at the seam it introduced: a partial per-topic failure still reports a successful pass and the app hardcodes a threshold independent of the configured sweep interval. Step 11 creates one config/liveness authority rather than layering a second check.
- `ace9e96` kept a healthy remote connection after a definitive second-attempt answer, but the earlier first-attempt path still closes the shared connection after errors whose connection provenance has already been erased. Step 18 carries provenance from Quinn instead of adding another status heuristic.
- `8eba714`, `1546780`, `b115881`, and `f274ac4` rolled out conformance contracts, module entries, and a blocking verify stage in quick succession. The result can self-attest with `NotApplicable` and enters production dependency graphs. Step 8 moves policy to the checker and makes gaps mechanically red before adding more entries.
- `b78444f` correctly made a failed tool installation fatal, but does not make a later cargo-audit network/invocation failure fatal. Step 5 defines one typed outcome path for both installation and execution.

These commits are evidence for preserving the good invariant from each earlier fix while replacing the flawed authority, not reverting the whole change or adding another special case beside it.

## Decisions fixed by the research

- The shared low-level tooling crate is `tools/processctl`; the user-facing binaries are `tools/devctl` and `tools/verifyctl`.
- `splitproof` stays a subprocess stage of `verifyctl`. This contains panics, dependency state, logs, and cleanup; an authenticated inherited rollout lease prevents false lock contention.
- `devctl up` is a foreground supervisor. `status` and `down` communicate with that supervisor over an authenticated loopback control channel; they never signal a PID read from a stale file.
- Local control authentication means ordinary OS-local ownership plus exact supervisor identity, not a new security protocol. The channel must bound frames and waits so an accidental partial client cannot hang cleanup; it is not required to withstand a malicious user running under the same account.
- Process-tree containment is supported explicitly on Windows and Linux, the two platforms exercised by this repository. Other Unix targets fail startup as unsupported instead of receiving a weaker `/proc`-based implementation under a misleading “Unix” name.
- Config admin edits carry the authoritative global `config.revision` as a hidden optimistic concurrency token. The page is a full coherent snapshot, so any intervening config change rejects the entire stale form instead of overwriting newer state.
- API-key admin edits carry per-row expected state in hidden fields and use an all-or-nothing transaction. These values are concurrency tokens, not secrets; existing admin authentication and CSRF remain the trust boundary. A stale row rejects the whole submitted batch.
- Match replay semantics remain idempotent: validate `ReportId` syntax first, then allow an exact existing report to replay even if newer participant policy would reject it; new reports receive the full participant validation.
- New textual byte caps are private domain policy unless a cross-layer fast rejection requires a shared contract constant. Proposed caps: session bearer 128 bytes, accounts display name 128 bytes, Epic ID token 65,536 bytes, character name 128 bytes, character class 64 bytes, match report id 128 bytes, winner 128 bytes, loser 128 bytes. These are byte caps, deliberately far below the 1 MiB transport/body ceiling and above realistic values.
- A retention pass is healthy only when every eligible topic sweep succeeds. Partial failure still attempts later topics but does not refresh liveness.
- Scheduler fairness is deterministic round-robin over a stable `ORDER BY name`; skipped rows after budget exhaustion do not advance the cursor.
- Remote retry/reset is based on transport provenance, not mapped `opsapi::Status`. Stream-local and application errors must not close a shared QUIC connection.
- Conformance policy belongs in the conformance tool, not the production dependency graph. Modules expose only minimal factual probes with no feature flag and no conformance types, avoiding workspace feature leakage.

## Finding closure map

| Review finding | Closing steps |
|---|---|
| broad `taskkill /IM` / `pkill -f`; PID-only teardown; partial-boot orphans | 1–4, 6–7 |
| PowerShell environment accumulation and shell quoting/job-control fragility | 3–4, 7 |
| audit network failure reported as successful blocking SKIP | 5 |
| public API/codegen/C# stage drift and unsafe cleanup | 5–6 |
| retention partial errors counted healthy; fixed 3h; duration overflow | 11 |
| scheduler starvation and aborted tasks not reaped | 12 |
| conformance false `NotApplicable` and production coupling | 8, 17 |
| RPC auth/identity, invalid success code, serialization-to-null | 9 |
| heterogeneous contributions and duplicate invalidation names fail late | 10 |
| session token DB amplification, non-atomic registration/login | 13 |
| Epic OAuth state not bound to browser | 14 |
| config/API-key lost updates and misleading anti-TOCTOU claim | 15 |
| NaN/infinite/negative rates; unbounded IP limiter | 16 |
| missing domain caps; empty/self match participants | 17 |
| stream-local error closes shared remote connection | 18 |
| stale commands, architecture/tooling claims, source comments and old plan status | 19 |

## Ordered implementation plan

### 1. Introduce exact process ownership in `processctl` `[subagent-complex]`

**Files/symbols:** root `Cargo.toml`; new `tools/processctl/Cargo.toml`, `src/lib.rs`, `src/process.rs`, `src/bin/processctl-guardian.rs`, `src/platform/mod.rs`, `src/platform/windows.rs`, `src/platform/linux.rs`, `src/tests.rs`; existing process helpers in `tools/splitproof/src/main.rs` only as behavioral reference.

Implement `SpawnSpec`, `OwnedChild`, `ProcessIdentity`, `StartMarker`, `ShutdownPolicy`, and `ShutdownOutcome`. `SpawnSpec` carries typed `Vec<OsString>` arguments, `BTreeMap<OsString, OsString>` environment, cwd, log destinations, label, and process-group policy—never a shell command string. `OwnedChild` retains the real child handle, exposes `try_wait`, and performs graceful signal, bounded wait, force termination of its owned process group, and final reap. Its `Drop` path must be a last-resort force-kill-and-reap guard.

On Windows, create each process suspended, assign it to a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, then resume it; also create a process group and use `CTRL_BREAK_EVENT` for graceful shutdown. Identify it with PID + `QueryFullProcessImageNameW` + creation time. On Linux, launch the target through the fixed `processctl-guardian` helper as a new process-group/session boundary. Before spawning the guardian, create a liveness pipe: the supervisor owns only the write end; the guardian inherits only the read end; both ends are close-on-exec/non-inheritable for the target, Cargo, services, and every descendant. The guardian closes unrelated descriptors, becomes a child subreaper, watches read-end EOF plus the target pidfd, forwards graceful signals, kills the owned group on supervisor EOF, and reaps descendants before exiting. The supervisor retains a pidfd for race-free guardian signaling/waiting and validates PID + `/proc/<pid>/exe` + `/proc/<pid>/stat` start time for persisted state. Other Unix targets return an unsupported-platform error. An unsupported or unverifiable identity fails closed; no basename matching and no global image/pattern kill is permitted.

**Why first:** every later tool and splitproof cleanup relies on this primitive. Implementing supervisors before exact ownership would reproduce the original bug in Rust.

**Focused verification:** unit tests with short-lived fake children for graceful exit, ignored graceful signal, force-kill, descendant cleanup, already-exited child, PID/start-marker mismatch, and idempotent repeated shutdown. Assert the target cannot see either liveness-pipe end and cannot keep the pipe alive. A crash test must terminate the supervisor without executing Rust cleanup and prove EOF reaches the guardian, the entire owned child/grandchild tree dies, and an unrelated decoy remains alive.

### 2. Add atomic state and an inherited rollout lease `[subagent-complex]`

**Files/symbols:** `tools/processctl/src/state.rs`, `src/lock.rs`, exports from `src/lib.rs`, tests in `src/tests.rs`.

Add versioned `FleetState`, `ManagedProcess`, and `StateStore`. Write state after every successful spawn through same-directory temporary write; record only labels, exact identities, status, control endpoint, log paths, run id, and topology. Never serialize environment maps, database URLs, passwords, bearer tokens, or private key material. Unix creates mode 0600, flushes the file, atomically renames, and fsyncs the parent directory. Windows applies an owner-only DACL before content is exposed, flushes with `FlushFileBuffers`, and replaces with `ReplaceFileW`. Failure to establish owner-only access is fatal. If the post-spawn state checkpoint fails, immediately stop/reap the new child and the already-started prefix.

Add `RolloutLock` backed by an OS advisory file lock. Its metadata records owner identity, run id, start time, and the allowed borrower role. Transfer the one-shot borrower credential through an inherited anonymous pipe/handle, not process environment or argv. A synchronous pre-runtime entry point consumes the credential to EOF, validates the live owner and role, closes the handle, and creates a non-transferable `BorrowedLease`; descendant handles are non-inheritable. Direct `splitproof` and `devctl up` acquire an owned lease normally. A borrowed lease cannot release, re-borrow, or transfer the parent's lock.

**Why now:** this replaces the unreliable “scan cargo processes” as machine-enforced rollout ownership while retaining the manual scan before raw Cargo commands, which can compile before application code acquires the lock.

**Focused verification:** concurrent owner rejection, one valid inherited borrower, second/replayed borrower rejection, wrong role, dead owner, and proof that Cargo, a fake service, and its grandchild cannot observe the credential or inherit its handle. Also test state recovery at every write/replace failure, failed post-spawn checkpoint rollback, stale identity never signaled, owner-only permissions, and redaction over serialized state and lock metadata.

### 3. Extract the canonical split fleet and migrate `splitproof` `[subagent-complex]`

**Files/symbols:** new `tools/processctl/src/fleet.rs`; `tools/splitproof/Cargo.toml`; `tools/splitproof/src/main.rs` (`Svc`, `fleet`, `Running`, platform signal helpers, monolith launch and I-GATE lookup); optionally split into `src/lib.rs` plus a thin `src/main.rs` for focused tests.

Create `FleetInputs`, `FleetFlavor::{Development, Proof}`, `ServiceSpec`, `FleetSpec`, and `game_backend_fleet`. Encode the current 12-service names, executable packages, HTTP/edge/player ports, dependencies, and typed environment exactly once. Proof-only timeouts/seeds belong in the `Proof` overlay. Preserve disk drift validation against `cmd/*-svc`, but make scenario lookup by stable service name rather than vector index.

Replace `splitproof`'s child/PID/signal implementation with `OwnedChild`, and its constants with `game_backend_fleet`. Its synchronous `main` consumes the optional borrowed handle before creating the Tokio runtime. Every Cargo/service/monolith `SpawnSpec` uses `env_clear` and explicitly non-inheritable handles. Preserve all named split and monolith assertions, health checks, DB assertions, cleanup-on-panic, Ctrl-Break/SIGTERM graceful-drain proof, and log capture. A direct invocation owns the lease.

**Why before the new launchers:** `splitproof` is the strongest executable specification of the fleet. Sharing its already-proven topology prevents `devctl` from inventing a second one.

**Focused verification:** pure fleet snapshot/drift tests and process fake tests first; then one direct `cargo run -p splitproof` only after checking no rollout is running. Compare named assertion output with the pre-refactor baseline.

### 4. Build `devctl` as a foreground supervisor `[subagent-complex]`

**Files/symbols:** new `tools/devctl/Cargo.toml`, `src/main.rs`, `src/cli.rs`, `src/supervisor.rs`, `src/control.rs`, `src/tests.rs`; `tools/edgeca` library surface if needed; existing `run.sh`/`run.ps1` as parity specifications only.

Provide `devctl up [monolith|split]`, `status`, and `down`; retain `microservices` as a one-release warning alias for `split`. `up` acquires `RolloutLock` for the supervisor lifetime, prepares the dev CA via a library call, seeds the admin user by invoking the built `adminctl` with password on stdin, launches from the canonical fleet, performs health checks, and stays foreground. `--skip-build` skips only the build phase.

Build every command with `env_clear`. Define and snapshot-test one `BUILD_ENV_ALLOWLIST` containing exactly `PATH`, `PATHEXT`, `SYSTEMROOT`, `WINDIR`, `COMSPEC`, `USERPROFILE`, `HOME`, `TEMP`, `TMP`, `CARGO_HOME`, `RUSTUP_HOME`, `RUSTFLAGS`, `CARGO_TARGET_DIR`, `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`, their lowercase variants, `SSL_CERT_FILE`, `SSL_CERT_DIR`, `CARGO_HTTP_CAINFO`, `CARGO_HTTP_PROXY`, `GIT_SSL_CAINFO`, and `CARGO_NET_GIT_FETCH_WITH_CLI`. The service allowlist is the platform subset required to start plus `RUST_LOG`, `RUST_BACKTRACE`, `DATABASE_URL`, and the documented app variables present in its typed `ServiceSpec`; it does not inherit Cargo/proxy variables by default. Caller overrides are accepted only for keys declared overrideable by that `ServiceSpec`. Precedence is platform baseline < typed fleet defaults < one immutable validated override snapshot < proof-only overlay. Reject unknown override keys and never log their values. The same environment builder is used by devctl and splitproof.

Expose a bounded local control endpoint: a local named pipe on Windows and a mode-0600 Unix-domain socket on Linux. The state file stores only its address, never a bearer token. `status` and `down` validate the recorded live supervisor before connecting and never kill recorded PIDs. Frames, reads, writes, and shutdown waits have small fixed bounds so malformed or partial local input cannot stall a rollout. On any child startup/health failure or unexpected later exit, mark the state, stop the successfully started prefix in reverse order, reap it, and exit nonzero. Ctrl-C and `down` perform the same bounded graceful teardown. The Windows Job Object or Linux guardian remains the crash safety net. Do not add cryptography or same-user adversarial defenses.

**Focused verification:** fake-service integration tests for environment isolation, topology switching, partial failure rollback, unexpected child exit, graceful/hung child, state progression after each spawn, stale-state decoy protection, shared rollout-lock contention, secret redaction, bounded partial control frames, and bind readiness. Runtime verification is Windows-only on the current machine; Linux receives cfg/static compile review without WSL claims. Then manually smoke `up monolith`, `status`, `down`, `up split`, and an injected failed service.

### 5. Build the typed `verifyctl` runner and preserve blocking semantics `[subagent-complex]`

**Files/symbols:** new `tools/verifyctl/Cargo.toml`, `src/main.rs`, `src/lib.rs`, `src/cli.rs`, `src/model.rs`, `src/runner.rs`, `src/stages/{mod.rs,command.rs,audit.rs,fortress.rs,splitproof.rs}`; root workspace members.

Define the closed enums `StageId`, `StageClass::{Blocking,Advisory,Slow}`, `Outcome::{Pass,Fail,Skip}`, `SkipReason`, `Summary`, and CLI actions `Verify`, `BlessPublicApi`, `BlessContractGolden`. Preserve user modes as `--fast` default, `--all`, `--strict`, `--slow`, and `--no-install`, with mutually exclusive bless actions. `verifyctl` is a static ordered list of stage functions, not a plugin framework; do not add dynamic registration or a generic orchestration protocol.

The initial exact stage order is: build, clippy with `-D warnings`, test, audit, fortress/archcheck, routecheck, split-proof; step 6 inserts the remaining custom stages in the fixed positions below. `verifyctl` owns the rollout lease for its whole run and lends one non-transferable handle only to splitproof. Blocking SKIP is legal only when the user explicitly selected `--no-install` and a required tool is absent. Typed `NotApplicablePlatform` is also an accepted SKIP for a stage that cannot run on the current platform (notably fuzz on Windows), including under `--strict`; strict promotes an advisory `Fail`, not platform non-applicability. Accept any installed `cargo-audit`; if absent and installation is allowed, install the latest available release with `cargo install cargo-audit --locked`. Cargo-audit install failure, invocation failure, or network failure is FAIL, never successful SKIP. No other advisory SKIP becomes green merely because the stage was hard to run.

Exit codes are stable: 0 green, 1 one or more stage failures, 2 CLI/orchestration/lock error, 130 interruption. Each run gets a run-id log directory and a deterministic PASS/FAIL/SKIP table.

**Focused verification:** fake executables on a temporary `PATH` exercise every outcome, especially audit network failure, install failure, strict advisory promotion, Windows fuzz `NotApplicablePlatform` under strict, explicit no-install, interruption cleanup, lease ownership, and summary/exit-code matrices.

### 6. Port the custom verification stages without unsafe cleanup `[subagent-complex]`

**Files/symbols:** `tools/verifyctl/src/stages/{public_api.rs,codegen.rs,contract_golden.rs,conformance.rs,csharp.rs,topiccheck.rs,fuzz.rs,mutants.rs}`; existing logic in `verify.sh`, `verify.ps1`, contract/codegen tools, and `csharp-client/` fixtures.

Make the complete stage manifest:

- blocking/default: build, clippy, test, audit, fortress, routecheck, codegen-freshness, contract-golden, conformance with `--deny-gaps`, split-proof;
- advisory under `--all` and blocking under `--strict`: public-api, fuzz, C# client, topiccheck;
- slow under `--slow`: mutants.

Derive public contract crates from parsed workspace manifests under `api/*/{api,events}`, sort deterministically, detect missing and orphan baselines, and render all proposed outputs to a temporary tree. Ordinary verification never edits tracked files. An explicit bless precomputes and validates the complete set, then performs a recoverable two-phase replacement with a backup manifest and startup recovery; on any failed replacement it rolls back already-replaced files. Codegen freshness likewise generates into a temporary tree and recursively diffs tracked output.

Port all C1–C6 C# assertions, but launch its fixture with `OwnedChild`, fail if the requested port is already occupied, drive it with reqwest, and clean up only that exact owned child/group through RAII. Delete every `taskkill /IM server.exe` and `pkill -f` behavior rather than translating it.

**Focused verification:** stage-manifest golden, TOML discovery ordering, missing/orphan baseline, recoverable bless rollback at every replacement point, codegen no-mutation, C1–C6 predicate fixtures, occupied port, child crash, timeout, and decoy `server.exe` survival.

### 7. Establish tooling parity before touching runtime behavior `[subagent-mechanical]`

**Files/symbols:** root `verify.sh`, `verify.ps1`, `run.sh`, `run.ps1`; new tooling README/help text; existing splitproof named-output snapshots.

Temporarily reduce the four scripts to argument-preserving forwarders to `devctl`/`verifyctl`; they must contain no process discovery, environment composition, stage logic, or cleanup. Record a short old-command to new-command mapping table and run one `verifyctl --fast` after the new tools' focused tests and after the mandatory no-concurrent-rollout check. Do not build a snapshot/parity framework for the wrappers.

Do not proceed to runtime fixes until build/clippy/test/audit/fortress/custom gates/splitproof all preserve their intended blocking behavior. This creates a trustworthy safety net before high-risk behavior changes.

### 8. Make conformance honest and remove it from production graphs `[subagent-complex]`

**Files/symbols:** `core/conformance/src/lib.rs`; new ordinary library crate `tools/rpc-contract-model/`; `tools/rpc-macro/src/lib.rs`; the existing C# generator parser; new `tools/conformance/src/policy/*.rs` and input inventory module; affected module `Cargo.toml` files and validators; `tools/verifyctl/src/stages/conformance.rs`; `docs/reference/public-api-baseline/` only if an intentional public contract actually changes.

Replace the binary `Stance` with `Applies`, `NotApplicable { why }`, and `KnownGap { why, remediation }`. `KnownGap` fails `--deny-gaps`; a separate explicit `--allow-known-gaps` research mode prints GAP and never OK. Do not build waiver, decision-id, or expiry machinery until the project has a real approved exception that needs it.

Remove normal conformance dependencies from every domain and command graph. Put policy entries and all `Stance` closures in `tools/conformance`; module crates never depend on `core/conformance`. Where the tool must exercise a domain validator, expose one minimal `#[doc(hidden)] pub` factual probe from that module containing no conformance types or policy. Do not use a Cargo feature: workspace feature unification could otherwise compile adapter behavior into shipping binaries. Add one automated dependency assertion that enumerates the shipping roots and proves they exclude conformance; do not maintain a hand-run cargo-tree ritual per service. The only module-to-tool direction is `tools/conformance` depending on the module probes.

Extract the existing `#[rpc]` syntax parser and semantic model into the ordinary `rpc-contract-model` library. Both the proc macro and the C# generator consume that library; delete their duplicated parsing paths. The conformance inventory consumes the same model and snapshots only its output, so the golden is drift detection rather than a second authority. Limit the inventory to externally or wire-reachable string request fields. Each maps to one of: concrete validator/cap, opaque token cap, intentionally unrestricted with rationale, or known gap. Initially record the missing caps as `KnownGap`; step 17 closes them before the next full fast verification.

**Focused verification:** false `NotApplicable` cannot pass, a known gap fails `--deny-gaps`, an omitted externally reachable field breaks the inventory golden, probes call the same production validator, and one dependency-graph assertion covers all shipping server/service roots.

### 9. Enforce RPC metadata invariants at the generator boundary `[subagent-complex]`

**Files/symbols:** `tools/rpc-macro/src/lib.rs` parser/expansion; existing `tools/rpc-macro-tests/` compile-fail/runtime harness; generated rpc crates only through the normal codegen-freshness flow.

At macro expansion, require `auth = player` if and only if the operation's leading parameter is `Identity`; emit a compile error for either mismatch. Accept success status codes only in 200–299 and reject invalid metadata instead of silently falling back to 200. When response serialization fails, generate an Internal error envelope; never convert it into transport success with JSON `null`. Keep a legitimate `Ok(None)` response unchanged.

**Why here:** this is one authoritative seam and prevents dozens of generated handlers from drifting independently.

**Focused verification:** use the existing compile-fail harness if it can cover both auth/identity directions and invalid success codes; do not add `trybuild` merely for form. Add one runtime fixture with a deliberately failing `Serialize` and one control proving `Option::None` remains a successful null payload. The blocking codegen-freshness stage is the permanent diff authority; inspect an actual generated diff only when one appears.

### 10. Fail wiring errors at construction, not on a request `[subagent-complex]`

**Files/symbols:** `core/contrib/src/lib.rs` and tests; lifecycle `Context::{contribute,contributions}`; typed constants in `core/{opsapi,edge,httpmw,remote}` and `api/admin/api`; every call site returned by `rg "contribute\(|contributions\(" core modules cmd demos`; `tools/archcheck`; `core/invalidation/src/lib.rs` and tests.

Replace string-only slot parameters with `contrib::Slot<T>`: `Context::contribute<T>(slot: Slot<T>, value: T)` and `Context::contributions<T: Clone>(slot: Slot<T>)`. The owning contract/core crate defines each canonical typed constant: admin items, `opsapi::{SLOT,BINDING_SLOT,LOCAL_SLOT,PEER_SLOT}`, `edge::EDGE_SLOT`, readiness, HTTP layers, and remote boot hooks. Providers and consumers use the same constant even when the consumer is absent from a split service, so topology does not affect admission. A wrong value for the first/only producer is a compile error, and request-time downcasts disappear. The internal map still records each canonical key's `TypeId` and rejects a conflicting forged key. Extend archcheck to reject construction of slot literals outside the owning contract/core files, ensuring modules cannot bypass the canonical constant.

Make invalidation registration reject empty channel/name and duplicate callback names globally, even across channels, before mutating its registry. Global uniqueness is required because health state and metrics are keyed by name; a collision would otherwise mask one callback.

**Focused verification:** one representative compile-fail wrong-value fixture, runtime forged-key collision failure, valid multi-value collection, archcheck rejection of module-local slot construction, standalone fortress boots without an admin/gateway consumer, duplicate same-channel and cross-channel invalidation registration, empty identifiers, and unchanged first-refresh startup semantics. Do not duplicate equivalent compile-fail fixtures for every slot owner.

### 11. Correct retention health and duration arithmetic `[subagent-complex]`

**Files/symbols:** `core/asyncevents/src/retention.rs`, `src/retention_tests.rs`, `src/lib.rs` (`Plane::start`, `Liveness`); `core/app/src/lib.rs` (`RETENTION_STALL_MAX`, readyz message).

Parse the existing operator knob `EVENTS_HOUSEKEEP_INTERVAL` once into an internal config containing `interval` and checked `stall_after = interval * 3`. Do not rename or alias it. Use checked arithmetic for hour/minute/second multiplication and the final sum; reject zero and overflow as startup errors while preserving the documented default for an absent value. Preserve subsecond precision in readiness messages/tests.

Change `sweep` to visit every topic, collect per-topic failures, and return an aggregate error after the pass. Increment the failed-sweep metric exactly once per failed pass, not once per topic and again in the caller. Refresh `retention_last_success` only on complete success. Move the derived threshold into plane/liveness state and remove the hardcoded app-level three hours and second environment authority.

**Focused verification:** 500ms produces a 1500ms stale threshold; 5m and 4h parse correctly; overflow and zero fail startup; topic A failure does not prevent B sweep, but the pass returns Err, liveness is not refreshed, and error metric increments once; full success refreshes it.

### 12. Make scheduler budget fair and cancellation fully reaped `[subagent-complex]`

**Files/symbols:** `modules/scheduler/src/lib.rs` (`DUE_SQL`, `tick`, task loop, `stop_tasks`); `modules/scheduler/src/tests.rs`.

Add `ORDER BY name` to `DUE_SQL` and maintain the last actually attempted schedule name as the scheduler loop's cursor. On each stable due list, start at the first name lexicographically greater than the cursor, wrapping to the beginning; if the prior name disappeared, use that insertion point. Update the cursor when an actual schedule attempt begins. If the shared 30-second tick budget expires, break without advancing past unattempted rows. Preserve the dedicated connection, advisory-lock recheck, update + `emit_tx`, commit-before-unlock, and exactly-once behavior.

When the four-second module stop grace expires, call `abort` and then await each same `JoinHandle`; accept cancellation as expected and log any other join error. Never discard an aborted handle while its cleanup may still run.

**Focused verification:** a pure rotation helper covers wrap and a removed cursor; two successive ticks prove a slow first name cannot starve the next schedule; abort is awaited and the dedicated connection Drop guard runs before stop returns. Existing exactly-once integration tests remain the regression proof; do not add a multi-replica scheduler simulator.

### 13. Bound session lookup and make account creation/login atomic `[subagent-complex]`

**Files/symbols:** `api/accounts/accountsapi/src/lib.rs`; `modules/accounts/src/store.rs` (`player_by_session`, session creation, external identity methods); `modules/accounts/src/lib.rs`; account/gateway tests; public API baseline for accountsapi.

Add `accountsapi::MAX_SESSION_TOKEN_BYTES = 128`. Reject over-cap bearers before SQL in `Store::player_by_session` and before remote dispatch in the gateway session verifier; classify them as invalid credentials/Unauthorized, not infrastructure Unavailable.

Add transaction-taking session helpers that all accept the same `&mut PgConnection`/transaction borrowed by their caller. Registration must insert account/identity, append `player.registered`, create the session, and commit once; it performs no pool read or session insert outside that transaction. Introduce one transactional external-login operation returning `(Session, created)`. Every writer of an external `(provider, subject)` identity—including explicit `link_identity`—must first take the same transaction-scoped advisory lock on a domain-separated stable 64-bit hash of that pair. External login then re-reads the identity: the winner creates player + identity + event; a serialized loser observes the identity and creates only its session. All effects use the same live outer transaction and commit once, avoiding PostgreSQL's aborted-transaction state after a uniqueness error. Use it from Epic API login and browser OAuth login. Keep explicit account linking's authorization semantics separate and do not mark registration retry-safe.

**Focused verification:** an over-cap token performs no store/remote call; an injected session-insert failure rolls back registration and its event; two controlled concurrent first-logins produce one player/event and two sessions; one `link_identity` versus first-login race proves the shared lock and authorized owner. These deterministic transaction/concurrency tests replace separate winner/loser/commit-failure matrices. Inspect and bless only the intended new accountsapi constant.

### 14. Bind Epic OAuth state to the initiating browser `[subagent-complex]`

**Files/symbols:** `modules/accounts/src/epic_oauth.rs` (`OauthState`, start/callback handlers, `take_state`); accounts router/tests; startup validation for `EPIC_REDIRECT_URI`.

Generate or reuse a high-entropy browser-binding cookie and store its binding with each OAuth state. Use a host-only, HttpOnly, `SameSite=Lax`, `Path=/accounts/epic`, `Max-Age=600` cookie with no Domain. Validate `EPIC_REDIRECT_URI` once: it must parse, use HTTPS (or HTTP only for loopback), and use `/accounts/epic/callback`. `Secure=true` for HTTPS and false only for the accepted loopback HTTP case. Do not build a separate hostile-operator URI policy beyond these runtime requirements. Reusing the binding cookie permits parallel tabs while each state remains single-use.

Change `take_state` to require both state and binding. Missing/wrong binding must not consume the legitimate state; matching expired state is removed and rejected. Perform this validation before calling Epic's token endpoint or mutating/linking an account.

**Focused verification:** cookie attributes, parallel starts, matching callback, wrong/missing binding, expired state, replay, and zero token-endpoint calls for rejected callbacks. One split-passthrough smoke proves a cookie set on the public gateway origin returns to the callback route; do not create an exhaustive URI matrix.

### 15. Replace admin snapshot claims with real compare-and-swap `[subagent-complex]`

**Files/symbols:** `api/admin/api/src/lib.rs` (`SubmitFn`, new `SubmitError`, hidden field representation); `modules/admin/src/lib.rs` (`item_post`) and tests; `modules/config/src/lib.rs` admin render/apply code and tests; `modules/apikeys/src/admin.rs` (`PlannedWrite`, render/apply) and tests; adminapi public baseline.

The real portal currently re-renders on POST, which creates a fresh closure and defeats a captured-snapshot CAS. Do not add a server-side form-instance store. Extend the declarative form with hidden expected-state fields and round-trip them through the existing authenticated, CSRF-protected GET/POST flow. They are concurrency tokens, not secrets. Remote forms remain read-only.

Change `SubmitFn` to return `Result<(), adminapi::SubmitError>` with `Conflict` and transparent `Other` variants. `item_post` maps `Conflict` to HTTP 409 with a stable stale-form message and `Other` to the existing error rendering. This is an intentional additive/breaking contract adjustment requiring exact public API inspection and re-blessing of adminapi only.

For config, render one coherent `{revision, settings}` and include `_expected_revision` as a hidden field. On submit, begin a transaction, lock/read the current revision, compare it with the posted expected revision, and return `SubmitError::Conflict` before any update/event if it changed. Apply the batch and commit once only when equal. This intentionally treats any intervening config change as a conflict because the form represents the whole snapshot.

For API keys, render each row's expected policy and revoked state as hidden fields. Execute conditional updates that include expected old state and require exactly one affected row; inserts rely on the unique key. Any missing/changed/conflicting row rolls back the entire batch. Return a clear stale-form response and remove the inaccurate “anti-TOCTOU” comments unless the new CAS is present.

**Focused verification:** exercise the real HTTP flow GET form → concurrent DB write → POST with the original hidden expected state → 409, proving zero partial writes/events. Fresh config succeeds; an unrelated config revision conflicts conservatively; one stale API-key row rolls back changes to other rows; an insert collision is non-destructive; existing CSRF-before-editability behavior remains covered. There are no nonce, TTL, capacity, reaper, or form-store tests because that subsystem is not introduced. Inspect and bless only the intended adminapi diff.

### 16. Validate rate configuration and bound per-IP state `[subagent-complex]`

**Files/symbols:** `core/app/src/lib.rs` (`env_f64`, gateway HTTP layer setup); `core/httpmw/src/limiter.rs` (`IpLimiter`); associated tests and gateway/player config parsers for consistency.

Replace permissive `env_f64` use with one pure rate parser that rejects non-finite and negative values. For the always-on gateway HTTP rate, invalid values fall back loudly to the documented default 20 rps rather than disabling protection. Permit zero only at call sites whose documented policy explicitly says zero disables; encode that policy in the parser call rather than accepting zero globally.

Give `IpLimiter` a private `DEFAULT_MAX_VISITORS = 65_536` hard capacity used by HTTP gateway and admin login; provide a crate-private small-cap constructor for tests. Existing IPs continue through their existing bucket. A new IP at capacity is rejected immediately without a request-path scan or insertion. The existing once-per-minute background reap is the only reclamation mechanism; do not add a second coordinated capacity reaper or LRU eviction. Increment one global `http_rate_limit_table_saturated_total` counter for these denials; saturation does not flip readiness.

**Focused verification:** table-test NaN, ±infinity, negative, malformed, zero under both policies, and a valid fractional value. A tiny-cap limiter proves existing IPs still work, new IPs cannot grow the map past the cap, and the existing background reap later frees expired entries. No request-path fake-clock coordination is introduced.

### 17. Close all input-policy gaps and harden new match reports `[subagent-complex]`

**Files/symbols:** private validators in `modules/accounts/`, `modules/characters/`, `modules/match/`; `modules/match/src/lib.rs` and store path; conformance adapters/policy/inventory from step 8; tests in separate test files.

Implement the byte caps fixed above using shared validator functions called by both production handlers and the factual conformance probes. Keep existing email/password/admin/API-key caps. For match report handling, apply this order:

1. validate `ReportId` nonempty and at most 128 bytes;
2. query by id; an exact existing payload returns the current idempotent success, while different payload is Conflict;
3. for a new id only, require nonempty winner/loser, each at most 128 bytes, and `winner != loser`;
4. only then calculate/store effects and emit `match.finished` in the existing transaction.

This ordering avoids breaking replay of already accepted reports while preventing new empty/self matches. Update the conformance inventory so every former `KnownGap` becomes `Applies`, then run conformance with `--deny-gaps` and confirm zero gaps.

**Focused verification:** table-test boundary and boundary+1 UTF-8 byte lengths through the production validators. ReportId and other domains reject before DB/service work; winner/loser validation performs exactly the one required replay lookup but no write or event. Cover exact replay, different-payload conflict, new invalid participants, and one inventory-golden omission case; do not build a second validation DSL.

### 18. Preserve healthy shared QUIC connections on stream-local errors `[subagent-complex]`

**Files/symbols:** `core/remote/src/lib.rs` (`Conn`, call/reset/retry loop); `core/edge/src/client.rs` and frame read/write mapping; `core/remote/src/tests.rs` and edge client tests.

Define private `CallFailure { mapped: opsapi::Error, provenance: FailureProvenance }` with `FailureProvenance::{ConnectionFatal, StreamLocal, PeerAnswer}`. Classify it at the exact Quinn/frame operation before mapping erases the concrete cause, and preserve it through edge client and remote. `ConnectionLost`, locally closed endpoint, or fatal connection open/read/write errors are `ConnectionFatal`; peer `STOPPED`, frame-too-large, codec/response serialization, and individual stream cancellation are `StreamLocal`; `Remote` and `UnknownMethod` envelopes are `PeerAnswer`. Unknown/unprovenanced failures default to `StreamLocal`, never to connection-fatal. Reset the cached client and perform the one allowed reconnect replay only for proven `ConnectionFatal`. Stream-local and peer-answer failures do not call `Client::close` and do not evict/replace the shared connection.

Keep generated retry policy authoritative: `RetryMode::Never` remains fail closed and `OnceAfterReconnect` gets at most one replay after proven connection loss. Do not infer fatality from `opsapi::Status::Unavailable`, because mapping has erased provenance. Do not expand this step into gateway cache eviction unless a focused failing test demonstrates the same provenance bug there.

**Focused verification:** a table maps concrete Quinn/frame failures to provenance; one representative concurrent-stream test proves a stream-local failure does not break the other stream; one connection-loss test proves exactly one redial/replay for retry-safe work; one non-retry-safe test proves no replay; UnknownMethod remains NotFound. Do not duplicate the full concurrent transport fixture for every stream-local variant.

### 19. Cut over commands and repair current documentation without rewriting history `[subagent-mechanical]`

**Files/symbols:** delete `run.sh`, `run.ps1`, `verify.sh`, `verify.ps1` after parity; update `README.md`, `CLAUDE.md`, `AGENTS.md` consistently only for their duplicated project facts while preserving tool-specific working agreements, `docs/reference/architecture-enforcement.md`, `docs/reference/hetzner-cloud.md`, C# client docs, splitproof header, and `docs/README.md`; add `docs/reference/current-tooling.md` only if it replaces duplicated command facts; correct stale comments in inventory/registry/match/leaderboard; update dated plans `2026-07-12-0952-convention-conformance-harness-plan.md` and `2026-07-11-2249-remediation-round4-plan.md` via status/errata headers only.

Make `cargo run -p devctl -- up monolith|split`, `status`, `down`, and `cargo run -p verifyctl -- [--fast|--all|--strict|--slow]` the only current commands. Document the inherited lease, exact owned-process cleanup, fail-closed audit behavior, and the single-rollout protocol. Replace current references to `go-arch-lint`, retired split-proof shell harnesses, obsolete player-QUIC limiter claims, and the invalid conformance command. Correct source comments that still describe messaging delivery, future `RemoteBackend`, one service-name assumption, or inventory cache behavior.

Add a small blocking `docs-current` stage inside `verifyctl`—not a new standalone checker. Limit its inputs to root `README.md`, `CLAUDE.md`, `AGENTS.md`, and `docs/reference/**/*.md`; exclude plans and status/history. Check local Markdown links, Cargo package tokens directly following `-p`, and exact executable command lines or links naming retired `run.*`/`verify.*` scripts. Exact matching plus history exclusion must make negative prose legal without a growing phrase allowlist. Source comments are corrected once by review/`rg`, not permanently parsed as documentation. Update the stage-manifest golden and place it after conformance and before split-proof. Add a new canonical tooling document only if it replaces duplicated facts rather than creating another copy.

After `devctl` and `verifyctl` parity has passed, remove the four shell/PowerShell forwarders in the same change so there is one implementation, not permanent wrappers. Dated plans remain historical: prepend Status/Superseded/Errata rather than editing their bodies.

**Focused verification:** small fixtures cover a broken local link, missing package, retired executable command, and legal negative prose. Smoke current CLI help, and use one exact `rg` check for broad process-kill commands in active tooling source. Do not add a general documentation parser or duplicated-fact reconciliation framework.

### 20. Final regression proof and implementation record `[inline]`

**Files/symbols:** this plan status header; a new dated implementation summary at `docs/status/YYYY-MM-DD-HHMM-architecture-remediation-summary.md`; public API/contract baselines only where reviewed.

Run focused tests sequentially throughout implementation. At the end, after confirming no Cargo/rustc rollout is active, inspect or clear Postgres only if there is evidence of an orphan or stuck session, then run exactly one:

```text
cargo run -p verifyctl -- --all --strict
```

This must cover build, clippy, workspace tests, audit, fortress/archcheck, routecheck, codegen freshness, contract golden, zero-gap conformance, docs-current, splitproof, public API, fuzz platform handling, C# client, and topiccheck. Run `--slow` as a separate explicitly requested rollout; do not overlap it with the final gate.

Inspect the final `git diff --stat`, the conformance dependency direction, intentional public/generated API diffs, and broad process-cleanup paths. Rely on passing archcheck/codegen/docs stages for invariants they already check instead of manually repeating the entire suite. Then prepend this plan's final status and write a concise implementation summary listing commands, results, commits, and approved deviations. Commit every completed task or independently reviewable part as it verifies; pushing still requires an explicit user request.

## Required commit boundaries during implementation

Commit at least at these reviewable boundaries: (1) processctl ownership/state/lease, (2) shared fleet + splitproof, (3) devctl, (4) verifyctl, (5) conformance + rpc generator, (6) boot invariants, (7) retention, (8) scheduler, (9) accounts/OAuth, (10) admin CAS, (11) rate/input policy, (12) remote provenance, (13) docs/tooling cutover. Split a boundary further whenever one independently reviewable part verifies earlier. Never hold verified changes until the entire rollout ends, and never include unrelated pre-existing changes.

## Reviewer checklist

The independent reviewer must specifically challenge:

- whether PID + executable + start marker is sufficient on both supported platforms and every signal path validates it;
- whether inherited lease authentication can be replayed or accidentally leaked to grandchildren;
- whether foreground `devctl` plus Job Object/process groups actually handles supervisor crash and partial boot;
- whether subprocess splitproof preserves the one-rollout rule;
- whether config's global revision CAS creates an unacceptable availability regression versus per-key CAS;
- whether the chosen byte caps break any current fixture or legitimate external token;
- whether the conformance inventory has one parser authority rather than a drifting syntax clone;
- whether retention metrics/liveness have exactly one authority;
- whether remote fatality classification can mistake a stream-local protocol error for connection death;
- whether any finding in the closure table lacks a concrete negative regression test.

## Independent review resolution

The reviewer returned `CHANGES REQUIRED`. The revised plan incorporates the substantive punch list:

- replaced environment-borne rollout nonce with a one-shot, non-inheritable pipe/handle consumed before Tokio;
- narrowed process support to Windows/Linux and closed the supervisor-crash race with suspended Job assignment on Windows and a Linux guardian/subreaper;
- made state permissions and post-spawn checkpoint failure fail closed;
- specified exact build/service environment allowlists and reuse by devctl/splitproof;
- reduced verifyctl to a closed static stage list, made routecheck unconditional, and made baseline blessing recoverable rather than claiming multi-file filesystem atomicity;
- selected one shared rpc parser/model authority and removed conformance policy from production graphs without feature leakage;
- changed contribution typing to consumer declarations in lifecycle `register`, covering the first producer and every known slot;
- retained the existing `EVENTS_HOUSEKEEP_INTERVAL` name and defined the scheduler cursor as a last-name/insertion-point cursor;
- replaced the ineffective “captured closure” CAS with hidden expected-state fields, conditional SQL, and a typed 409 conflict path; no server-side form-instance subsystem is needed;
- fixed redirect URI policy, exact IP-map capacity, match test ordering, and remote provenance variants;
- narrowed docs-current to current root/reference Markdown links, package tokens, and exact retired commands; source trees and phrase allowlists are excluded.

The initial review incorrectly treated provider-specific dispatch tags and opt-in commits as repository requirements. `AGENTS.md` actually mandates `[inline]`/`[subagent-complex]`/`[subagent-mechanical]` and commits after every completed task or independently reviewable part. This revision corrects the plan and the stale reference documents; only pushing remains opt-in.

The reviewer re-audited the revised plan twice. The second pass exposed and closed platform-SKIP semantics, split-safe typed slots, external-login serialization, limiter saturation complexity, guardian-pipe ownership, and environment allowlists. The final pass required `link_identity` to share the external-identity advisory lock; after that correction the reviewer returned **APPROVE**.
