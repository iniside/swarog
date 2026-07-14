---
name: amplification-proof-belongs-in-unit-tests
description: "prove no-I/O / anti-amplification fixes in SEQUENTIAL unit tests, not split-proof latency — a killed peer's port fast-fails so behavioral timing can't distinguish fixed/unfixed"
metadata: 
  node_type: memory
  type: project
  originSessionId: 0f48d012-5fb7-49b7-88ce-41f5ec2fe219
---

When fixing a "work-per-request" / amplification / zero-I/O defect (e.g. the 2026-07-13
`/readyz` background-probe fix: `remote::Stub` per-request QUIC dial → cached verdict),
the airtight proof is a **sequential unit test** exercising the decision path with the
work-source seeded/mocked (a pure fn like `readiness_verdict(cached,last_probe_at,now)`
proves zero-I/O BY CONSTRUCTION). Do NOT try to prove it behaviorally on the split.

**Why (bit twice this rollout):**
- A concurrent burst of `/readyz` completes in ~1s wall-clock even against the UNFIXED
  per-request-dial code, because axum overlaps handlers → latency-of-burst proves nothing.
- A single-request latency threshold ("<500ms while peer down") is ALSO unsound: a killed
  svc leaves a CLOSED loopback port, and a QUIC dial to it **fast-fails** (cf.
  `probe_unreachable_peer_errs_fast`) instead of consuming its 1s timeout — so even the old
  in-request-dial path is fast, and the threshold false-passes the unfixed AND false-fails
  the fixed under CI jitter.
- Same failure class as a hollow `<100ms vs ~100s` timing assert against `127.0.0.1:1` — a
  closed port never gives the slow path the timing test assumes.

**How to apply:** amplification/zero-I/O proof → sequential unit test + a pure decision fn
(zero-I/O by construction). The split-proof scenario is honestly scoped to **accuracy +
recovery** (dead peer flips readyz 503 naming the stub; recovers to 200), NOT to
distinguishing fixed/unfixed. This is the [[fix-the-authority-not-the-symptom]] "prove the
failing branch" rule specialized: pick a proof medium the branch can actually be pinned in.
See [[verify-the-at-risk-path-not-the-safe-one]] — the split still matters for accuracy, just
not for the amplification claim.
