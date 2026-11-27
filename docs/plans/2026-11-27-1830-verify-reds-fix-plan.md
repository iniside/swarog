# Verify reds left by earlier plans (clippy, fortress, audit, processctl)

Date: 2026-11-27
Status: approved (owner, 2026-11-27: "ok zaczynaj z rekomendacja")
Plan review: core-reviewer (opus, high), APPROVE; its fact corrections, checks and TODO list are applied below.
Q1 answered: the recommended shape (the lookup reads the filtered map).

## Context

Owner (2026-11-27), after the oidc-authorization-code plan-end report: "no dobra zrob plan". The outcome
is a green `verifyctl --all --strict` except for the admindiff mockup drift.

**Explicit exclusions:**
- `tools/admindiff/` (8 failing `--lib` tests, mockup menu drift): the owner's call, not this plan.
- chrono `RUSTSEC-2026-0543` (unmaintained, no fixed release): a warning, and it does not fail the stage.
- The C# client. Any new archcheck rule or verifyctl change.

None of these reds comes from A3. The plan-end run is `run/verify/verify-1795797185-77912-e22ec858`.

## Facts (HEAD 4cc66b58; path:line read 2026-11-27)

1. **clippy:** `modules/config/src/tests.rs:423` `svc.set_tx(&mut *tx, ...)` fails `explicit_auto_deref`.
   Source: admin-second-pass step 10.
2. **fortress, config:** `modules/config/src/lib.rs:113` `SCHEMA_DDL_TEMPLATE` holds the schema, tables and
   both trigger functions. Its name does not end in `_DDL`, so archcheck rule 22 (main.rs:1656-1720) reports
   6 `CREATE` phrases outside a `*_DDL` item. The text reaches Postgres correctly: `static SCHEMA_DDL:
   LazyLock<String>` (lib.rs:222-224) fills `@MAX_REVISIONS@` from `store::MAX_REVISIONS` and is passed to
   `Ddl::new(pool).sql(&SCHEMA_DDL)` (lib.rs:601-602). Rule 22 exempts anything inside a `*_DDL` item, so
   the use of the template inside the `SCHEMA_DDL` initialiser is exempt. Its hint at main.rs:1711 names
   this LazyLock shape. Source: f08390d5 (admin-second-pass step 10).
3. **fortress, inventory:** in `modules/inventory/src/lib.rs` `SCHEMA_DDL` (from :55):
   - :91 is an SQL comment ending "... without a call into characters. Integrity ...". Rule 15
     (`foreign_schema_sql_refs`, main.rs:2190) reads `into characters.` as a foreign-schema write. It skips
     Rust comment lines, not `--` SQL comments inside a literal. Source: ccf29df3.
   - :97 `CREATE INDEX IF NOT EXISTS character_owners_player_idx ON inventory.character_owners (player_id)`
     (ccf29df3) and :109 `CREATE INDEX IF NOT EXISTS operator_grants_at_idx ON inventory.operator_grants (at)`
     (5f77efea). Both hit the locking-DDL form `INDEX IF NOT EXISTS`.
   - `migrate` (:208-219) already builds `Ddl::new(pool).sql(SCHEMA_DDL).index(..).index(..)`.
     `Ddl::index` (core/lifecycle/src/ddl.rs:118) runs its SQL only when `to_regclass(name)` is NULL, so an
     index already created by the old `IF NOT EXISTS` text is kept as it is. No DROP or reboot is needed.
4. **audit** (`tools/verifyctl/src/stages/audit.rs:50-75`, `cargo audit` with two ignores): Cargo.lock has
   - lettre 0.11.23: RUSTSEC-2026-0560 to 0565, fixed in >=0.11.24. The workspace spec is `"0.11"` (Cargo.toml:282).
   - url 2.5.8: RUSTSEC-2026-0592, fixed in >=2.5.9. The spec is `"2"` (Cargo.toml:246).
   - cookie 0.18.1: RUSTSEC-2026-0594, fixed in >=0.18.3. It comes in through axum-extra, cookie_store and
     reqwest.
   All three fixes are semver-compatible lock bumps. No manifest change is needed.
5. **test, processctl:** `fleet_tests::sanitized_build_path_contains_the_discovered_msvc_linker`
   (fleet_tests.rs:628, assert at :643) fails only under verifyctl: "first PATH entry ...ring.../out has no link.exe".
   - verifyctl spawns every stage with only `EnvironmentSnapshot::build_environment()` (runner.rs:248,268,392); on
     Windows it replaces the parent environment (processctl platform/windows.rs:170,484).
   - `BUILD_ENV_ALLOWLIST` (fleet.rs:9-15) has `ProgramFiles(x86)` but not `ProgramFiles`.
   - `append_msvc_linker_path` (fleet.rs:699-750) reads the PROCESS environment (`std::env::var_os("ProgramFiles")`,
     :703), not the filtered map it is building; its comment says the VS 2022+ IDE editions install there. It
     started reading `ProgramFiles` in da737d80 (2026-11-01) without an allowlist entry.
   - On this machine VS (release 18) is only under `C:/Program Files/Microsoft Visual Studio`.
   - So a process verifyctl spawned (the test binary) finds no linker and PATH stays unprefixed. devctl and
     splitproof started from a shell still see `ProgramFiles`, and builds under verifyctl still link, so
     this one test is the only visible failure. The plain workspace run inherits the full environment.

## Steps

### Step 1 [core-implementer, sonnet] config: clippy and the DDL template name

(a) `modules/config/src/tests.rs:423`, `modules/config/src/lib.rs` (`SCHEMA_DDL_TEMPLATE` and its doc links).
(b) Independent of the other steps.
(c)
- Write `&mut tx` at tests.rs:423, as clippy suggests.
- Rename the template to a name ending in `_DDL` (e.g. `SCHEMA_TEMPLATE_DDL`).
- Keep it used only inside the `SCHEMA_DDL` LazyLock initialiser, and keep the `store::MAX_REVISIONS` fill.
- Update the doc-comment links that name it.
- No SQL text changes.
Invariant: the SQL sent to Postgres is byte-identical, and `archcheck` reports no `modules/config` violation.
Verify:
- `cargo clippy -p config --all-targets -- -D warnings`
- `cargo run -q -p archcheck`: the config lines are gone.
- `cargo test -p config`
No test is added: this is a rename plus a lint fix, guarded by archcheck and clippy.

### Step 2 [core-implementer, sonnet] inventory: the SQL comment and the two indexes

(a) `modules/inventory/src/lib.rs` (`SCHEMA_DDL` :88-110, `migrate` :208-219).
(b) Independent.
(c)
- Reword the :89-92 SQL comment so no word `characters` is followed by `.`. Keep the meaning.
- Remove both `CREATE INDEX IF NOT EXISTS` statements from `SCHEMA_DDL`. Add them to `migrate` as
  `.index("inventory.character_owners_player_idx", "CREATE INDEX character_owners_player_idx ON
  inventory.character_owners (player_id)")` and `.index("inventory.operator_grants_at_idx", "CREATE INDEX
  operator_grants_at_idx ON inventory.operator_grants (at)")`, after `.sql(SCHEMA_DDL)`.
- Move the index's explanatory comment with it, as a Rust comment. Rule 22 scans comments too, so the
  comment must not quote a locking phrase (e.g. the old index form).
Invariant: index names and columns are unchanged, so an existing database keeps its indexes. The rollout
needs no DROP.
Verify:
- `cargo run -q -p archcheck`: no inventory line.
- `cargo test -p inventory`: the boot migration runs `Ddl::index`, and `Ddl::index` panics on a name/SQL
  mismatch.

### Step 3 [core-implementer, sonnet] processctl: pass ProgramFiles into the build environment

(a) `tools/processctl/src/fleet.rs` `BUILD_ENV_ALLOWLIST`, `tools/processctl/src/fleet_tests.rs`.
(b) Independent.
(c)
- Q1 (owner): recommended, make the VS/SDK lookup read `ProgramFiles` and `ProgramFiles(x86)` from the
  filtered map it is building instead of the process environment. Then any variable the allowlist drops
  breaks the lookup in a plain `cargo test` too, and `sanitized_build_path_contains_the_discovered_msvc_linker`
  becomes the self-check for the next drift. Minimal alternative: only the allowlist line below.
- Add `"ProgramFiles"` to `BUILD_ENV_ALLOWLIST`. Like `ProgramFiles(x86)` it stays out of
  `SERVICE_ENV_ALLOWLIST`.
- Add a unit test modelled on `apple_toolchain_selection_is_preserved_only_in_build_environment`
  (fleet_tests.rs:28): a snapshot holding `ProgramFiles` keeps it in `build_environment()` and not in
  `runtime_environment()`.
Bar: revert the allowlist line and the new test must fail.
Verify: `cargo test -p processctl --lib`. The at-risk path (the verifyctl-sanitised env) is proven by the
plan-end verifyctl run, where `sanitized_build_path_contains_the_discovered_msvc_linker` must pass.

### Step 4 [main session] dependency advisories

(a) `Cargo.lock` only.
(b) Last, so its build is the plan-end build.
(c) `cargo update -p lettre -p url -p cookie`. Check with `git diff Cargo.lock` that only these three crates
(and any of their own deps that the bump requires) changed. The step fails if `aws-lc-rs` or `aws-lc-sys` enters Cargo.lock (rustls must stay ring-only).
Verify: `cargo audit --ignore RUSTSEC-2023-0071 --ignore RUSTSEC-2026-0459`. Only the chrono warning may
remain. Then `cargo build --workspace`.

## Plan-end

`verifyctl --all --strict`. Expected: every stage PASS except `test`, which keeps only the 8 `admindiff --lib`
failures (excluded above). A `cargo test -p mail` run is part of that suite (lettre bump).

## TODO items closed by the steps

Each step commit moves its item to the matching done file:
- Step 1: `docs/todo/config.md`, the archcheck item and the clippy item (the archcheck item at :26 names
  `SCHEMA_DDL_TEMPLATE`).
- Step 2: `docs/todo/inventory.md`, "archcheck fails on inventory's DDL".
- Step 3: `docs/todo/processctl.md`, "The MSVC linker PATH test fails here".

## Not checked

- More clippy errors may hide behind the first one: cargo stopped after it. The plan-end run shows the
  full set; a new one is reported, not fixed in this plan.

- Whether any other module's SQL comment trips rule 15. The plan-end archcheck run answers it.
- The verifyctl `--ignore` for the sqlite advisory is is `SQLITE_ADVISORY = "RUSTSEC-2026-0459"`
  (audit.rs:9, passed at :72).
