---
name: lore-integration-decisions
description: "Owner decisions (2026-11-23) for Lore support — Swarog as OIDC IdP, Tier 2, stock loreserver, in-repo zero-sharing portal, review state in Swarog tables"
metadata:
  node_type: memory
  type: project
  originSessionId: 8214c2ca-dbe5-496f-9b45-71068b5bf051
  modified: 2026-11-23T23:27:26.964Z
---

Owner decisions for the Lore extension, 2026-11-23/24. October P1–P5 (docs/research/2026-10-22-1539-…) are RESEARCH, not decisions; the owner corrected this 2026-11-24 ("to nie byla decyzja tylko resreach"), so re-check them, don't cite them as closed:
- Lore server is never modified; track the latest stable tag (v0.14.0 at decision time). No Weles/installer integration: the operator installs loreserver, Swarog only knows its address.
- Auth is **Tier 2** (ResourceGrants, per-repo urc-{id} via token exchange). Swarog is a full OIDC provider + lore.user.v1 at user_url; no UrcAuthApi adapter.
- The portal (browse + review + comments + merge requests) lives in this repo as a separate zero-sharing project like weles; mockup brief in docs/design/2026-11-23-2205-lore-portal-mockup-brief-summary.md.
- Review/comment/MR state lives in Swarog module tables (not Lore revision metadata as the source of truth).
- RBAC is a separate module/service `authz` (2026-11-24). Modules own their action catalogs; authz owns roles (operator data) and grants (global + per resource) and answers actions(user, kind, resource); modules never see role names. The contract stays in api/authz; it moves to core/ only if core/admin operators adopt it. accounts (token exchange), lore and review consume authzapi.
- Merge (decided 2026-11-23): done from the Lore client; the portal is review-only and detects merges from pushes. A Swarog-side merge worker is a workaround for something Lore will likely ship, so it is deferred, as is a third option of an upstream Lore PR (docs/todo/lore.md ### Deferred).

**Why:** owner direction in the Lore design session; research in docs/research/2026-11-23-2308-lore-v0-14-integration-summary.md.
**How to apply:** don't re-propose Tier 1, Lore patches, a UrcAuthApi adapter, or a portal module inside Swarog. Related: [[never-monolith-only-features]], [[shared-postgres-is-the-model]].
