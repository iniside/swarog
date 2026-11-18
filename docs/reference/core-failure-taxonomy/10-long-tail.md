# 10. Long tail (one or two instances each — still real, still catalog them)
- `retry-semantics-wrong` — classifying reset/replay on a post-mapping value; classify on the
  raw error before erasure (`RPC-RETRY-01`).
- `budget-starvation` / fairness-not-preserved — a correctly-bounded budget whose allocation
  order starves the same tail every pass; replay ≥2 passes and check the starved SET changes
  (`addc824 → 3615b9e`).
- `unsupervised-task-death` — a bare `tokio::spawn` with nothing observing its panic/exit;
  name the mechanism that notices death and where it surfaces (`a9300c`/`a9c7e32`).
- `torn-write-published-early` — a secret/file published non-atomically; temp-then-rename with
  a validated temp (`a83dc79`).
- `sentinel-collision` — two special integer values sharing a space near a saturation boundary
  (`7cf9957`).
- `lost-update-no-concurrency-token` — render-then-submit forms with no optimistic-concurrency
  check; **3 admin modules** (`99025bd`, `4c6bd44`, `79cabed`).
- `missing-ttl-security-cache` — a credential cache that refreshes on miss but never expires a
  rotated-out key (`945e9d0`).
- `security-boundary-missing-binding` — OAuth `state` not bound to the initiating browser
  (`6cc0e7c`); TOCTOU on identity link (`b8c7812`).
- `policy-authority-drift` — the sole `ALTER` in `modules/`, violating idempotent-DDL policy
  (`1970677`).
- `silent-truncation-parse` — `take_while(is_ascii_digit)` silently recording a wrong version
  (`75d4042`).
- `generated-surface-escapes-governance` — codegen emitting a new `pub` item without re-blessing
  public-api; treat any new `pub` from codegen as public-API-affecting by construction
  (`627c33e`).
