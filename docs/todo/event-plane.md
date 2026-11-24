# Event plane

Last updated: 2026-11-24

## In progress

_None._

## Open

- [ ] **Contract golden does not pin enumerated string values**
  `tools/topiccheck/src/golden.rs:444-446` (`flatten_shape`) keeps only `key:kind`, so a rename or
  removal of a `REASON_*` value (the friends and groups reason consts) passes every gate. Fix: one
  typed set per enumerated field (an enum with `as_str()` and an exhaustive `ALL`), from which
  topiccheck emits value lines, so a rename or removal shows as BREAKING.
  Source: proof-auditor review of admin-second-pass step 17a (2026-11-24).
- [ ] **Scheduled prune tests seed rows far from the retention edge**
  The audit, groups, mail and notifications prune tests seed stale rows well past retention
  (e.g. modules/notifications/src/tests.rs:1315 at DEFAULT_RETENTION_DAYS + 5, modules/audit/src/tests.rs:326),
  so a cutoff in the wrong unit (hours for days) or off by days still passes. The inventory grants
  prune test now seeds RETENTION±1 days and pins the constant (modules/inventory/src/tests.rs). Apply
  the same pattern to each sibling, and prove each with an hours-unit revert check.
  Source: proof-auditor review of the inventory grants prune, admin-second-pass step 14 (2026-11-24).
- [ ] **deliver_all call sites in notifications and mail tests**
  EVENT-PLANE-01. `modules/notifications/src/tests.rs` has 16 and
  `modules/mail/src/projection_tests.rs` has 13 `deliver_all()` call sites
  (Grep counts at HEAD); notifications is exposed to foreign emitters in both
  directions, mail to under-count only. The fix hoists `deliver_until_consumed`
  (today private to `modules/wallet/src/tests.rs:1240`) into
  `asyncevents::testing`, which changes a `core/*` public surface and needs
  `--bless-public-api` (docs/reference/event-plane-ops.md:69-80 @ fd797b3a, 2026-07-14).

### Known limits (admin-control-plane-redesign step 24)

- [ ] **Plane verb errors carry eventctl wording into the portal**
  `VerbError`'s `Display` (`core/asyncevents/src/admin.rs:180-206`) prefixes every text with
  "eventctl:", so the admin portal shows "eventctl: no such subscription …". Related: the skip
  path's NotFound says "no subscription" (a second text for one case), and `poison_event` mixes
  the "asyncevents:" and "eventctl:" prefixes. Fix: one neutral wording in the core and the
  CLI prefix added by eventctl itself (review of 8ac46dab).
