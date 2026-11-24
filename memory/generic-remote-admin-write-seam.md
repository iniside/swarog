---
name: generic-remote-admin-write-seam
description: "A module's remote (split-topology) admin writes go through the generic controlplane cp.invoke seam; never fork the portal with a module-coupled bespoke page"
metadata: 
  node_type: memory
  type: project
  originSessionId: db044fb9-aa94-4b76-a698-3f1682463a6e
  modified: 2026-11-24T13:03:38.524Z
---

**Decision (load-bearing): remote admin writes use the GENERIC control-plane seam; do NOT
build a bespoke admin page that imports a module's `<name>api` and forks the portal.**
`core/admin` stays domain-agnostic and renders every module from
`controlplane::PROVIDER_SLOT`.

A write is a manifest action that invokes (`Then::invokes`), sent through
`Provider::invoke`. In the split, admin-svc calls the peer over `cp.invoke`
(`controlplanerpc::remote_factory`), so the write runs in the provider's own process
where its store is local. `cp.invoke` is not `#[retry_safe]`: it is never replayed
after a connection-fatal error. `Manifest::write_refusal` is the one write authority,
applied by the local face and the portal's admission. No new cross-process capability,
no admin-to-module coupling.

**This is the reusable pattern for any remotely-editable admin page: fill the
seam, don't fork.** Pre-redesign rationale (adminapi `AdminSubmit`, `remote_submit`)
is archived in docs/plans/2026-07-14-1421-apikeys-configurator-plan.md.

Related: apikeys secrets are **base64url(sha256(secret))** not hex, CAS-by-`revision`,
roles normalized (keys→roles FK, policy resolved by JOIN). See [[module-reference-pair]],
[[admin-extension-points-shipped]] and [[adversarial-subagent-review]].
