# Authz

Last updated: 2026-11-28

## In progress

_None._

## Open

### Deferred

- [ ] **Durable events for grant and role changes**
  The first authz plan ships no `api/authz/events` crate and no topics. When a consumer needs to react to a
  grant or role change (an audit trail, a cache in another module), add the topics with a durable
  subscriber (`ALLOW_UNSUBSCRIBED` is empty), the audit sink and the contract-golden entries.
  Source: owner answer 4 on the authz research (2026-11-28); roadmap
  docs/plans/2026-11-24-0004-lore-integration-roadmap-plan.md Step 5a lists the events crate.
