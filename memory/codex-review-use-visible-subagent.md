---
name: codex-review-use-visible-subagent
description: "run codex adversarial-review as the VISIBLE codex:codex-rescue Agent subagent with --wait, not the background companion CLI (which hangs/zombies on this Windows box)"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 0f48d012-5fb7-49b7-88ce-41f5ec2fe219
---

For per-commit codex adversarial reviews, invoke the **visible `codex:codex-rescue` Agent
subagent** (`Agent` tool, `run_in_background:false`, prompt starting `--wait --fresh --effort
high`). Lukasz explicitly prefers this — it shows up in the console as a running subagent.

**Why:** the raw `codex-companion.mjs` background job path (`adversarial-review --background` /
`task --background` + polling) repeatedly **hung** and left **zombie "running" entries**
(the companion doesn't reap dead PIDs; `status <id>` doesn't find still-running jobs by id,
`result <id>` never returns for a killed job, and `cancel` breaks on Git-Bash `/PID` path
mangling). Two ~30-min hangs before switching. The visible `--wait` subagent path returned
clean every time (~5-8 min at high effort).

**How to apply:** `Agent(subagent_type:"codex:codex-rescue", model:"sonnet",
run_in_background:false, prompt:"--wait --fresh --effort high\n\n<review task>")`. The
subagent is a thin forwarder; its final message IS codex's output. Don't poll the companion
CLI. Ties to [[adversarial-subagent-review]] (codex = the independent-reviewer boundary,
per-commit).
