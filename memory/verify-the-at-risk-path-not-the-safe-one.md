---
name: verify-the-at-risk-path-not-the-safe-one
description: "Verify the topology a change actually puts at risk (split, not just monolith), with a committed repeatable proof — testing the safe path and passing it as coverage is a lie"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 88cdd953-b406-40a0-8ab2-6c7eb07acece
---

Exercise the path the change ACTUALLY puts at risk, not the easiest one, and make the proof
a committed repeatable artifact. This is CLAUDE.md Fix-the-Authority rule #5 ("prove the
failing branch… on the topology at risk (split, not just monolith)") — the incident below
is why it exists.

**Why:** on the config module (2026-07-07) I made it monolith-only via a soft require, then
"verified" by driving the MONOLITH — the one topology never at risk — and passed it as done.
User: "cheatowanie". The whole fix lived in the SPLIT and was broken there. Testing the safe
path and presenting it as coverage of the hard path is a lie even if each claim is true.

**Two nuances NOT in CLAUDE.md:**
- Prefer a committed named assertion in `tools/splitproof` (fails loud) over a one-time
  manual drive; run it inside the one selected `verifyctl` manifest, don't add a second fleet
  supervisor.
- Don't conflate "no value set for a key" with "the service isn't deployed" — a missing
  dependency should fail loud, not silently degrade. If a design doc claims a property ("edit
  via psql propagates"), verify THAT property empirically or don't claim it.

Related: [[scope-claims-to-what-was-verified]].
