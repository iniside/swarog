# Core remote

Last updated: 2026-10-23

## In progress

_None._

## Open

- [ ] **Hostname support in EdgeDialer**
  CORE-REMOTE-01. `EdgeDialer::dial` parses the resolved peer as a
  `SocketAddr` (`core/remote/src/lib.rs:435`), so a hostname or DNS answer fails
  with "bad peer edge addr". Needed once `resolve` returns real hosts
  (docs/reference/weles-design.md:113, 2026-07-21).
- [ ] **Stopped fence on Pool::stop and Stub::stop**
  CORE-REMOTE-02. Neither sets a `stopped` flag (Grep of `core/remote/src/lib.rs`
  finds none), so a dispatch after stop would re-resolve and mint fresh
  instances with no owner to stop them. Safe today only through `app::run`
  teardown order (fronts shut down before any module `stop`)
  (`modules/gateway/src/lib.rs:259-267`, 2026-07-28).
