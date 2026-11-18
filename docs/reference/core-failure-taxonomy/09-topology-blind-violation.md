# 9. `topology-blind-violation` / `monolith-only-not-proven-on-split` — *rare, highest severity*
A gate/conditional that is correct in the monolith but bypassed in the split, because the
`cmd/*-svc` edge registration and `gateway-svc` route table don't share the monolith's `if`.
- **Recurred:** **`INVENTORY_DEV_GRANT` gated only the monolith op-contribution** — the
  inventory-svc internal edge and gateway-svc routing served the dev-only IAP grant
  UNCONDITIONALLY in split; a dev endpoint live in "production" topology (`25f2163`). Gateway
  `RouteTable` lock-across-await is dormant in monolith (no remote dials) and manifests ONLY
  under split (`5be0d0f`).
- **Attack:** for any env-gated capability, trace the gate through **every** exposure path —
  HTTP op contribution, player-QUIC allow-list, internal mTLS edge registration — not just the
  monolith branch. **These bugs are invisible to a review that reads only `modules/<name>` or
  runs only the monolith.** Prove on the split (routecheck / splitproof), not a monolith unit
  test.
- **Authority:** move the gate INTO the capability method every exposure path traverses
  (`Holdings::grant` itself), and contribute ops unconditionally so monolith and split route
  sets are structurally equal by construction.
