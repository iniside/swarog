---
name: config-as-code-anti-magic
description: "Lukasz dislikes config-file magic — prefer config-as-code (typed Rust, like cmd/* mains); any data-format config must be ascetic (strict, no layering, no templating)"
metadata: 
  node_type: memory
  type: user
  originSessionId: fb10aade-7f3e-4b87-9d35-e9f2dfc074bf
---

Lukasz has never liked configuration files — "zawsze to byl jakis magic, ktory nie
mial sensu" (2026-07-10). The repo already votes the same way: topology lives in
`cmd/*` mains as typed Rust, not yaml.

**Why:** the "magic" is a specific sin list — layered overrides (env>file>profile),
stringly-typed keys with silently-ignored typos, file-shows-delta-not-truth,
templating-on-yaml, unclear live-vs-restart reload semantics.

**How to apply:** default to config-as-code (Rust, compiler-checked — recompile is
cheap with agents; leaning this way for the orchestrator manifest,
[[mini-orchestrator-native-no-containers]]). If a data file is genuinely needed:
one file, `serde(deny_unknown_fields)`, no layering/profiles/env-overrides, at most
one documented placeholder syntax, and a dry-run command that prints the fully
resolved result. Never propose Helm-style templating or multi-source config
precedence chains.
