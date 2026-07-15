---
name: live-acceptance-report-dont-fix
description: "During live acceptance runs (Step-7-style), a failure means STOP and report — never fix-and-retry loops; Lukasz decides the fix"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: fb10aade-7f3e-4b87-9d35-e9f2dfc074bf
---

Lukasz's standing directive (2026-07-15, Weles M0 Step 7, reinforced twice): when a
live acceptance/verification run breaks, do NOT enter a fix-retry loop. Stop,
diagnose (reading logs + controlled isolation experiments ARE allowed and wanted),
report symptom → root cause → options, and WAIT for his decision.

**Why:** premature fixing entrenches the wrong layer. Proof from the same day: the
weles linker failure (`link.exe not found` under the filtered build env) had an
obvious 1-line "fix" (add SYSTEMDRIVE/ProgramData to the allowlist) — but the real
defect was design-level: the orchestrator should never build at all
([[mini-orchestrator-native-no-containers]]). Auto-fixing would have shipped a
correctly-working wrong design ("bysmy tu burdel mieli"). Stop-and-report let the
design get corrected instead.

**How to apply:** in any live-run/acceptance phase, on failure: capture logs,
optionally reproduce in isolation to pin the root cause, write the report
(symptom, evidence, resolved paradoxes, options with recommendation), and end the
turn. Fixes only after an explicit go — then through the normal lane + review
cycle. Related: [[adversarial-subagent-review]], [[no-inline-adhoc-fixes]].
