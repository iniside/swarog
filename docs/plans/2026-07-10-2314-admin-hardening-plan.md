# Admin hardening for public deployment (Hetzner) — plan

> On approval, Step 0 commits this file into the repo as
> `docs/plans/2026-07-10-HHMM-admin-hardening-plan.md` (Plans & Status Docs rule;
> plan mode forbids writing repo files, so the copy happens at implementation start).
> Reviewed by an independent grumpy-reviewer pass (12-item punch list, verdict BLOCK);
> all items are addressed in this revision — the disposition notes are inline, marked
> `[R#]`.

## Context

The backend is heading for a public Hetzner deployment. Today the admin portal is a
deliberately-local surface: HTTP Basic auth from `ADMIN_USER`/`ADMIN_PASS` env
(`modules/admin/src/lib.rs:473-505`), no TLS anywhere on the HTTP front (bare
`TcpListener` in `core/app/src/lib.rs:556`), no CSRF protection on the mutating
`POST /admin/:slug` form, no login lockout, and no audit trail of admin actions.
Lukasz rejected the SSH-tunnel/VPN workaround: the admin must be publicly reachable
and hardened properly.

Decisions locked with the user:
- **Installer over web-setup**: admin users created by an operator CLI (`adminctl`,
  modeled on `tools/eventctl`) wrapped by `install.sh`/`install.ps1`.
  `ADMIN_USER`/`ADMIN_PASS` env vars are removed.
- **Session login replaces Basic auth**: argon2id-hashed passwords (pattern from
  `modules/accounts/src/password.rs`), opaque session token in an
  `HttpOnly + Secure + SameSite=Strict` cookie.
- **Admin gets a DB**: schema `admin` (users, sessions, login_attempts); admin-svc
  drops `without_db()` and leaves topiccheck's `PLANELESS_PROCESSES`.
- **TLS natively in gateway-svc**: `TLS_MODE=acme|files|off` — `rustls-acme`
  (TLS-ALPN-01, auto-renew, no :80 needed) plus a cert/key-file mode; env parsed in
  `cmd/gateway-svc` main, mechanism in `core/app`.
- **Audit of admin actions**: new durable contract `admin.action` (new crate
  `adminevents`), consumed by audit as its 7th raw subscription.
- **Gateway stub-coverage tripwire** (independent small item): archcheck rule +
  checkmodules test that every `#[http]`-exposing domain has a `remote::Stub` in
  `cmd/gateway-svc`.

Why not extend existing systems (Research-before-planning rationale):
- **accounts sessions**: player identity domain; admin is GameOps identity. Pattern
  copied, tables not shared.
- **httpmw rate limiter for lockout**: per-IP token bucket in the *gateway* process;
  lockout must live at the auth-check site with persistent per-user state → new
  `admin.login_attempts` table.
- **LAYER_SLOT for TLS**: wraps the axum `Router` post-accept; TLS terminates at the
  listener → `core/app` serve path, not a layer.

Key research facts the plan builds on:
- No cookie handling exists anywhere; `axum-extra 0.9` (cookie feature, the correct
  axum-0.7 line) must be added. Workspace already has `axum 0.7`, `rand 0.8`,
  `base64 0.22`, `argon2 0.5`, `rustls 0.23` (ring, no aws-lc-rs).
- `axum-server` and `rustls-acme` are NOT in Cargo.lock — new deps (Step 4, with an
  explicit crypto-provider strategy — see `[R3]` there).
- `app::run` binds `tokio::net::TcpListener` at `core/app/src/lib.rs:556-559`,
  serves via `axum::serve(...).with_graceful_shutdown(...)` (`lib.rs:580-607`,
  `HTTP_DRAIN_GRACE_MS` default 5000).
- admin module has NO `register()`/`migrate()`; `cmd/admin-svc/src/main.rs:47` boots
  `without_db()`; topiccheck hard-fails durable traffic in admin-svc via
  `PLANELESS_PROCESSES` (`tools/topiccheck/src/main.rs:84`).
- **In the split, every admin item in admin-svc is REMOTE and remote forms are
  read-only by construction** (`SubmitFn` is `#[serde(skip)]`; `page_view` drops the
  form `modules/admin/src/lib.rs:370-376`; `item_post` 405s remote items
  `lib.rs:228-241`). Form submits only ever run where the owning module is co-hosted
  (today: the monolith). This drives the `admin.action` design — see `[R1]` in Step 2.
- eventctl is the CLI pattern: hand-rolled args (no clap anywhere), logic in
  `lib.rs`, live-Postgres tests in a separate file, `dsn()` honoring `DATABASE_URL`,
  `#[tokio::main] -> ExitCode`.
- events-crate anatomy: `api/match/events`; verify's `public-api` stage
  auto-discovers `api/*/events/Cargo.toml` — new crate needs a blessed baseline.
- audit topics/ids are two positionally-zipped const lists
  (`modules/audit/src/lib.rs:44-62`) + anti-drift tests; topiccheck's
  `defined_topics()` (`tools/topiccheck/src/main.rs:170-186`) is a second list to
  extend. Stale-comment sites that reference the old ADMIN_USER bail:
  `tools/topiccheck/src/main.rs:491`, `tools/requirecheck/src/main.rs:211`,
  `modules/audit/src/lib.rs:340`, `verify.sh:449` (+ps1 twin) `[R5][R10]`.
- The `#[http(` attribute in `api/<name>/api/src/lib.rs` is the ONLY valid textual
  marker for "domain has HTTP ops" (generated `route_bindings()` exists even for
  wire-only crates, e.g. `ratingrpc`). Today's `#[http(` domains: accounts,
  characters, inventory, leaderboard, match.
- split-proof pins `ADMIN_USER=proofadmin`/`ADMIN_PASS=proofpass`
  (`split-proof.sh:142-143`); admin assertions live at `[AD1]` (sh:814), `[K5]`
  (sh:833-846, page slug is `/admin/api-keys`), monolith-parity `[M3]` (sh:1299)
  `[R6][R9]`; run scripts default `admin/admin`.
- Set-Cookie survives the gateway passthrough (only hop-by-hop + `host` stripped,
  `modules/gateway/src/proxy.rs:41-51`); XFF trusted-proxy walk
  (`core/httpmw/src/client_ip.rs:61-89`) is sound ONLY when
  `TRUSTED_PROXY_CIDRS` is configured on the admin process `[R2]`.

## Non-goals

- TOTP/2FA (future add on top of sessions).
- HTTP→HTTPS redirect listener on :80.
- CSRF token on the login form itself (`SameSite=Strict` + single-tenant GameOps).
- Multi-admin invitation UI (`adminctl create-user` covers N users).
- Remote (cross-process) admin form submits — remote pages stay read-only; making
  them mutable is a separate future project.
- Player QUIC per-IP rate limit and per-service edge certs (separate work items).

## Design decisions folded in from review

- **`[R1]` `admin.action` emits from the AUTH surface, which is local in BOTH
  topologies**: `login-succeeded`, `login-locked` (lockout threshold crossed),
  `logout`, and additionally `form-submit` where a form is local (monolith today).
  This keeps the durable trail topology-proof; form-submit emission is a bonus in
  whatever process co-hosts the owning module, not the feature's core.
- **`[R2]` Lockout thresholds are asymmetric**: user-row locks after 5 fails
  (backoff `least(2^fails,900)s`), IP-row after 20. `TRUSTED_PROXY_CIDRS` is wired
  everywhere admin runs (scripts + Hetzner checklist) so `ip:<subject>` is the real
  client, not the gateway.
- **`[R4]` Cookie `Secure` flag is a fail-closed dev knob**: `ADMIN_COOKIE_SECURE`
  default ON; scripts set `=0` with the same explicit-opt-out doctrine as
  `ACCOUNTS_DEV_AUTH` (loud warn when off). Needed because PowerShell's
  `CookieContainer` refuses to send Secure cookies over http in the proof scripts.
- **`[R5]` `ADMIN_OPEN=1` disables the whole auth layer**: sessions AND CSRF (a
  deliberately open local portal with working forms, loud warn). Tested.
- **`[R11]` No status-code username oracle**: every failed login (wrong password,
  unknown user, locked) answers 401 with an identical generic body. Lockout is
  asserted via DB rows in the proofs, not via a 429.
- **`[R8]` No hand-duplicated DDL**: `modules/admin` exports `pub const USERS_DDL`
  (and applies it in `migrate`); `tools/adminctl` depends on the `admin` module
  crate and executes the same const. Tools are not modules — archcheck's fortress
  rules constrain `modules/*`→`modules/*` edges, not `tools/*` consumers (same
  spirit as `checkmodules` importing `cmd/*` libs). One source of truth, no drift.

## Steps

### Step 0 — Commit this plan into the repo `[inline]`
**(a)** Copy to `docs/plans/2026-07-10-<HHMM>-admin-hardening-plan.md`, commit
`docs(plans): admin hardening plan`.
**(b)** First: repo is the source of truth for plans.
**(c)** Mechanical copy + commit.

### Step 1 — `adminevents` contract + audit 7th subscription `[sonnet]`
**(a)** New crate `api/admin/events/` (name `adminevents`, deps `bus`+`serde`,
workspace member); `modules/audit/src/lib.rs` + `tests.rs`;
`tools/topiccheck/src/main.rs` + Cargo.toml.
**(b)** First code step: Step 2 emits this contract; audit and topiccheck must know
it so every intermediate commit stays green.
**(c)** How:
- `api/admin/events/src/lib.rs`: payload
  `pub struct AdminAction { pub actor: String, pub action: String, pub target: String, pub detail: String }`
  — `action` ∈ `login-succeeded|login-locked|logout|form-submit` (documented, not an
  enum — additive evolution) +
  `pub static ACTION: LazyLock<EventType<AdminAction>> = LazyLock::new(|| define("admin.action", 1, HistoryPolicy::MinRetention { days: 30 }));`
  Mirror `api/match/events/src/lib.rs` (derives, docs).
- `modules/audit`: append `"admin.action"` to `DURABLE_TOPICS` and
  `"audit.admin-action.v1"` to `DURABLE_SPEC_IDS` (positional zip), add `adminevents`
  dep, extend `durable_topics_match_events` want-set.
- `tools/topiccheck`: append `of(adminevents::ACTION.contract())` in
  `defined_topics()`, add dep. `PLANELESS_PROCESSES` untouched here (Step 2 flips it
  together with the svc, keeping states green `[R7]`).
- Tests: `cargo test -p audit -p topiccheck` + `cargo run -p topiccheck`.

### Step 2 — Admin module core + admin-svc DB flip (ONE commit) `[fable]`
**(a)** `modules/admin/src/lib.rs` (major rework), `admin.html.tmpl` + new
`login.html.tmpl`, `modules/admin/Cargo.toml`, new `modules/admin/src/password.rs`,
`modules/admin/src/tests.rs`, workspace `Cargo.toml` (add
`axum-extra = { version = "0.9", features = ["cookie"] }`),
`cmd/admin-svc/src/main.rs` (drop `.without_db()`, rewrite header doc),
`tools/topiccheck/src/main.rs` (`PLANELESS_PROCESSES` loses `"admin-svc"` + stale
comment at :491), `tools/requirecheck/src/main.rs:211` comment,
`modules/audit/src/lib.rs:340` comment.
**(b)** The heart of the package. `[R7]`: the module's new `register()` bails
without a DB, so the svc flip and the planeless-list removal MUST land in the same
commit — no boot-dead window exists at any commit boundary.
**(c)** How:
- **Lifecycle**: `register()` — `ctx.db()` (bail if `None`) + `ctx.bus()` into
  `AdminState`. `migrate()` applies:
  ```sql
  CREATE SCHEMA IF NOT EXISTS admin;
  -- USERS_DDL (pub const — adminctl reuses it, [R8]):
  CREATE TABLE IF NOT EXISTS admin.users (
      username   text PRIMARY KEY,
      pass_hash  text NOT NULL,
      created_at timestamptz NOT NULL DEFAULT now()
  );
  CREATE TABLE IF NOT EXISTS admin.sessions (
      token      text PRIMARY KEY,
      username   text NOT NULL REFERENCES admin.users(username) ON DELETE CASCADE,
      csrf_token text NOT NULL,
      created_at timestamptz NOT NULL DEFAULT now(),
      expires_at timestamptz NOT NULL
  );
  CREATE INDEX IF NOT EXISTS admin_sessions_expires_idx ON admin.sessions(expires_at);
  CREATE TABLE IF NOT EXISTS admin.login_attempts (
      subject      text PRIMARY KEY,   -- 'user:<name>' | 'ip:<addr>'
      fails        int  NOT NULL DEFAULT 0,
      locked_until timestamptz,
      updated_at   timestamptz NOT NULL DEFAULT now()
  );
  ```
- **Password**: `modules/admin/src/password.rs` — copy accounts' argon2id params +
  `hash_password`/`verify_password` (make them `pub` in this crate so adminctl can
  call them `[R8]`), add `argon2` dep.
- **Routes** (Basic auth deleted): `GET /admin/login`, `POST /admin/login`
  (form-encoded), `POST /admin/logout`. Login flow:
  1. Resolve client IP via `core/httpmw` client-ip (XFF walk, honors
     `TRUSTED_PROXY_CIDRS` — the admin process env; wired in scripts +
     checklist `[R2]`).
  2. Lockout check on `user:<u>` (threshold 5) and `ip:<addr>` (threshold 20)
     rows; if either `locked_until > now()` → 401 with the SAME generic body as a
     wrong password `[R11]`.
  3. Verify: fetch `pass_hash`; ALWAYS run `verify_password` (dummy PHC hash when
     the user is unknown — no user oracle in timing or status).
  4. Failure: upsert both rows `fails=fails+1`; when a row crosses its threshold set
     `locked_until = now() + least(2^fails, 900) * interval '1 second'` and — on the
     user row crossing — emit `admin.action{action:"login-locked"}` `[R1]`.
     Respond 401 generic.
  5. Success: delete both attempt rows; opportunistic
     `DELETE FROM admin.sessions WHERE expires_at <= now()`; mint session (token +
     csrf_token = 32B `OsRng` base64url each, `expires_at = now()+ 12h`); set cookie
     `admin_session=…; HttpOnly; SameSite=Strict; Path=/admin; Max-Age=43200`
     (+`Secure` unless `ADMIN_COOKIE_SECURE=0`, loud warn when off `[R4]`); emit
     `admin.action{action:"login-succeeded", actor:username}` in a small own-pool tx
     (match's `emit_tx` shape, `modules/match/src/lib.rs:101-113`); 303 → `/admin`.
- **Session check** replaces `check_auth`: cookie →
  `SELECT username, csrf_token FROM admin.sessions WHERE token=$1 AND expires_at>now()`;
  miss → 303 `/admin/login` (GET) / 401 (POST). `ADMIN_OPEN=1` bypasses sessions
  AND CSRF entirely (loud warn) `[R5]`. Zero-user boot allowed (warn: "no admin
  users — run ./install.sh"); startup no longer fails on missing env.
- **CSRF**: `item_post` + `logout` require `_csrf` == session's csrf_token
  (`ct_eq`, lib.rs:510-519) → else 403 (skipped entirely under `ADMIN_OPEN=1`
  `[R5]`). Hidden input injected by the admin template from the verified session
  (`page.csrf`), NOT via `adminapi::Form` — contract crates untouched, no
  public-api churn.
- **Security headers**: middleware on the admin router only: CSP
  `default-src 'self'; frame-ancestors 'none'`, `X-Frame-Options: DENY`,
  `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`.
- **Durable audit `[R1]`**: emits at login-succeeded, login-locked, logout — all
  LOCAL in admin-svc, so the feature is topology-proof. Additionally in `item_post`
  after a local `submit(values).await` Ok → `form-submit` (detail = joined field
  NAMES, never values). Emit failure after a successful mutation → error card
  "action applied but audit append failed". Not atomic with the owner module's tx
  (opaque closure) — documented as-is.
- **admin-svc flip**: `main.rs` uses `app::Config::from_env()` (DB on), header doc
  rewritten ("aggregator + session-auth owner, schema `admin`"); topiccheck
  `PLANELESS_PROCESSES` drops `admin-svc` (+ stale comments at topiccheck:491,
  requirecheck:211, audit lib.rs:340 updated `[R5][R10]`).
- **Tests** (live-Postgres, like accounts'): login ok / wrong-pass / unknown-user
  (identical 401 bodies `[R11]`), user lock at 5 + generic 401 while locked +
  unlock after window, IP row increments but does not lock below 20 `[R2]`,
  session expiry, CSRF reject + `ADMIN_OPEN` bypasses both `[R5]`, logout, zero-user
  boot, cookie flags incl. `ADMIN_COOKIE_SECURE=0` variant `[R4]`, emit rows in
  `asyncevents.events` for login-succeeded/login-locked/logout/form-submit `[R1]`.
- Gate: `cargo test -p admin_module -p checkmodules`, `cargo run -p topiccheck --
  --durability-strict`, `cargo run -p archcheck`. (No split-proof yet — scripts
  still reference the old env until Step 5; run/split-proof are known-red between
  Steps 2 and 5, unit/integration tests carry the interim.)

### Step 3 — `adminctl` CLI + `install.sh`/`install.ps1` `[opus]`
**(a)** New `tools/adminctl/` (Cargo.toml, `src/main.rs`, `src/lib.rs`,
`src/lib_tests.rs`), root `install.sh` + `install.ps1`, workspace members.
**(b)** After Step 2: reuses the module's `USERS_DDL` const and `password.rs` fns
(`adminctl` depends on the `admin` module crate `[R8]`).
**(c)** How:
- Mirror eventctl: hand-rolled args, `const USAGE`, `dsn()` with the same
  `DEFAULT_DSN`, `#[tokio::main] -> ExitCode`, logic in `lib.rs`, live-Postgres
  tests in `lib_tests.rs`. Deps: `sqlx`, `tokio`, `anyhow` + the `admin` module
  crate (hash fns + `USERS_DDL` — no argon2/DDL duplication `[R8]`).
- Subcommands: `create-user <username>` (executes `CREATE SCHEMA IF NOT EXISTS
  admin;` + `USERS_DDL`, then `INSERT … ON CONFLICT (username) DO UPDATE SET
  pass_hash = EXCLUDED.pass_hash` — doubles as password reset), `list`,
  `delete <username>`.
- Password input (NEVER argv): `--password-stdin` (one line from stdin) or env
  `ADMINCTL_PASSWORD`; error if neither. No new deps for no-echo prompting — the
  shell wrappers do the prompting.
- `install.sh`/`install.ps1` (repo root, paired like run/verify/split-proof):
  `./install.sh <username>`; password from `ADMINCTL_PASSWORD` or no-echo prompt
  (`read -s` / `Read-Host -AsSecureString`), piped to
  `cargo run -q -p adminctl -- create-user <u> --password-stdin`; prints next steps
  (TLS_MODE, ADMIN_HTTP_ADDR, TRUSTED_PROXY_CIDRS). PowerShell 5.1-compatible:
  ASCII only, no em-dashes.
- Tests: upsert-then-verify with the module's own `verify_password`, reset changes
  hash, delete removes, create-user works on a FRESH schema-less DB (the installer
  precondition).

### Step 4 — Native TLS in the HTTP front (`core/app` + `cmd/gateway-svc`) `[fable]`
**(a)** Workspace `Cargo.toml`, `core/app/Cargo.toml` + `src/lib.rs`,
`cmd/gateway-svc/src/main.rs`.
**(b)** Independent of Steps 1-3; before Step 5 so scripts can set `TLS_MODE=off`.
**(c)** How:
- **Crypto-provider strategy `[R3]` (the review's runtime-panic trap)**: the
  workspace pins rustls with `ring` and `core/edge` deliberately avoids the
  process-global provider. New deps are pinned to ring and the global default is
  installed EXPLICITLY:
  - `axum-server = { version = "0.7", features = ["tls-rustls-no-provider"] }`
    (NOT `tls-rustls`, which drags aws-lc-rs);
  - `rustls-acme = { version = "0.14", default-features = false, features = ["ring", "axum"] }`
    (verify at implementation time that 0.14 is the current 0.23-rustls line and
    the feature names match — if the axum feature turns out axum-server-major
    -incompatible, fall back to rustls-acme's `Incoming` stream + manual accept
    loop; both integration shapes are supported by the crate);
  - `cmd/gateway-svc/main.rs` calls
    `rustls::crypto::ring::default_provider().install_default()` once before
    building any TLS config, so default-provider builders inside axum-server/
    rustls-acme resolve to ring. `cargo tree -i aws-lc-rs` must come back EMPTY —
    add that check to the step's done-criteria (no NASM/CMake native build on this
    Windows box).
- `core/app`: `pub enum TlsFront { Files { cert: PathBuf, key: PathBuf }, Acme {
  domains: Vec<String>, cache_dir: PathBuf, contact: Option<String> } }` +
  `Config::with_tls(Option<TlsFront>)`. Serve path: `None` → today's `axum::serve`
  branch byte-identical; `Some(Files)` → `axum_server` with
  `RustlsConfig::from_pem_file`; `Some(Acme)` → `AcmeConfig::new(domains)
  .contact(…).cache(DirCache::new(cache_dir)).directory_lets_encrypt(true).state()`
  → axum acceptor + spawned state-driver task (logs issuance/renewal; aborted on
  shutdown). Graceful shutdown: `axum_server::Handle::graceful_shutdown(
  Some(http_drain_grace))` wired to the same `sig_rx` — drain-bounded semantics
  preserved on both branches.
- `cmd/gateway-svc/main.rs`: parse `TLS_MODE` (`off` default | `files` | `acme`),
  `TLS_CERT_PATH`/`TLS_KEY_PATH` (files: both required else bail), `ACME_DOMAINS`
  (comma-separated, required for acme), `ACME_CONTACT` (optional), `ACME_CACHE_DIR`
  (default `run/acme-cache`). Fail loudly on partial config. Modules see nothing.
  `[R12]` Other mains untouched — the monolith does NOT get TLS by setting envs;
  if it ever should, `cmd/server/main.rs` adds the same ~10 parse lines (mechanism
  is already in core/app). Only gateway-svc parses today: single public front door.
- ACME is not E2E-testable locally → `files` mode is the tested path (integration
  test: bind ephemeral port with an rcgen-minted localhost cert, reqwest client
  trusting the test CA, roundtrip through the full `app::run` TLS branch); ACME
  gets config-parse unit tests + the Hetzner manual checklist.

### Step 5 — Scripts: run/split-proof/verify wiring + new assertions `[opus]`
**(a)** `run.sh`/`run.ps1`, `split-proof.sh`/`split-proof.ps1`, `verify.sh`/
`verify.ps1` (csharp stage env + stale comment `[R10]`).
**(b)** After Steps 2-4: everything it exercises exists; this step turns
run/split-proof green again.
**(c)** How:
- `run.sh`/`run.ps1`: drop `ADMIN_USER`/`ADMIN_PASS`; seed dev admin after DB
  reachable (monolith: before `start_server`; split: next to the edgeca mint,
  run.sh:178-181): pipe `admin` into `cargo run -q -p adminctl -- create-user admin
  --password-stdin`. Set `TLS_MODE=off` and `ADMIN_COOKIE_SECURE=0` `[R4]`
  explicitly (dev opt-outs, keep the "explicit opt-ins" doctrine comment updated);
  set `TRUSTED_PROXY_CIDRS=127.0.0.1/32` on the admin process (and monolith) so the
  XFF path is exercised, not bypassed `[R2]`. Print dev creds in the banner.
- `verify.sh`/`verify.ps1` csharp stage: drop the `ADMIN_USER=admin ADMIN_PASS=admin`
  env + fix the fail-closed comment; boot relies on zero-user-boot `[R10]`.
- `split-proof.sh`/`.ps1`: seed `proofadmin` AND `prooflock` (dedicated lockout
  user `[R2]`) via adminctl pre-boot; admin-svc env gains
  `TRUSTED_PROXY_CIDRS=127.0.0.1/32`, `ADMIN_COOKIE_SECURE=0`. Replace
  `[AD1]`/`[K5]`/`[M3]`:
  - `[AD1]` GET `/admin` unauthenticated → 303 with `Location: /admin/login`.
  - `[AD2]` POST `/admin/login` as `prooflock`, 6 wrong passwords → all answer 401
    with the identical generic body `[R11]`; then `pg` asserts
    `SELECT fails, locked_until IS NOT NULL FROM admin.login_attempts WHERE
    subject='user:prooflock'` → `fails>=5`, locked. IP row exists with fails=6 but
    NOT locked (threshold 20) `[R2]` — assert `locked_until IS NULL` on
    `subject LIKE 'ip:%'`.
  - `[AD3]` login as `proofadmin` (cookie jar `curl -c`), assert 303 +
    `admin_session` cookie; GET `/admin/api-keys` `[R9]` with `-b jar` → 200 (the
    old `[K5]` remote-page assertion rides this session).
  - `[AD4]` POST `/admin/api-keys` `[R9]` with valid session, no `_csrf` → 403.
    (This targets the apikeys page which is REMOTE in admin-svc — expected answer
    for a remote item is 405; assert the CSRF 403 comes FIRST i.e. the request is
    rejected before the remote/local decision. If implementation orders 405 first,
    the assertion pins whichever admin-svc actually answers — decide in Step 2 and
    keep the proof in lock-step: the plan's order is CSRF → editability, so 403.)
  - `[AD5]` `pg`: `SELECT count(*) FROM asyncevents.events WHERE
    topic='admin.action'` ≥ 2 (login-succeeded + at least one of
    login-locked/logout `[R1]` — no form-submit in the split, remote forms are
    read-only), and the audit ledger shows `admin.action` rows (poll loop like
    sh:859-862).
  - `[M3]` monolith-parity phase `[R6]`: full session login against the monolith
    front (fresh cookie jar), NOT `curl -u`; before the parity phase run
    `pg "DELETE FROM admin.login_attempts; DELETE FROM admin.sessions;"` so split-
    phase lockout/session residue can't poison parity `[R6]`. In the monolith the
    apikeys form is LOCAL → parity also asserts a real `form-submit` emit
    (`[M3b]`: submit the apikeys edit form WITH `_csrf`, then `pg` count of
    `admin.action` where `payload->>'action'='form-submit'` ≥ 1) `[R1]`.
  - Keep the isolated-admin-instance scenario in sync (ps1:1106 area).

### Step 6 — Gateway stub-coverage tripwire `[opus]`
**(a)** `tools/archcheck/src/main.rs` (+tests.rs), `tools/checkmodules/src/tests.rs`.
**(b)** Independent; sequenced last-but-one to keep one rollout at a time.
**(c)** Two halves mirroring the rule-12 dual-check pattern:
- archcheck (textual): each `api/<name>/api/src/lib.rs` containing boundary-checked
  `#[http(` (comment lines skipped, existing `grep_*` style) requires
  `cmd/gateway-svc/src/lib.rs` to contain `Stub::new("<name>"`. Extra stubs
  (apikeys) fine; missing → violation in the shared vec (fortress stage picks it
  up, zero verify.sh changes).
- checkmodules (semantic): new `#[test]` — `gateway_svc::modules(&w, None)` name-set
  must be a superset of the `#[http(`-bearing domain dirs (filesystem walk like
  `monolith_hosts_every_modules_dir`, tests.rs:55-82). Rides `cargo test`.

### Step 7 — Bless, verify, docs `[inline]`
**(a)** `docs/reference/public-api-baseline/adminevents.txt`, `CLAUDE.md`, final
verification, Hetzner checklist doc.
**(b)** Last: needs everything merged.
**(c)** `./verify.sh --bless-public-api` (exactly ONE new baseline file expected —
adminevents; any adminapi diff = STOP and review). Then the full gate (ONE run at a
time): `./verify.sh`, then `./verify.sh --all`. CLAUDE.md updates: admin module
paragraph (sessions/CSRF/lockout/installer/`ADMIN_COOKIE_SECURE`, env removal,
`ADMIN_OPEN` semantics), commands (`./install.sh`), gateway TLS env table, smoke
section. New `docs/reference/hetzner-deploy-checklist.md`: DNS →
`TLS_MODE=acme ACME_DOMAINS=… ACME_CONTACT=…`, `TRUSTED_PROXY_CIDRS` for the real
proxy hop topology `[R2]`, `install.sh` with a real password, confirm issuance in
logs, `/admin` over https, lockout burst check via `admin.login_attempts`.
Memory-sync if memory changed.

## Verification (end-to-end)

1. `cargo test --workspace` green (admin/adminctl live-Postgres tests included).
2. `cargo run -p archcheck` / `-p topiccheck -- --durability-strict` green (new
   rule + new topic + admin-svc off planeless).
3. `./split-proof.sh` green incl. `[AD1]-[AD5]` + `[M3]/[M3b]` (session login,
   asymmetric lockout via DB, CSRF, durable admin.action in BOTH topologies —
   auth events in split, form-submit in monolith `[R1]`).
4. `./run.sh`; browser: `http://localhost:8082/admin` → login → `admin`/`admin` →
   portal, forms carry `_csrf`, cookie flags in devtools (Secure off in dev via
   `ADMIN_COOKIE_SECURE=0` `[R4]`).
5. TLS files-mode integration test (rcgen cert, full `app::run` TLS branch);
   `cargo tree -i aws-lc-rs` empty `[R3]`.
6. `./verify.sh` full blocking pass; bless produced exactly one new baseline file.
7. Hetzner-day manual checklist executed from
   `docs/reference/hetzner-deploy-checklist.md`.

## Dispatch summary

| Step | Lane | Effort |
|------|------|--------|
| 0 plan commit | inline | — |
| 1 adminevents+audit+topiccheck | sonnet | think |
| 2 admin core + svc flip (one commit) | fable | think hard |
| 3 adminctl+install | opus | think |
| 4 TLS core/app+gateway-svc | fable | think hard |
| 5 scripts+assertions | opus | think |
| 6 stub tripwire | opus | think |
| 7 bless/verify/docs | inline | — |

Test-rollout protocol applies throughout: ONE `cargo test`/verify/split-proof at a
time on this machine; every subagent prompt includes the check. Known-red window
for run/split-proof between Steps 2 and 5 (old env vars deleted) — no split-proof
runs in that window, unit/integration tests carry it.
