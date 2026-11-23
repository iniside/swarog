---
name: admin-extension-points-shipped
description: "SUPERSEDED: the 2026-07-14 ExtensionPoint/ExtensionEntry admin seam was razed; cross-module admin is now controlplane::Provider + Manifest/ContributionDecl/Point"
metadata:
  node_type: memory
  type: project
  originSessionId: 2d1f848f-2c99-4b3e-a3bb-5c792c0ff30b
  modified: 2026-11-23T23:04:38.001Z
---

The ExtensionPoint/ExtensionEntry admin seam (shipped 2026-07-14, plan
docs/plans/2026-07-14-1851-admin-extension-points-plan.md) no longer exists. It was
removed by b8435c9a "refactor(admin): raze the old admin contract
(admin-control-plane-redesign step 1)"; checked 2026-11-24 (no ExtensionPoint in
core/, modules/ or api/).

The replacement: a module contributes a `controlplane::Provider` (manifest, panel,
search, invoke, prepare) to `PROVIDER_SLOT` (core/controlplane/src/lib.rs). The
manifest is `Manifest { contributions: Vec<ContributionDecl> }` with `Point`
placements (Page, EntityTab, EntityListColumn, RowAction, PanelForm, TableRow...).
In the split, every edge peer answers `cp.*`, and admin-svc reaches peers via its
`peer_face` hand list (cmd/admin-svc/src/lib.rs). The nearest page precedent is
modules/apikeys/src/admin.rs.

Read the code before relying on this note. See [[step13-resume-point]] and
[[follow-uilayout-mockup-faithfully]].
