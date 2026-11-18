# 2. `unbounded-operation` — *dominant in transport + domain; ties #1 overall*
A peer-controlled or resource-consuming operation has no bound of its own — missing timeout,
missing capacity cap, missing grace, or a bound that covers the wrong phase.
- **Recurred:** `axum::serve` detached per-connection tasks surviving teardown (`5943eed`);
  unbounded IP-limiter visitor table (`bdbd325`); plane DDL advisory lock with no
  `lock_timeout` (`23a8aee`); player-QUIC stream read/write unbounded — the twin the internal
  plane already fixed (`ac87caa`); `Stub::start` boot hook awaited unbounded, stalling ALL
  later module starts (`6d505e2`); devctl control protocol with no frame/time bound
  (`049a6f5`); **gateway credential admission bounded only at dial, not the RPC round-trip**
  (`cec0d73`); gateway `RouteTable` Mutex held across `dial` serializing all providers
  (`5be0d0f`); scheduler wedge on a pooled connection (`3f35d4a/addc824`); inventory quantity
  overflow inside the durable delivery tx (`5bfe36b`); unbounded session-token / match
  inputs (`384f5f0/a035fd2`, `aefcb1f`).
- **Attack:** for every new I/O call or lock acquisition ask "**what specifically bounds
  this, and does the bound cover the actual hang point?**" A *dial* timeout is not an *RPC*
  timeout. A *connection* idle timeout does not bound a *stream*. A cache mutex must never
  span an `.await`. A future holding a session-scoped DB lock must never be wrapped in a
  cancelling timeout (bound at the DB layer or use a dedicated connection whose drop closes
  the session).
- **Authority:** one seam wrapping the WHOLE operation (`FrontDoor::admit()`,
  `PLAYER_STREAM_GRACE`, `Stub::start_with_boot_timeout`) — plus the sibling/twin plane.
