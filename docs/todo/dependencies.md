# Dependencies

Last updated: 2026-11-27

## In progress

_None._

## Open

- [ ] **New RustSec advisories fail the audit stage**
  `cargo audit` reports 8 vulnerabilities and 1 warning:
  - cookie 0.18.1, RUSTSEC-2026-0594 (fix: >=0.18.3);
  - lettre 0.11.23, RUSTSEC-2026-0560 to -0565 (fix: >=0.11.24);
  - url 2.5.8, RUSTSEC-2026-0592 (fix: >=2.5.9);
  - chrono 0.4.45, RUSTSEC-2026-0543, unmaintained, no fixed upgrade (warning only).
  Source: verifyctl run verify-1795776086-65428-62521403, 2026-11-27.

### Known limits (security-quick-wins step 1b)

- [ ] **Drop the RUSTSEC-2026-0459 ignore by upgrading sqlx**
  `tools/verifyctl/src/stages/audit.rs` ignores RUSTSEC-2026-0459 (libsqlite3-sys
  0.30.1, in `Cargo.lock` only through sqlx's weak `sqlx-sqlite?/…` entries; only
  `postgres` is enabled, so sqlite is never compiled). The fix needs libsqlite3-sys
  >=0.39. The workspace is on sqlx 0.8.6, whose sqlx-sqlite requires `^0.30.1`; the latest
  release, 0.9.0, still caps it at `>=0.30.1, <0.38.0` (crates.io dependency list of
  sqlx-sqlite 0.9.0, 2026-11-17). Once a released sqlx allows >=0.39, upgrade sqlx and
  remove the ignore; until then the `audit` stage's premise guard
  (`sqlite_premise_finding`) fails if libsqlite3-sys is compiled or leaves the lock.
  The upgrade costs about 77 call sites across about 29 files (sqlx 0.9
  `SqlSafeStr`, launchbadge/sqlx#3723) plus the archcheck rule-22 `raw_sql` pattern,
  and all three sqlx pins move together: the root `Cargo.toml`, `weles/Cargo.toml` and
  `tools/processctl/Cargo.toml` (each is `version = "0.8"`). The 77/29 figure is from
  the 2026-11-13 research and was not re-counted. This upgrade needs its own plan.
