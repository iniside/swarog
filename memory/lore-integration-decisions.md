---
name: lore-integration-decisions
description: "Owner decisions (2026-11-23) for Lore support — Swarog as OIDC IdP, Tier 2, stock loreserver, in-repo zero-sharing portal, review state in Swarog tables"
metadata:
  node_type: memory
  type: project
  originSessionId: 8214c2ca-dbe5-496f-9b45-71068b5bf051
  modified: 2026-11-23T22:20:54.007Z
---

Owner decisions for the Lore extension, 2026-11-23 (October P1–P5 still stand: wipe, stock loreserver + Subscribe bridged to lore.* topics, one studio = one deployment, per-repo permissions in the lore module edited in admin, provider-independent device approval page):
- Lore server is never modified; track the latest stable tag (v0.14.0 at decision time). No Weles/installer integration: the operator installs loreserver, Swarog only knows its address.
- Auth is **Tier 2** (ResourceGrants, per-repo urc-{id} via token exchange). Swarog is a full OIDC provider + lore.user.v1 at user_url; no UrcAuthApi adapter.
- The portal (browse + review + comments + merge requests) lives in this repo as a separate zero-sharing project like weles; mockup brief in docs/design/2026-11-23-2205-lore-portal-mockup-brief-summary.md.
- Review/comment/MR state lives in Swarog module tables (not Lore revision metadata as the source of truth).
- Merge: preferred Swarm/GitHub style (MR waits, "Merge" clicked in the portal). If server-side merge is too much work, merge is done from the client and the portal is review-only. Under research as of 2026-11-23.

**Why:** owner direction in the Lore design session; research in docs/research/2026-11-23-2308-lore-v0-14-integration-summary.md.
**How to apply:** don't re-propose Tier 1, Lore patches, a UrcAuthApi adapter, or a portal module inside Swarog. Related: [[never-monolith-only-features]], [[shared-postgres-is-the-model]].
