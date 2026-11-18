# 1. `error-folded-into-success` — *the most recurrent class; every slice had it*
A real failure (an error, a serialize failure, a cleanup failure, a name collision, a
truncated parse) is absorbed into an apparently-successful result: `Ok(())`, `Value::Null`,
a green diff, `FleetStatus::Stopped`, a logged-but-not-propagated counter.
- **Recurred:** retention sweep returning `Ok` while a topic persistently failed
  (`71e3a6a`, sibling of the landmark `7ca0b51`); rpc-macro `to_value(..).unwrap_or(Null)`
  (`56ce6fb`); csharp-gen last-writer-wins DTO collision (`982ec8d`); guardian cleanup
  errors discarded (`192ffda`); devctl teardown reporting Stopped despite cleanup failures
  (`18c9076`, whole 6-commit chain); splitproof `.ok().flatten()` swallowing SQL error as
  zero-rows (`20fcf9c`); conformance `KnownGap` conflated with `Fail` — the *inverse*
  polarity, a tracked non-blocker diluting the stop-the-line signal (`0d5ff32`);
  invalidation/asyncevents treating explicit `0` as absent (`e5c1b7f`).
- **Attack:** trace every error/`.ok()`/`.unwrap_or`/`let _ =`/`take_while` to its TERMINAL
  caller and confirm the caller's control flow (readyz stamp, exit code, golden diff, the
  status the operator trusts) actually *observes* the failure. "It's logged / there's a
  metric" is NOT evidence the failure is surfaced.
- **Authority:** the return-value contract of the function the caller trusts (`sweep()` now
  `bail!`s; `teardown_with` decides status from a `cleanup_failures` vec; the codegen
  template emits `Internal` on serialize failure).
