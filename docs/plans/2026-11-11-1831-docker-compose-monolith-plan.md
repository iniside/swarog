# Docker Compose for the monolith — plan

## Context

**Owner's request (2026-11-11, own words, paraphrased from Polish):** "Let's do docker
compose for the monolith now, it's the simplest thing." Base image: **distroless
`cc-debian12`** (owner chose it over Alpine and scratch+musl). Two variants: **"one dev and
one secure by default"**. Added by the owner after research showed the monolith cannot
terminate TLS: **"we do it, otherwise the safe monolith doesn't work in practice"** — the
monolith gets the same native TLS front as `gateway-svc` (Step 0).

**Explicit exclusions:** the split in compose (part of the overall deliverable per memory
`docker-is-distribution-and-multihost-bench`, but needs a peer-address decision first —
literal-IP parse at `core/remote/src/lib.rs:435`); per-service edge certificates; a real
player-QUIC certificate; the installer; the module-selection configurator.

**Overlap map:**
- `cmd/gateway-svc/src/tlsenv.rs` — the only `TLS_MODE`/`ACME_*` env parser today, private
  to gateway-svc. Step 0 RELOCATES it into `core/app` next to the mechanism it feeds
  (`app::TlsFront`, `Config::with_tls`, `serve_https`); no twin is left behind. Precedent
  for one parser driven by both front mains: `gateway::PushLimits::from_values`
  (`modules/gateway/src/push_ws.rs:182`).
- `devctl up monolith` (`tools/devctl`; env from
  `tools/processctl/src/fleet.rs::game_backend_monolith`) — the native dev supervisor.
  Compose does not replace or call it; the dev overlay copies its env VALUES. Why not have
  devctl emit compose: devctl builds on the host and supervises native processes, compose
  builds in a container — different build owner. Duplicated ~8-key list accepted; generator
  is a split-compose concern (19 services). Recorded as leftover.
- `experiments/jvm-quarkus-sketch/infra/docker-compose.yml` — archived JVM sketch; not reused.
- `install.sh` / `adminctl` — admin creation; reused inside the image, no new mechanism.

**Facts verified in code (2026-11-11):**
- The monolith is a public front door: `cmd/server/src/main.rs` "fronts the same two public
  planes as gateway-svc". Yet only `cmd/gateway-svc/src/main.rs:93` calls
  `tlsenv::tls_front_from_env()` / `.with_tls(tls)` (`:184`); `cmd/server` never does, so the
  monolith is HTTP-only. The justification in `tlsenv.rs:1-5`, `core/app/src/lib.rs:122-127`
  and `gateway-svc/src/main.rs:90-92` ("[R12]: no other cmd/* main is TLS-aware … single
  public front door") is false — a split-only feature (`never-monolith-only-features`,
  inverse direction).
- `serve_https` builds rustls configs via the process default provider; the workspace
  compiles only `ring` (`core/app/src/lib.rs:1336-1340`), and gateway-svc additionally pins
  it in `main` (`gateway-svc/src/main.rs:81-87`).
- Config is env-only (`core/app/src/lib.rs:230-248`); `:PORT` binds `0.0.0.0`
  (`to_bind_addr`, `:566`). Player QUIC default `:9100`.
- **Set-but-empty env fails boot for mail:** `MailConfig::from_vars` rejects
  `set but empty` (`modules/mail/src/config.rs:117-121`). Compose pass-through MUST NOT
  create empty vars → bare list-form `- KEY` (Compose omits unset keys). Applied to every
  pass-through key below.
- **Compile-time file reads:** `core/admin/src/lib.rs:65-66` `include_str!`s
  `UILayout/design-system/tokens.css` and `.../components/bundle.css`. No `build.rs` in the
  repo. No runtime asset reads from disk in `core/`, `modules/`, `demos/` (grep lower bound;
  only edge PEM loads when `EDGE_CA_*` set). Templates/CSS are compiled in.
- Edge CA: with no `EDGE_CA_*` the process generates an ephemeral CA, loud warning
  (`core/edge/src/tls.rs:358`). The monolith has no internal edge server; the only consumer is
  the player QUIC cert. `edgeca` overwrites on every run, so a mint service would rotate the
  anchor each `up` → **no CA minting in compose**; the player port is therefore not usable by
  verifying clients from outside (documented, leftover).
- Admin seeding: `adminctl` runs `CREATE SCHEMA IF NOT EXISTS admin` + the server's DDL;
  server setup is `IF NOT EXISTS`; `create-user` is an upsert, default role `admin`
  (`tools/adminctl/src/lib.rs:128-180`). devctl seeds before the fleet
  (`tools/devctl/src/supervisor.rs:239`) — compose keeps the order.
- Admin cookie `Secure` by default (`core/admin/src/lib.rs:462`). Per-IP limiters use
  `TRUSTED_PROXY_CIDRS` (`core/admin/src/lib.rs:379`, `core/app/src/lib.rs:1078`).
- Dev env values (`fleet.rs:983-1003`): `APIKEYS_DEV_SEED=1, ACCOUNTS_DEV_AUTH=1,
  INVENTORY_DEV_GRANT=1, WALLET_DEV_SEED=1, ADMIN_COOKIE_SECURE=0, MAIL_PROVIDER=log,
  MAIL_FROM=dev@localhost, DATABASE_POOL_MAX_CONNECTIONS=20`. Not copied:
  `TLS_MODE=off` (it is the default) and `TRUSTED_PROXY_CIDRS=127.0.0.1/32` (host traffic
  arrives from the Docker bridge, loopback trust is meaningless).
- Toolchain: `rust-toolchain.toml` = `stable` + darwin/windows-gnu targets; local
  `rustc 1.99.1`; edition 2021. Docker 29.9.2 / Compose v5.6.1 available locally.

## Steps

### Step 0 — one TLS-front env parser, used by both front mains `[opus]`

**(a) Touched:**
- `core/app/src/lib.rs`: new `impl TlsFront { pub fn from_values(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<TlsFront>> }`
  with the body of `parse_tls_front` moved verbatim (modes `off`|`files`|`acme`,
  partial-config errors, unknown-mode error, trim/blank = unset) and
  `const DEFAULT_ACME_CACHE_DIR: &str = "run/acme-cache"` moved with it.
  `core/app` dependencies unchanged (no provider install in `serve_https`: it runs last in
  `app::run`, after module init and the player listen, so it would precede nothing).
- `cmd/gateway-svc/src/main.rs`: `let tls = app::TlsFront::from_values(|k| std::env::var(k).ok())?;`
  replaces `tlsenv::tls_front_from_env()`; delete `mod tlsenv;` / `mod tlsenv_tests;`.
- Delete `cmd/gateway-svc/src/tlsenv.rs` and `cmd/gateway-svc/src/tlsenv_tests.rs` (the
  tests return in Step 0b against the new home).
- `cmd/server/src/main.rs`: first line after `tracing_subscriber` init:
  `let _ = rustls::crypto::ring::default_provider().install_default();` — the same pin and
  placement as `cmd/gateway-svc/src/main.rs:88`, the only placement that precedes every TLS
  builder in the process; add `rustls = { workspace = true }` to `cmd/server/Cargo.toml`.
  Then the same `from_values` call before `app::run`, and
  `app::Config::from_env().with_tls(tls)`.
- False/stale comments fixed in the same step: `core/app/src/lib.rs:122-127`, `:198` and the
  `serve_https` crypto-provider note `:1336-1340` (now: "every public front main pins ring");
  the `[R12]` comment at `cmd/gateway-svc/src/main.rs:90-92`; the `tlsenv_tests` mention at
  `cmd/gateway-svc/src/addrs_tests.rs:8`. Rewrite to the invariant ("every public front
  main parses TLS via `TlsFront::from_values`"). Grep `tlsenv|parse_tls_front|tls_front_from_env`
  after the edit must return 0 hits outside `docs/` history.

**(b) First:** Step 1's secure variant passes `TLS_MODE` to the monolith; without this the
var is silently ignored.

**(c)** Env stays read in the mains (closure), parsing has one authority — the
`PushLimits::from_values` shape. No behaviour change for gateway-svc.

**(d)** `[opus]` → `core-implementer`, `model:"opus"`, effort `medium`. Verification:
`cargo check -p app -p server -p gateway-svc` (per `safe-verification`, no test run in
this step). Commit `feat(app,server): give the monolith the same native TLS front as gateway-svc (docker-compose-monolith step 0)`.

### Step 0b — tests for the relocated parser and the monolith wiring `[test-author]`

**(a)** New `core/app/src/tls_front_tests.rs`, wired as `#[cfg(test)] mod tls_front_tests;`
in `core/app/src/lib.rs`: port all 8 cases from the deleted file
(`git show <step-0-sha>^:cmd/gateway-svc/src/tlsenv_tests.rs` — the orchestrator passes the
Step 0 commit hash; never `HEAD~1`, other commits may land in between) to
`TlsFront::from_values` with a map-backed closure: `unset_and_blank_and_off_mean_plain_http`,
`unknown_mode_bails_loudly`, `files_mode_requires_both_paths`,
`files_mode_is_case_insensitive`, the three `acme_mode_*`,
`tls_paths_ignore_acme_vars_and_vice_versa`. `cmd/server`: none — `main` is not unit-testable; the previously-wrong branch
(monolith ignoring `TLS_MODE`) is proven live in Step 3.2 (`TLS_MODE=files` → HTTPS answers,
plain HTTP does not).

**(b)** After Step 0 lands and compiles.

**(c)** Run `cargo test -p app tls_front_tests` (expect exactly 8 passed, report the
count) and `cargo test -p gateway-svc` (one rollout at a time, `safe-verification`).

**(d)** `test-author`, `model:"sonnet"`. Commit `test(app): cover TlsFront::from_values (docker-compose-monolith step 0b)`.

### Step 0c — integration test: the real monolith binary serves HTTPS `[test-author]` (opus)

Owner-approved 2026-11-11 (option A) to close the gap Step 0b leaves: nothing committed
proves `cmd/server` passes `TlsFront` into `with_tls`.

**(a)** New `cmd/server/tests/tls_front.rs` (+ `[dev-dependencies]` in
`cmd/server/Cargo.toml`: `testdb`, `rcgen`, `reqwest`, `tokio` — all workspace deps already
used by `core/app` tests).

**(b)** After Step 0 lands (the binary must read `TLS_MODE`). Independent of 0b and 1
(disjoint files).

**(c)**
- Gate on `testdb::test_pool()` (strict semantics: unreachable Postgres panics unless
  `TESTDB_ALLOW_SKIP`). Target `testdb::dsn()` — the shared dev database, as devctl /
  verifyctl monolith fleets do (the role has no CREATEDB; `scripts/db-provision.sh:137`
  creates the DB as superuser). `DATABASE_POOL_MAX_CONNECTIONS=4`.
- Mint a `localhost` cert/key with `rcgen` into a temp dir; pick a free TCP port (bind
  `127.0.0.1:0`, read, drop) for `PORT` and another free UDP port for `PLAYER_EDGE_ADDR`.
- Spawn `env!("CARGO_BIN_EXE_server")` with `TLS_MODE=files`, `TLS_CERT_PATH`,
  `TLS_KEY_PATH`, `PORT=127.0.0.1:<p>`, `MAIL_PROVIDER=log`, `MAIL_FROM=dev@localhost`, a
  cleared inherited `TLS_*`/`ACME_*` env, stdout/stderr captured to a file in the temp dir.
  A drop guard kills and waits the child on every exit path (incl. assertion panic).
- Readiness: poll `https://127.0.0.1:<p>/healthz` with a `reqwest` client trusting the
  minted cert (`add_root_certificate`), connecting via `localhost` SNI (`resolve("localhost", addr)`),
  until 200 or a hang-guard deadline with generous headroom (60s — monolith migrates every
  schema); on timeout fail with the captured server log. Also fail fast if the child exits.
- Assert: HTTPS `/healthz` = 200; a plain `http://127.0.0.1:<p>/healthz` request does NOT
  return an HTTP response (connection error / non-HTTP).
- Previously-wrong branch: with Step 0's `.with_tls(tls)` removed from `cmd/server`,
  the HTTPS poll never succeeds and the plain-HTTP assertion fails — state this in the test
  doc comment and demonstrate it once locally (temporarily revert the line, run, restore;
  report both outcomes; do not commit the revert).
- Timing doctrine (memory `timing-sensitive-tests-doctrine`): no fixed sleeps as proof;
  poll with backoff only up to the hang-guard.

**(d)** `test-author`, `model:"opus"` (novel harness: whole-monolith process spawn), effort
`medium`. Run per `safe-verification`: `cargo test -p server --test tls_front`, one rollout.
Commit `test(server): prove the monolith binary serves HTTPS under TLS_MODE=files (docker-compose-monolith step 0c)`.

### Step 1 — Dockerfile, .dockerignore, compose files `[sonnet]`

**(a) New files, repo root:** `Dockerfile`, `.dockerignore`, `compose.yaml`,
`compose.dev.yaml`, `.env.example`; `.gitignore` gains `tls/`.

**(b)** After Step 0 (the secure variant relies on `TLS_MODE` in the monolith).

**(c)**
- `Dockerfile`:
  - builder `rust:1.99.1-slim-bookworm` (bookworm ⇒ glibc matches `cc-debian12`);
    `ENV RUSTUP_TOOLCHAIN=1.99.1` (overrides `rust-toolchain.toml`, whose `stable` + cross
    targets would download on every cold build). `COPY . .`, then
    `cargo build --release --locked -p server -p adminctl` with BuildKit cache mounts for
    `/usr/local/cargo/registry` and `/src/target`; copy both binaries to `/out/` in the
    same `RUN`; `mkdir -p /out/acme`.
  - runtime `gcr.io/distroless/cc-debian12:nonroot@sha256:<digest>` (resolve via
    `docker buildx imagetools inspect`, pin). Binaries → `/usr/local/bin/{server,adminctl}`;
    `COPY --from=builder --chown=65532:65532 /out/acme /var/lib/swarog/acme` (so a named
    volume mounted there inherits nonroot ownership — distroless has no shell to chown).
    `ENTRYPOINT ["/usr/local/bin/server"]`, `EXPOSE 8080/tcp 9100/udp`. No HEALTHCHECK.
- `.dockerignore`: `target/ run/ deploy/ *.exe *.zip mutants.out/ nocommit/ memory/
  experiments/ clients/ docs/ .git/ .claude/ tls/ .env .env*`, and `UILayout/` followed by
  `!UILayout/design-system/` (compile-time `include_str!` inputs, `core/admin/src/lib.rs:65-66`).
  Guard before finalizing: no workspace member AND no `include_str!`/`include_bytes!` target
  lies under an excluded path (Grep `include_(str|bytes)!` over `core modules api cmd demos tools`).
- `compose.yaml` — **secure default** (`docker compose up`), project `name: swarog`:
  - `postgres`: `postgres:17.<minor>-alpine` exact tag (state which);
    `POSTGRES_USER`/`POSTGRES_DB=gamebackend`, `POSTGRES_PASSWORD=${SWAROG_DB_PASSWORD:-}`
    (empty ⇒ the image refuses to initialize — loud); volume `pgdata`; healthcheck
    `pg_isready -h 127.0.0.1 -U gamebackend -d gamebackend` (TCP, not the init-time socket);
    no host port.
  - `server`: `build: .`, `image: swarog:local`; environment (list form):
    `DATABASE_URL=postgres://gamebackend:${SWAROG_DB_PASSWORD:-}@postgres:5432/gamebackend?sslmode=disable`,
    `PORT=:8080`, `PLAYER_EDGE_ADDR=:9100`, `DATABASE_POOL_MAX_CONNECTIONS=20`,
    `ACME_CACHE_DIR=/var/lib/swarog/acme`, and BARE pass-through keys (omitted when unset):
    `TLS_MODE TLS_CERT_PATH TLS_KEY_PATH ACME_DOMAINS ACME_CONTACT TRUSTED_PROXY_CIDRS
    MAIL_PROVIDER MAIL_FROM MAIL_SMTP_HOST MAIL_SMTP_PORT MAIL_SMTP_USERNAME
    MAIL_SMTP_PASSWORD MAIL_SMTP_TLS`.
    Ports `${SWAROG_BIND:-127.0.0.1}:${SWAROG_HTTP_PORT:-8080}:8080` and
    `${SWAROG_BIND:-127.0.0.1}:9100:9100/udp`. Volumes `acme:/var/lib/swarog/acme`,
    `${SWAROG_TLS_DIR:-./tls}:/tls:ro`. `read_only: true`, `cap_drop: [ALL]`,
    `security_opt: [no-new-privileges:true]`, `depends_on: postgres: service_healthy`,
    `restart: unless-stopped`.
- `compose.dev.yaml` — overlay (`docker compose -f compose.yaml -f compose.dev.yaml up`):
  top-level `name: swarog-dev` (own containers, network and volumes — a dev DB never
  shadows the secure DB's password); `postgres` `POSTGRES_PASSWORD=gamebackend`; `server`
  `DATABASE_URL` with `gamebackend` + the dev values from Context; service `admin-seed`
  (`image: swarog:local`, `entrypoint: ["/usr/local/bin/adminctl"]`,
  `command: ["create-user","admin","--role","admin"]`, `ADMINCTL_PASSWORD=admin`, same
  `DATABASE_URL`, `depends_on: postgres: service_healthy`, `restart: "no"`);
  `server.depends_on.admin-seed: condition: service_completed_successfully`.
- `.env.example`: `SWAROG_DB_PASSWORD=` (URL-safe chars only — embedded in `DATABASE_URL`),
  commented `SWAROG_BIND`, `SWAROG_HTTP_PORT`, `TLS_*`/`ACME_*` (ACME needs
  `SWAROG_BIND=0.0.0.0`, `SWAROG_HTTP_PORT=443`, public DNS), `TRUSTED_PROXY_CIDRS`
  (set it when behind a reverse proxy), `MAIL_*` (unset ⇒ server boots, `/readyz` red by
  design until a provider is configured; never write `KEY=` with an empty value).
- Comments: only the two that earn a line (why admin-seed precedes server; why
  `!UILayout/design-system/`).

**(d)** `[sonnet]` → `core-implementer`, `model:"sonnet"`, effort `medium`. Runs
`docker compose build` once (proves the build, reports image size); does NOT run the fleet.
Commit `feat(docker): add a distroless image and secure and dev compose files for the monolith (docker-compose-monolith step 1)`.

### Step 2 — docs `[docs]`

**(a)** `README.md` (~34, ~54, ~228 "no containers" + new "Run with Docker" under Getting
started); `docs/reference/weles-design.md` ~202 (Docker is a distribution path and
multi-host test bench outside Weles; Weles still supervises native processes — leave ~730,
Weles's own rejected list, untouched); `docs/reference/testing.md` ~10 (tests still use
local Postgres; Docker is not a test dependency); any `docs/reference` prose claiming only
gateway-svc terminates TLS (Grep `TLS_MODE` in `docs/reference`).

**(b)** After Steps 0 and 1 — documents landed code.

**(c)** README: both commands; `.env` requirement; switching variants keeps separate data;
admin creation in the secure variant without echo/argv:
`read -rs ADMINCTL_PASSWORD; export ADMINCTL_PASSWORD; docker compose run --rm -T -e ADMINCTL_PASSWORD --entrypoint /usr/local/bin/adminctl server create-user <name>`;
TLS options (`files` via `./tls` — on a Linux host the key must be readable by uid 65532,
e.g. mode 644 or `chown 65532`; `acme` on 443) or a reverse proxy + `TRUSTED_PROXY_CIDRS`;
player QUIC port not usable by verifying clients yet (ephemeral CA); split not in compose
yet. Smoke commands reuse the existing curl pair.

**(d)** `docs-writer`, `model:"sonnet"`. Commit
`docs(docker): document running the monolith with docker compose (docker-compose-monolith step 2)`.

### Step 3 — live acceptance (orchestrator; docker commands only)

Gate first (`safe-verification`): no live cargo/rustc, no devctl fleet on 8080.
1. Dev: `up -d --build`; `admin-seed` exits 0; `/readyz` 200; README curl pair succeeds;
   `/admin` login admin/admin (browser named in the report); `/` renders. `down -v`.
2. Secure: no `.env` → postgres refuses init. With `SWAROG_DB_PASSWORD`: boots; `/readyz`
   red (no mail, expected); `dev-key-server` → 401; adminctl `run` creates a login that
   signs in. Then `TLS_MODE=files` with a self-signed cert (Git-bash `openssl`) in `./tls`:
   `curl -k https://localhost:8080/healthz` 200 and plain `http://` fails — proves the
   compose TLS wiring (mounts, env pass-through); the monolith-TLS code path itself is
   proven by Step 0c. `down -v`.
Failure ⇒ stop and report options (`live-acceptance-report-dont-fix`), no fix loop.

### Review

`core-reviewer` `model:"opus"` (≥ every author tier), effort `medium`, after each of Steps
0, 0b, 0c, 1, 2. Steps 0b, 0c and 1 touch disjoint files → implementations may be
dispatched together after Step 0's commit, but their cargo test runs are serialized (one
rollout at a time); Step 1 runs only `docker compose build`, no cargo test.

## Leftovers (out of scope, recorded)
- Split in compose: peer-address decision (static IPs vs DNS in `core/remote`), shared CA
  via `EDGE_CA_*`, compose generated from the fleet source of truth.
- Real player-QUIC certificate (ephemeral dev CA today) — already tracked as GATEWAY-07 in
  `docs/todo/gateway.md`, owner-prioritized as the next item after this plan.
- Dev env values duplicated from `game_backend_monolith`.
