---
name: admin-extension-points-shipped
description: "Admin seam = controlplane::Provider in PROVIDER_SLOT, served over the cp.* face; the old ExtensionPoint/adminapi seam is razed"
metadata:
  node_type: memory
  type: project
  originSessionId: 2d1f848f-2c99-4b3e-a3bb-5c792c0ff30b
  modified: 2026-11-24T13:03:38.392Z
---

A module contributes a `controlplane::Provider` (manifest, panel, search, invoke,
prepare) to `controlplane::PROVIDER_SLOT` (core/controlplane/src/lib.rs). The manifest
declares contributions placed at `Point`s. `core/admin` is the portal (core, not a
module) and renders every provider from the slot.

Every edge process serves the slot over the `cp.*` face: `core/app/src/lib.rs` calls
`controlplanerpc::register_face` unconditionally (an empty manifest list when the process
hosts no provider). admin-svc reaches peers with `controlplanerpc::remote_factory(peer)`
in its `peer_face` hand list (cmd/admin-svc/src/lib.rs). Nearest page precedent:
modules/apikeys/src/admin.rs.

The old ExtensionPoint/ExtensionEntry seam (2026-07-14) and `adminapi::SLOT` no longer
exist (razed in b8435c9a). Read the code before relying on this note. See
[[step13-resume-point]], [[generic-remote-admin-write-seam]] and
[[follow-uilayout-mockup-faithfully]].
