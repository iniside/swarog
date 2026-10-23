# Party

Last updated: 2026-10-23

## In progress

_None._

## Open

- [ ] **Socket-driven disconnect detection**
  PARTY-01. A disconnect is noticed by lease expiry, typically the lease plus
  one 5s sweep tick, not by socket close; socket-driven leases are the follow-up
  (docs/roadmap/feature-tracker.md:204, 2026-10-05;
  docs/plans/2026-09-28-2212-push-multiplex-invoke-and-party-plan.md:679).
- [ ] **devctl override for PARTY_LEASE_SECS**
  PARTY-02. Only the proof fleet sets it (`PROOF_PARTY_LEASE_SECS` in
  `tools/processctl/src/fleet.rs:126,962,1025`); Grep over `tools/devctl` finds
  no party env (docs/plans/2026-09-28-2212-push-multiplex-invoke-and-party-plan.md:618,
  2026-10-05).
- [ ] **Purge residue window leaves a player_revisions row**
  PARTY-03. An op verified before the session revoke can re-insert a member row
  after the purge ran; the lease sweep removes the member row later but nothing
  deletes that player's `party.player_revisions` row. Taken from the plan
  (docs/plans/2026-09-28-2212-push-multiplex-invoke-and-party-plan.md:683,
  2026-10-05); the race itself was not reproduced at HEAD, only the purge path
  that deletes the row (`modules/party/src/store.rs:593`) was read.
- [ ] **Sweep COMMIT outside its statement_timeout**
  PARTY-04. `modules/party/src/sweep.rs:34-42` counts the transaction's commit
  as one of `PARTY_TX_STATEMENTS_MAX` statements, each assumed bounded by the
  2s `SET LOCAL statement_timeout`; a commit is not a statement that timeout
  covers (docs/plans/2026-09-28-2212-push-multiplex-invoke-and-party-plan.md:686,
  2026-10-05).
- [ ] **Conformance proof for the invite outage path**
  PARTY-05. Conformance drives `resolve_target` only
  (`modules/party/src/conformance.rs:81`); the outage rules are covered by the
  unit test `invite_target_rules` (`service_tests.rs:91`)
  (docs/plans/2026-09-28-2212-push-multiplex-invoke-and-party-plan.md:669,
  2026-10-05).
