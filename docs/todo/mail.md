# Mail

Last updated: 2026-11-22

## In progress

_None._

## Open

- [ ] **Requeue-all-parked has no caller**
  `Service::requeue_all_parked` (modules/mail/src/service.rs) and
  `Store::requeue_all_parked_tx` (modules/mail/src/store.rs) are the bounded recovery path
  for a mass-park (docs/plans/2026-08-31-1650-mail-outbound-channel-3b-plan.md:914), but no
  page, op or test calls them since b8435c9a razed the old admin page; Step 11's G11 (a)
  (docs/plans/2026-11-17-step11-mail-checklist.md:375) left them off the portal and ab071c25
  marks them `#[expect(dead_code)]`. Decide whether to reach them (an action with an owner
  approval, or an ops command) or delete them. Owner 2026-11-22: decide later.
- [ ] **Mail projection tests hang**
  MAIL-04. `cargo test -p mail -- --test-threads=1` never finishes inside
  `projection_tests`: `a_faulting_handler_is_uncounted_and_backed_off` hangs, and a run
  skipping only that test still hung in the same group (not narrowed further). Seen
  2026-11-18 during admin-control-plane-redesign Step 13c.1a, whose diff touches only
  `modules/mail/src/admin.rs`; suspected to be the durable-plane test deadlock, not that
  step. Closed by: finding the hanging test and its wait, then a fix at the source.
  Source: 0c380299 hand-off.
- [ ] **In-tree producer for `mail.send_requested`**
  MAIL-03. Only mail consumes `SEND_REQUESTED` (Grep of modules/, lower bound). The
  intended producer is accounts. Source: docs/roadmap/feature-tracker.md:246 @ fd797b3a
  (2026-09-05).
