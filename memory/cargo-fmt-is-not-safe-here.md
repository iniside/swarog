---
name: cargo-fmt-is-not-safe-here
description: "Never run `cargo fmt` in GameBackend — no rustfmt.toml, the tree is not rustfmt-default-clean, it churns ~19 untouched files"
metadata: 
  node_type: memory
  type: reference
  originSessionId: 3158fbae-cb42-4982-9305-c2dac1161a5c
---

**Do not run `cargo fmt` in this repo, on any scope.** There is no `rustfmt.toml`,
and the tree is NOT rustfmt-default-clean. Discovered 2026-07-17: `cargo fmt -p
gateway-svc -p weles` reformatted **19 files the agent never touched**. The agent
had to revert all of it (restore to HEAD, re-apply its own edits by hand) and
hand-format to house style — pure waste, plus a real risk of an unrelated-churn
commit if it hadn't been caught.

Format by hand, matching the surrounding code. `verifyctl` does not run a format
check, so nothing forces the default style; the house style is what is already in
the file.

**How to apply:** paste this into any code-writing subagent prompt for this repo —
subagents reach for `cargo fmt` by reflex at the end of a task. If the repo ever
wants formatting enforced, that is a deliberate one-time rollout (add
`rustfmt.toml`, format everything, add a verify stage), not a side effect of one
task. Related: [[historical-docs-are-archives]].
