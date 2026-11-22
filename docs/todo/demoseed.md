# Demoseed

Last updated: 2026-11-22

## In progress

_None._

## Open

- [ ] **Monolith logs empty under `devctl up`**
  DEMOSEED-03. `run/devctl/monolith.out.log` / `.err.log` stayed 0 bytes during a full demoseed run
  although the server answered every call; check whether logging is off (`RUST_LOG`) or routed
  elsewhere before any future profiling.
  Source: owner live acceptance (2026-11-01).

### Known limits (demoseed-client-simulator step 5)

- [ ] **Accept-entry cleanup after a demoseeded crawl**
  The former admin-redesign Step 24b4: on a demoseeded monolith, run `admindiff crawl` + `diff`
  and delete only the `data difference` entries in `tools/admindiff/accept/*.txt` the crawl no
  longer produces; rows that vary per run (throwaway guests' deletion receipts, audit rows,
  tombstones) go to the owner, never into the accept files. The commit gets a `proof-auditor`.
  Deferred by the owner 2026-11-22 (the live run's goal was seeding).
- [ ] **Split fleet is not seeded by demoseed**
  The mockup-implementation skill's split crawl seeds only with `eventctl seed-mockup`, so a
  `FirstRow` follower on an empty split table fails the crawl, and once the accept entries above
  are cleaned the split diff shows them as `NEW`. Nothing in demoseed is monolith-specific
  (`--base-url http://127.0.0.1:8082`). Deferred by the owner 2026-11-22.
