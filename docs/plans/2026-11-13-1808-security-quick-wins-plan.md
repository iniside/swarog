# Security quick wins (plan A) — plan

## Context

Owner request (2026-11-13): "zacznij research pod punkt A", where point A is "security
quick wins": the RustSec advisories, WALLET-01, APIKEYS-01, FRIENDS-03 and MAIL-02.
Owner decisions after the research:

- WALLET-01: the purge sweep claims and locks at most **64 players per fire**, like storage.
- MAIL-02: the positive TLS proof uses a **`#[cfg(test)]` trusted-root constructor**; no
  production setting.
- FRIENDS-03: **excluded** — a separate plan later. The handle oracle is also open on
  `party.invite`, so closing friends alone buys nothing. This plan only retargets the todo
  entry.
- Leftovers found during research go into **one docs step**. No code fixes for them here.

Explicitly out of scope:
- FRIENDS-03 code.
- The suspected purge-vs-write races in characters, friends and notifications. They are
  filed as todo entries only.
- The razed admin (b8435c9a). APIKEYS-01 is stale because the role-policy editor no longer
  exists; its fix belongs to step 21 of
  `docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md`.
- Running `verifyctl`. Owner 2026-11-13: admin splitproof/clippy failures are expected
  until the admin panel lands; no verifyctl until then.

No new module, event, capability, contract or schema. Every mechanism below copies an
existing in-repo precedent (storage's owner lock, mail's `plaintext_for_tests`).

## Verified facts

**RustSec** (research 2026-11-13; rustsec.org pages read for 0524, 0523, 0221, 0382, 0349,
0381; the rest from the listing only):
- Every patched version is a semver-compatible patch inside the current requirements
  (`Cargo.toml:224-282`):
  - quinn-proto 0.11.16 → ≥0.11.19 (0523, 0524 and the 2026-10-12 batch);
  - rustls 0.23.45 → ≥0.23.46;
  - rustls-webpki 0.103.15 → ≥0.103.16;
  - webpki-roots 1.0.8 → ≥1.0.10;
  - webpki-root-certs 1.0.8 → ≥1.0.10;
  - event-listener 5.4.1 → ≥5.4.2.
- The fix is lockfile-only, with no API call sites.
- The advisory set has grown since the 12 recorded on 2026-10-21; two quinn-proto advisories
  were published on 2026-11-13.
- Ignored advisory: only RUSTSEC-2023-0071 (`tools/verifyctl/src/stages/audit.rs:33-36`).
- cargo-audit is not installed; the stage installs it with `cargo install cargo-audit
  --locked` (`audit.rs:8-44`).
- The crypto provider is ring only; there is no aws-lc in `Cargo.lock`.

**WALLET-01** (lines read 2026-11-13):
- The movement guard `Service::apply_on` (`modules/wallet/src/service.rs:236-250`) runs
  `validate_movement` and then `Store::is_player_purged_tx`
  (`modules/wallet/src/store.rs:154-168`, a plain SELECT, no lock) before any write.
- `credit`, `debit` (pool path `apply`, `service.rs:325-346`) and `grant_starter`
  (`projection.rs:112-133`, delivery tx) all go through `apply_on`.
- The sweep `Service::sweep_purged_players` (`modules/wallet/src/projection.rs:187-234`)
  claims `PURGE_BATCH = 256` tombstones per statement (`:151-154`).
- It loops batches under `SWEEP_BUDGET` (5 s), with a `PURGE_FLOOR` watermark (`:156-174`).
- Per player it runs `Store::purge_player_tx` (`store.rs:348-363`) and stamps `swept_at`.
- **Race (READ COMMITTED: the delivery worker issues a bare `BEGIN`,
  `core/asyncevents/src/worker.rs:194`; `store.rs:184-188`):**
  1. A movement passes the guard.
  2. The tombstone commits.
  3. The sweep deletes the balance and re-owns the ledger, and misses the movement's
     uncommitted rows. This happens in exactly two windows:
     - the movement's balance write is the INSERT of a first-ever `(player, currency)` row,
       which the DELETE never sees or waits for;
     - the sweep runs between the movement's guard and its `apply_balance_tx`.

     If instead the movement has already UPDATEd an existing balance row, the sweep's DELETE
     blocks on that row lock and, after the movement commits, deletes the post-movement row.
     Its next statement re-owns the committed ledger row, so that interleaving is safe today.
     The sweep then stamps `swept_at` and commits.
  4. The movement commits a balance and ledger row under the real id.
  5. `swept_at` is never reset (`plant_purged_player_tx` is `ON CONFLICT DO NOTHING`), so the
     money survives erasure.
- **Storage precedent:**
  - `owner_lock_key` (`modules/storage/src/store.rs:125-140`): FNV-1a over
    `b"storage.owner/"` plus the uuid normalised to 32 lowercase hex digits.
  - `lock_owner_tx` (`:265-277`): `pg_advisory_xact_lock` on the tx connection.
  - Writer: begin → lock → tombstone read → write (`service.rs:230-259`).
  - Sweep: claim `FOR UPDATE SKIP LOCKED` → per owner lock → delete → stamp
    (`projection.rs:59-123`).
  - `STORAGE_OWNERS_PER_FIRE = 64` is both the claim `LIMIT` and the lock bound, with no
    batch loop (`projection.rs:30-42`).
- Lock order on both sides: advisory lock, then row locks. Writers never lock
  `purged_players` rows, so there is no cycle.
- Deterministic concurrency test precedent: `modules/party/src/tests.rs`. It uses "staged,
  never slept" (`:11-14`), `finish` (`:532`), `wait_blocked` on `pg_stat_activity` (`:543`),
  and `hold_party_lock` (`:571`).

**MAIL-02**:
- lettre 0.11.23 with `tokio1-rustls-tls`; default trust is `webpki-roots`.
- `SmtpSender::new` (`modules/mail/src/smtp.rs:121-146`) has no TLS customisation.
- `plaintext_for_tests` (`:148-176`, `#[cfg(test)]`, `builder_dangerous`) is the shape to
  copy.
- lettre exposes `TlsParameters::builder(host).certificate_store(CertificateStore::None)
  .add_root_certificate(..).build_rustls()` and the builder's `.tls(Tls::Required(..))`.
- The fixture `Relay` / `converse()` (`modules/mail/src/smtp_tests.rs:120-263`) is wired to
  `TcpStream::into_split`.
- `tokio-rustls` is not a workspace dependency (0.26.4 is in the lock via lettre). Its
  defaults pull aws-lc-rs, which is forbidden (`Cargo.toml:270-275`).

**APIKEYS-01**:
- `modules/apikeys/src/admin.rs` and `policy_method_options` no longer exist (b8435c9a).
- The new control plane already supports a pre-checked `FieldKind::Multi` via
  `Field.default` (`core/controlplane/src/lib.rs:436-454`, `core/admin/src/render.rs:1029-1042`).

## Invariants

1. **One wallet lock authority per player.** Every wallet write for a player — every
   `apply_on` caller and the sweep's per-player purge — first takes
   `pg_advisory_xact_lock(wallet_player_lock_key(player_id))` on the SAME connection as its
   writes, then reads or writes.
   - In `apply_on` the lock precedes the purged-guard read.
   - Consequence: either the movement sees the committed tombstone and returns
     `Outcome::Purged`, or the sweep waits for the movement and purges its rows.
2. **The key is spelling-invariant.** Two spellings of one uuid that are equal in Postgres
   give the same key. The `wallet.player/` namespace keeps it apart from other modules'
   keys.
3. **One fire holds at most 64 advisory locks.** One constant is both the claim `LIMIT` and
   the bound on the players a fire works; there is no batch loop. A fire may still stop early
   on `SWEEP_BUDGET`, at a player boundary.
4. **Dependencies change in `Cargo.lock` only.** The lock diff touches only the advisory
   crates and their forced co-updates. Still no aws-lc / `*-sys` / `cc` package, and ring
   stays the only provider.
5. **No change to mail production surface.** The trusted-root constructor is
   `#[cfg(test)]`; `SmtpSender::new` and `TlsMode` are untouched.

## Steps

### Step 1 — dependency advisories `[opus]`
**(a) What:** `Cargo.lock` only.
**(b) Why first:** it has no code dependency and unblocks the blocking `audit` stage.
**(c) How:**
1. Install cargo-audit (`cargo install cargo-audit --locked`) — one cargo command, with no
   other rollout live.
2. Run `cargo audit --ignore RUSTSEC-2023-0071` and record the authoritative advisory list.
3. Run `cargo update -p <crate> --precise <patched>` for each flagged crate. Use the
   `name@version` spelling for the duplicated `webpki-roots`.
4. If a precise update is refused by a parent's range (rustls-acme 0.15.3 or
   rustls-platform-verifier 0.7.0), update that parent within its current requirement.
   Never edit a `Cargo.toml` requirement without stopping to report.
5. Check, by reading the lock diff (`git diff Cargo.lock`), that only the expected packages
   move.
6. Check that `cargo tree -i aws-lc-rs` and `cargo tree -i aws-lc-sys` are empty.
7. Re-run `cargo audit`: it must be clean apart from the ignore.
8. Run `cargo build --workspace` and `cargo test --workspace --no-fail-fast`.
9. Report observed counts.

**(d) Lane:** `core-implementer`, model `opus`.

Commit: `fix(deps): lift the TLS and QUIC stack past the open RustSec advisories (security-quick-wins step 1)`.

### Step 1b — ignore the never-compiled sqlite advisory `[opus]`
Added 2026-11-17 after step 1 (`0fedd251`).
- **Why:** cargo audit still flags RUSTSEC-2026-0459 on libsqlite3-sys 0.30.1.
  - It is in the lock only because sqlx's weak `sqlx-sqlite?/…` feature entries pull it in;
    only `postgres` is enabled and sqlite is never compiled.
  - The released sqlx 0.9.0 still caps it at `<0.38`, while the fix needs ≥0.39. Only
    unreleased sqlx main allows `<0.41`.
- **Owner decision 2026-11-17:** ignore it with that rationale. Removing the ignore is a todo
  entry for when a released sqlx allows ≥0.39; that sqlx upgrade gets its own plan.
**(a) What:** `tools/verifyctl/src/stages/audit.rs` only.
**(c) How:**
- Add `--ignore RUSTSEC-2026-0459` beside RUSTSEC-2023-0071, with a comment stating the
  lock-only / never-compiled reason and the removal condition.
- Run `cargo build -p verifyctl` and `cargo test -p verifyctl`.
- Also run `cargo audit --ignore RUSTSEC-2023-0071 --ignore RUSTSEC-2026-0459`: it must report
  no vulnerability.
**(d) Lane:** `core-implementer`, model `opus`.

Commit: `fix(verifyctl): ignore the sqlite advisory the build never compiles (security-quick-wins step 1b)`.

### Step 1c — execute the ignore's premise `[opus]`
Added 2026-11-17 after proof-auditor on 9c6ca7a3. Owner approved it.
- **Why:** nothing executes the ignore's premise ("sqlite is never compiled") or its
  staleness. If sqlite is compiled, or libsqlite3-sys leaves the lock, the audit stays green
  and the ignore lives forever.
**(a) What:** `tools/verifyctl/src/stages/audit.rs`. If its runner needs it, the fake-cargo
fixture `tools/verifyctl/fixtures/verifyctl/main.rs` (`tree` branch) may change too, but only
to answer the new invocation.
**(c) How:** before the cargo-audit call, the stage checks the premise:
- libsqlite3-sys is absent from `Cargo.lock` → FAIL "RUSTSEC-2026-0459 ignore is stale; remove
  it".
- `cargo tree -i libsqlite3-sys --workspace --target all -e all` (with `--locked`) shows the
  package in the build graph → FAIL "libsqlite3-sys is compiled; the RUSTSEC-2026-0459 ignore
  is no longer justified".
- Otherwise, pass the ignore as today.
- First verify, by running it, how `cargo tree -i` reports a lock-only package (exit code and
  output) and branch on the observed behaviour. Do not guess it.
- The check is keyed to this single ignore, as data next to it. It is not a generic framework.
- Build with `cargo build -p verifyctl`, test with `cargo test -p verifyctl`, and check with
  `cargo clippy -p verifyctl --all-targets`. No new tests in this step.
**(d) Lane:** `core-implementer`, model `opus`.

Commit: `fix(verifyctl): fail the audit when the sqlite ignore's premise breaks (security-quick-wins step 1c)`.

### Step 1d — premise-guard tests `[test-author]`
**(a) What:** `tools/verifyctl/tests/runner.rs`, the fake-cargo fixture, and the stage's unit
tests.
**(c) How:**
- Three cases: stale (the crate is absent from the lock), compiled (`tree` shows it), and the
  premise holds (pass).
- Also, from the step 1c report and core-reviewer on 0fad5a23:
  - a generic `cargo tree` error → FAIL;
  - exit 0 with empty stdout and no "nothing to print" → FAIL;
  - in every FAIL case, assert that the record shows no `cargo-audit` call;
  - the fake cargo pins the premise argv exactly
    (`--workspace --target all -e all --locked`), the same way it pins the cargo-audit argv,
    so that dropping a flag turns the tests red.
- `premise_finding` is pure; give it unit tests in addition to the runner-level cases.
- Each case is proven red by mutating the guard.
- Run `cargo test -p verifyctl` only.
**(d) Lane:** `test-author`, model `opus`. A `proof-auditor` follows.

Commit: `test(verifyctl): prove the sqlite ignore guard fails stale and compiled cases (security-quick-wins step 1d)`.

### Step 1d′ — proof hardening from proof-auditor on 552fce89 `[test-author]`
The proof-auditor found the proof sound and left two residuals; both fail closed.
- **Anchor the stale wording.** Add one test that runs real
  `cargo tree -i <nonexistent-crate> --workspace --target all -e all --locked --offline` at
  the workspace root and asserts `premise_finding` returns the stale reason. Today the fake
  and the classifier only share a string.
- **Close the other spelling.** In the FAIL runner test, also reject any record line starting
  with `cargo audit`, i.e. cargo-audit run through the cargo subcommand.
- Run `cargo test -p verifyctl` only. Run it after step 3, because cargo runs one at a time.

Commit: `test(verifyctl): anchor the stale premise wording on real cargo (security-quick-wins step 1d)`.

### Step 1d″ — make the real-cargo anchor hermetic `[test-author]`
Added 2026-11-17. core-reviewer REJECTed 54a309b4 because of an error in this plan: 1d′
prescribed `--offline --target all` at the workspace root. On a cold cache, or after a lock
bump that adds a non-host crate, cargo fails offline before it reaches the `-i` spec. The
test then goes red, and since verifyctl's `test` stage runs before `audit`, it flakes on cache
state. It never gave a false pass. Owner decision: use a throwaway workspace.
- The test writes a temp workspace: one package with no dependencies, and its own lockfile,
  generated offline.
- It runs the argv derived from production (`SQLITE_PREMISE_ARGS` with the crate swapped, plus
  `--offline`) there, and asserts the stale reason.
- Comment fixes from the same review:
  - the `audit_tests.rs:1` module doc names the real-cargo test as the exception;
  - `:65` says "no lock holds" instead of "no lock can hold";
  - the hang-guard panic at `:99/:109` says "did not finish within 300 s (likely blocked on
    the package-cache lock)".
- Re-prove the stale-substring mutation red.

Commit: `test(verifyctl): run the real-cargo premise anchor in a throwaway workspace (security-quick-wins step 1d)`.

### Step 1e — yanked lock entries `[opus]`
Added 2026-11-17; owner approved it.
- **What:** `Cargo.lock` only. Move chacha20 0.10.1 and spin 0.9.8 (both yanked) to the
  nearest non-yanked versions within the current requirements.
- **How:** the same procedure as step 1: a minimal lock diff, `--locked` acceptance, no
  aws-lc / `*-sys` / `cc`, `cargo audit` with no warnings, and a full `cargo test --workspace`.
- If a parent requirement pins the yanked version, stop and report.
- **Lane:** `core-implementer`, model `opus`.

Commit: `fix(deps): move off the yanked chacha20 and spin releases (security-quick-wins step 1e)`.

### Step 2 — wallet per-player lock `[opus]`
**(a) What:** `modules/wallet/src/store.rs`, `modules/wallet/src/service.rs`,
`modules/wallet/src/projection.rs`.
**(b) Why now:** it is independent of step 1. It runs after step 1 because cargo runs
serially.
**(c) How:**
- `store.rs`:
  - Add a private `fn wallet_player_lock_key(player_id: &str) -> i64`, copied from
    `storage::store::owner_lock_key` with the prefix `b"wallet.player/"`.
  - Add `pub(crate) async fn lock_player_tx(&self, conn: &mut PgConnection, player_id: &str)
    -> Result<(), sqlx::Error>` running `SELECT pg_advisory_xact_lock($1)`.
  - These are copies, not imports: the fortress rule forbids a cross-module helper.
- `service.rs` `apply_on`: after `validate_movement(m)?` and before `is_player_purged_tx`,
  call `self.store.lock_player_tx(conn, &m.player_id)`, mapping the error with
  `movement_error`.
- `projection.rs`:
  - Replace `PURGE_BATCH` and the batch loop with `pub(crate) const WALLET_PLAYERS_PER_FIRE:
    i64 = 64` as the single claim `LIMIT`.
  - Keep the `FOR UPDATE SKIP LOCKED` claim and the `ORDER BY wallet.purged_players.player_id`.
  - Delete the `PURGE_FLOOR` watermark. With a single claim per fire there is no re-scan, so
    the quadratic hazard it guarded is gone.
  - Keep `SWEEP_BUDGET` at the player boundary.
  - In the per-player body, call `lock_player_tx` before `purge_player_tx`.
- No schema change and no topology branch. Advisory keys are database-global, so the lock
  holds across `cmd/server` and `cmd/wallet-svc`.
- Doc comments that become false are left to step 6. List their file:line in the report.
- Build with `cargo build -p wallet` and `cargo clippy -p wallet --all-targets`. No tests in
  this step.

**(d) Lane:** `core-implementer`, model `opus`.

Commit: `fix(wallet): serialize movements and the purge sweep on one per-player lock (security-quick-wins step 2)`.

### Step 3 — wallet race tests `[test-author]`
**(a) What:** `modules/wallet/src/tests.rs`.
**(b) Why now:** step 2 has landed and compiles.
**(c) How:**
- **Key test:** stable, namespaced (differs from a storage-prefixed hash of the same id), and
  spelling-invariant (upper case, braces and hyphens give the same key). Mirror
  `modules/inventory/src/tests.rs:323-355`.
- **Hermeticity:** the sweep claims from the shared `wallet.purged_players` table.
  - Every sweep test holds `DB_LOCK`.
  - Use fixed, lowest-sorting player ids, following groups' `fixed_uuid` cap test
    (`modules/groups/src/purge_tests.rs:275`).
  - Assert only on the test's own rows.
- **Race 1, movement first (the once-wrong branch):** a FIRST-EVER credit for player P runs
  through `apply_on` on an open tx, past the guard, uncommitted.
  1. Commit P's tombstone separately.
  2. Run the sweep on a racer pool with a unique `application_name`.
  3. `wait_blocked` until it is blocked on Lock.
  4. Commit the movement and `finish` the sweep.
  5. Assert: no balance row for P, P's ledger row owned by `Store::PURGE_SENTINEL`, and
     `swept_at` set.

  There is no existing-balance variant: its row lock already serializes it without the fix
  (see Verified facts), so it cannot be proven red.
- **Race 2, sweep first — a lock-order check, not a once-wrong branch:**
  1. A sweep tx has locked P and not committed.
  2. A racer `credit` for P blocks (`wait_blocked`).
  3. Commit the sweep.
  4. Assert the credit returns 409 `PLAYER_DELETED`, with no ledger row claimed and no
     balance row.

  Label it as such in its doc comment. Without the fix, only its `wait_blocked` fails.
- **Fire bound:** with 65 of the test's own lowest-sorting tombstones, one fire stamps exactly
  64 of them.
- **Existing test (added 2026-11-17, core-reviewer on 39e2f802):**
  `sweep_clears_balances_reassigns_ledger_to_sentinel_key_stays_claimed` uses a random
  `gen_random_uuid()` player, and one fire now claims only the 64 lowest ids. Move it to a
  fixed, lowest-sorting id under `DB_LOCK`, the same pattern as the new tests.
- No `sleep`; use the party helpers' pattern.
- Prove Race 1 and the fire bound red against step 2's change: remove the lock calls for
  Race 1, restore the loop for the fire bound. Mutate, run, then restore from a scratchpad
  copy.
- Run `cargo test -p wallet` only.

**(d) Lane:** `test-author`, model `opus`.

Commit: `test(wallet): prove the purge sweep and movements serialize per player (security-quick-wins step 3)`.

### Step 3′ — wallet proof hardening `[test-author]`
Added 2026-11-17. Source: core-reviewer and proof-auditor on b6108348. Both found the proof
sound. None of these is a false pass; each one is a misleading red or a hermeticity hole.
**(a) What:** `modules/wallet/src/tests.rs` only.
**(c) How:**
1. **Race 1, and the existing sentinel test at `0x100`:** before the sweep runs, assert that
   no unswept tombstone sorts below the test's player. Race 2's `stamped` check is the model.
   Starvation should then fail as a precondition failure, not as "balance survived".
2. **Fire bound:** run the `foreign_below` check BEFORE planting the 65 tombstones, and clean
   up on every exit path, so a failure cannot starve later sweep tests.
3. **`wait_advisory_parked`:** match the waiting lock to P's key. Join `pg_locks` with
   `NOT granted`, `locktype='advisory'` and `(classid::bigint<<32)|objid = key` to the
   racer's pid. "Parked" then means parked on P's lock.
4. **Hermeticity lead:** the delivery tests (`wired_for_delivery` / `wired_for_dual_delivery`
   + `init` + `deliver_all`, tests.rs ~1373–1902) run without `DB_LOCK` but register the real
   purge-sweep subscription.
   - First check whether a `player-purge` fired event can reach them.
   - If it can, make them take `DB_LOCK`, per the `docs/reference/testing.md` contract. If it
     cannot, record why in the report and change nothing.
- Prove 1–3 with mutations or by staging the failure.
- Run `cargo test -p wallet` three times.
**(d) Lane:** `test-author`, model `opus`. `proof-auditor` follows.

Commit: `test(wallet): fail the race proofs on their own preconditions (security-quick-wins step 3)`.

### Step 4 — mail trusted-root test constructor `[opus]`
**(a) What:** `modules/mail/src/smtp.rs`, `modules/mail/Cargo.toml` (dev-dependencies).
**(b) Why now:** the step 5 tests need it; it is independent of the wallet steps.
**(c) How:**
- Add `#[cfg(test)] pub(crate) fn tls_trusting_for_tests(host: &str, port: u16, from: &str,
  connect_timeout: Duration, mode: TlsMode, root_pem: &[u8], credentials: Option<(String,
  String)>) -> anyhow::Result<SmtpSender>`, in the same shape as `plaintext_for_tests`.
  - Build `TlsParameters::builder(host.to_string()).certificate_store(CertificateStore::None)
    .add_root_certificate(Certificate::from_pem(root_pem)?).build_rustls()?`.
  - Use `builder_dangerous(host).tls(Tls::Required(..))` for `StartTls` and
    `.tls(Tls::Wrapper(..))` for `Implicit`.
  - Then add the same port/timeout/hello/credentials chain.
- Add `tokio-rustls = { version = "0.26", default-features = false, features = ["ring",
  "tls12"] }` to `[workspace.dependencies]` in the root `Cargo.toml`, next to `rustls`
  (`:279`). Its defaults pull aws-lc-rs.
- Add mail dev-dependencies `rcgen`, `rustls` and `tokio-rustls`, all `workspace = true`.
- Check `cargo tree -i aws-lc-rs` is empty.
- Build with `cargo build -p mail --tests` and `cargo clippy -p mail --all-targets`.

**(d) Lane:** `core-implementer`, model `opus`.

Commit: `feat(mail): add a test-only sender that trusts a given root (security-quick-wins step 4)`.

### Step 5 — SMTP TLS handshake proofs `[test-author]`
**(a) What:** `modules/mail/src/smtp_tests.rs`.
**(b) Why now:** step 4 has landed.
**(c) How:**
- Generalise `converse()` to any `AsyncRead + AsyncWrite + Unpin` stream.
- Add a STARTTLS fixture. Its script:
  1. `220`
  2. `EHLO` → `250-STARTTLS`
  3. `STARTTLS` → `220`
  4. hand the raw `TcpStream` to a `tokio_rustls::TlsAcceptor` holding a minted
     CA-signed leaf (SAN `127.0.0.1`)
  5. post-TLS `EHLO` → `250-AUTH PLAIN`
  6. `AUTH` → `235`
  7. `MAIL`, `RCPT`, `DATA` → `250`
- Every line is recorded before the reply (happens-before), and every socket test runs under
  `HANG_GUARD`.
- Tests:
  - **(i) negative, production path:** `SmtpSender::new` (StartTls) against the fixture.
    Assert the transcript holds `STARTTLS`, no `AUTH` or `MAIL FROM` follows, and the result
    is the infra/retry class. This proves the certificate check runs on the deployed
    constructor.
  - **(ii) positive:** `tls_trusting_for_tests` (StartTls, credentials) delivers. Assert
    `AUTH PLAIN` carries base64 `\0user\0pass` and appears only after the upgrade, and that
    the message is accepted.
  - **(iii) implicit TLS:** the same as (ii) with the acceptor wrapping the socket from the
    first byte.
- If lettre refuses an IP-literal server name, mint the SAN `localhost` and dial `localhost`.
  Report which one held.
- Run `cargo test -p mail` only.

**(d) Lane:** `test-author`, model `opus`.

Commit: `test(mail): complete STARTTLS, implicit TLS and AUTH against a minted relay (security-quick-wins step 5)`.

### Step 5′ — implicit-TLS production negative `[test-author]`
Added 2026-11-17 after proof-auditor on 7c21ceee. Test (i) proves the certificate check for the
`starttls` arm of `SmtpSender::new` only. The implicit arm (`relay()`) has no certificate
proof: replacing it with an invalid-certs-accepting `Tls::Wrapper` would leave the suite green.
- Add test (iv): `smtp_sender_with(addr, TlsMode::IMPLICIT, .., Some(creds))` against
  `Script::ImplicitTls(minted.acceptor)`. Assert:
  - hits == 1;
  - no TLS_UP and no SMTP line in the transcript;
  - `rustls_failure == Some(InvalidCertificate(UnknownIssuer))`;
  - Infra/Retry.
- Prove it red with an implicit-arm `dangerous_accept_invalid_certs(true)` mutation.
- Nit: `smtp_tests.rs:856` puts NUL bytes in the assert message. Escape it as `\\0`.
- Run `cargo test -p mail` only.

Commit: `test(mail): prove the production implicit-TLS dial refuses an untrusted certificate (security-quick-wins step 5)`.

### Step 6 — docs and todo `[docs]`
**(a) What:** `modules/wallet/src/store.rs` doc comments, `modules/mail/src/smtp_tests.rs`,
and the todo and plan-tracking files listed in (c).
**(b) Why last:** the prose describes landed code (transient-state rule).
**(c) How:**
- Wallet comments:
  - `store.rs:154-156` "plain read, no advisory lock" → the lock-then-guard rule.
  - `store.rs:231` "No advisory lock" → it now applies only to movement-vs-movement on the
    row.
  - Any other comment step 2 reported false. After 39e2f802 these sit at `store.rs:188-190`
    and `:265-266`.
  - `projection.rs:213`: "All six sweeps place it here". Count the sweeps, then correct the
    number or rewrite the sentence so it states no number.
- Mail comments: `smtp_tests.rs:13-19` "gap 1" and the `plaintext_for_tests` doc
  (`smtp.rs:148-157`) where it says nothing past the upgrade is reachable.
  - "Gap 1" is rewritten to the remaining gaps, not deleted (proof-auditor on 7c21ceee).
    The one gap left is production hostname checking. A test cannot reach it because
    production never trusts the minted CA. The wrong-SAN case is proven only on the
    test constructor.
  - Also, from core-reviewer on 7c21ceee:
    - `smtp_tests.rs:6-11`, the module header, says "Two constructors…" and "the whole
      dialogue is reached through `plaintext_for_tests`". It should name the third
      constructor, `tls_trusting_for_tests`, which reaches delivery over TLS, or state no
      count.
    - `smtp_tests.rs:470-471`, the `plaintext_sender` doc: "the only way the dialogue past
      the upgrade is reachable" → it is the plaintext dial, the only one that reaches
      delivery without TLS.
- Close entries per the core-rules done-line format, each with date and plan slug:
  - RustSec in `docs/todo/dependencies.md` → `dependencies-done.md`;
  - WALLET-01 in `docs/todo/wallet.md` → `wallet-done.md`;
  - MAIL-02 in `docs/todo/mail.md` → `mail-done.md`.
- `docs/todo/dependencies.md`: add an open entry. It says to remove the RUSTSEC-2026-0459
  ignore (step 1b) and upgrade sqlx once a released sqlx allows libsqlite3-sys ≥0.39.
  - Facts for the entry: sqlx 0.9.0 caps it at `<0.38`.
  - The upgrade costs about 77 call sites across about 29 files (#3723 `SqlSafeStr`), plus
    the archcheck rule-22 `raw_sql` pattern.
  - All three sqlx pins move together: the root `Cargo.toml`, `weles/Cargo.toml` and
    `tools/processctl/Cargo.toml`.
- `tools/verifyctl/src/stages/audit.rs:41-42` (step 1b comment): say that the workspace is on
  sqlx 0.8, whose sqlx-sqlite pins `^0.30.1`, and that the latest release, 0.9.0, still caps
  the crate below 0.38. This was a core-reviewer precision note on 9c6ca7a3.
- **`audit.rs`, the RUSTSEC-2023-0071 (rsa, Marvin) ignore** (owner 2026-11-17: research +
  comment, no guard). Write a rationale comment that states only verified facts:
  - `rsa` 0.9.10 is used directly by `modules/accounts` (dev-dependency only,
    `modules/accounts/Cargo.toml:63`) and by `tools/splitproof` (`tools/splitproof/Cargo.toml:35`).
  - Both uses only generate throwaway 2048-bit keys and export n/e plus PKCS#8 PEM for JWKS
    fixtures (`accounts/src/tests.rs:87`, `oidc_tests.rs:30`, `providers_tests.rs:331`,
    `splitproof/src/idp.rs:34-44`).
  - The signing is done by `jsonwebtoken` via ring.
  - Production OIDC verification (`modules/accounts/src/oidc.rs:237-250`) is public-key only,
    via jsonwebtoken/ring, and does not touch `rsa`.
  - **Before writing**, run `cargo tree -i rsa --workspace --target all -e normal,build
    --locked` (tasklist check first) to settle whether `sqlx-mysql` puts `rsa` into any
    non-dev build. State the observed result.
  - Revisit condition: `rsa` used for decryption or signing, or a patched release.
- `docs/todo/apikeys.md` APIKEYS-01: rewrite as stale. The editor was razed by b8435c9a, and
  the requirement (a pre-checked policy `Multi` from the stored policy, revision-checked
  save) moves to admin-control-plane-redesign step 21.
- `docs/todo/friends.md` FRIENDS-03: extend to party
  (`api/party/api/src/lib.rs:134-139`, `modules/party/src/service.rs:175-181`). Note that
  closing it is a player-API contract change and needs its own plan.
- New open entries:
  - characters, friends and notifications purge sweeps take no per-player lock while their
    writers guard on the tombstone — a suspected WALLET-01-class race, unverified:
    `modules/characters/src/projection.rs:86-94`, `modules/friends/src/projection.rs:67-133`,
    `modules/notifications/src/projection.rs:441-446`;
    - **Verify before filing (added 2026-11-17).** A Grep for `pg_advisory|lock_key` hits
      `characters/src/service.rs`, `friends/src/store.rs`, `groups/src/store.rs`,
      `inventory/src/{lib,projection}.rs`; it does not hit `notifications`. Before filing,
      read each module's writer and sweep for characters, friends, notifications, inventory
      and groups. A module whose writers and sweep already share a per-player lock is dropped
      from the entry. Record what was read.
  - splitproof `create_role` posts to the razed `/admin/api-keys`
    (`tools/splitproof/src/main.rs:325-364`), in `docs/todo/splitproof.md`.
- Feature-tracker rows that cite RustSec, WALLET-01 or MAIL-02 (`docs/roadmap/feature-tracker.md`).
- Do not edit historical plans or status docs.

**(d) Lane:** `docs-writer`, model `sonnet`.

Commit: `docs(wallet,mail,dependencies,apikeys,friends,splitproof): close the plan-A security items and file the leads it found (security-quick-wins step 6)`.

## Verification / review

- One review pass each:
  - `core-reviewer` (opus) after every step's commit, steps 1 to 6;
  - `proof-auditor` (opus) after steps 3 and 5, in addition to `core-reviewer`.
- Known residual, accepted and not fixed here. It is rare but a real deadlock:
  1. A movement for player B reuses an idempotency key whose ledger row the open sweep
     already re-owned for an earlier player A.
  2. B's `insert_ledger_tx` then waits on the sweep's transaction while holding B's lock.
  3. If B is later in the same claim set, the sweep in turn waits on B's lock.

  Postgres aborts one side; the plane retries a failed delivery and the movement returns a
  transport error. Requires cross-player key reuse plus a tombstone.
- Cargo commands run one at a time, each after a `tasklist` check.
- Final checks:
  - `cargo test --workspace --no-fail-fast`;
  - `cargo audit --ignore RUSTSEC-2023-0071` clean;
  - `cargo run -p archcheck`.
- No `verifyctl` until the admin panel lands (owner 2026-11-13). The first verifyctl run
  after it must show `audit` PASS.
- Monolith vs split:
  - Wallet: the lock is topology-blind (one shared Postgres), so the race tests run
    in-module. There is no new splitproof assertion, because the split wallet path executes
    the same `apply_on` behind the RPC stub. Splitproof's wallet assertions are red today
    for admin reasons only.
  - Mail: per-process module code.
