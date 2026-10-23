# Event plane

Last updated: 2026-10-23

## In progress

_None._

## Open

- [ ] **deliver_all call sites in notifications and mail tests**
  EVENT-PLANE-01. `modules/notifications/src/tests.rs` has 16 and
  `modules/mail/src/projection_tests.rs` has 13 `deliver_all()` call sites
  (Grep counts at HEAD); notifications is exposed to foreign emitters in both
  directions, mail to under-count only. The fix hoists `deliver_until_consumed`
  (today private to `modules/wallet/src/tests.rs:1240`) into
  `asyncevents::testing`, which changes a `core/*` public surface and needs
  `--bless-public-api` (docs/reference/event-plane-ops.md:69-80 @ fd797b3a, 2026-07-14).
