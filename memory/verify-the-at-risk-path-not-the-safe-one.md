---
name: verify-the-at-risk-path-not-the-safe-one
description: "Verify the deployment/path a change actually affects (split, not just monolith); a committed repeatable proof, never eyeball"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: e2474d37-b06f-41bb-a2ab-76e4e9659478
---

When verifying a change, exercise the path the change ACTUALLY puts at risk — not the
easiest path — and make the proof a committed, repeatable artifact.

**Why:** On the config module (2026-07-07) I made config monolith-only via a soft
require, then "verified" by driving the MONOLITH — the one topology that was never at
risk — and passed it off as done. The user called it "cheatowanie". The whole point of
the fix lived in the SPLIT (microservices), and it was broken there. Testing the safe
path and presenting it as coverage of the hard path is a lie, even if each individual
claim is true.

**How to apply:**
- Ask "what does this change put at risk, and in which deployment/topology?" This repo
  has two: monolith (`cmd/server`) and split (`cmd/*-svc`, run via `run.sh microservices`).
  A change touching cross-service/foundation behavior MUST be driven in the split.
- Prefer a committed script that fails loud (exit!=0) over a one-time manual drive —
  e.g. `scripts/smoke-split-config.sh`. Capture output into a `docs/…-verified.md`.
- Don't conflate "no value set for a key" with "the service isn't deployed" — a missing
  dependency should fail loud ([[work-on-master-no-branches]] repo prefers hard-require),
  not silently degrade.
- If a design doc claims a property (e.g. "edit via psql propagates"), verify THAT
  property empirically or don't claim it. See the config split fix + DB-trigger NOTIFY.
