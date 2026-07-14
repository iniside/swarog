---
name: generic-remote-admin-write-seam
description: "How a module gets a remotely-editable (split-topology) admin page — enrich the generic adminapi seam, never fork the portal with a module-coupled bespoke page"
metadata: 
  node_type: memory
  type: project
  originSessionId: db044fb9-aa94-4b76-a698-3f1682463a6e
---

Before 2026-07-14 the admin portal was read-only over the edge: `adminapi::Form.submit`
is `#[serde(skip)]` and remote `admin.adminData` render forced `form: None`, so a module
hosted in a split process (its own `*-svc`) could only DISPLAY its admin page, not accept
writes. The apikeys configurator rollout needed remote-editable admin pages with rich
widgets (role dropdown, method checkboxes, show-once secret).

**Decision (load-bearing): enrich the GENERIC `adminapi` seam; do NOT build a bespoke admin
page that imports the module's `<name>api` and forks the portal's nav/routing.** The admin
module stays domain-agnostic (imports NO `<name>api`, renders every module from
`adminapi::SLOT`). The seam now carries:
- typed `Field` (`FieldKind::{Text,Select,CheckboxGroup}` + `FieldOption`),
- `SubmitOutcome{reveal: Vec<RevealItem>}` (the `SubmitFn`/submit Ok-type; `reveal` = show-once
  values rendered via PRG-with-flash so a POST refresh can't double-submit),
- an **opt-in** `#[rpc(prefix="admin")] AdminSubmit::admin_submit(id, params)` trait a provider
  implements to accept writes; a provider without it → edge `UnknownMethod`→`NotFound` → admin
  degrades to read-only (graceful),
- a **per-provider** `Item.remote_submit` closure (parallel to `remote_fetch`) that routes a
  write to the CORRECT peer. NOT a single `dyn AdminSubmit` registry capability — that key
  collides/misroutes with >1 provider (caught in review; see [[adversarial-subagent-review]]).

So: the provider's own submit closure runs server-side in ITS process (where its store is
local); admin-svc dials it over the edge. No new cross-process capability, no admin→module
coupling. **This is the reusable pattern for ANY future remotely-editable admin page** — fill
the seam, don't fork. admin emits `admin.action{form-submit}` uniformly (local+remote) so
remote writes are audited without per-provider work.

Rollout plan + full step detail: `docs/plans/2026-07-14-1421-apikeys-configurator-plan.md`.
Related: apikeys secrets are **base64url(sha256(secret))** not hex, CAS-by-`revision`, roles
normalized (keys→roles FK, policy resolved by JOIN). Ops-catalog checkbox source is a
build-time generated `opscatalog::OPERATIONS` (from `route_bindings()` `#[http]` ops),
freshness-gated in verifyctl `codegen-freshness`. See [[module-reference-pair]].
