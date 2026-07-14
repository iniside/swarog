---
name: admin-extension-points-shipped
description: "Admin portal has cross-module extension points + GameOps restyle (shipped 2026-07-14) — the seam model, where things live, how to extend"
metadata: 
  node_type: memory
  type: project
  originSessionId: 2d1f848f-2c99-4b3e-a3bb-5c792c0ff30b
---

Admin extension points + mockup restyle SHIPPED 2026-07-14 (commits acbb476..9034c00
on master; plan docs/plans/2026-07-14-1851-admin-extension-points-plan.md, research
docs/research/2026-07-14-1804-*.md). Full verify --all --strict green same day.

**The seam (Unreal-UToolMenus-inspired, three parties, bus-triangle analog):**
- Page OWNER declares typed `ExtensionPoint` consts in its own api crate
  (`accountsapi::admin::PLAYERS_ROW_MENU`, `charactersapi::admin::{CHARACTERS_CARD_MENU,
  CHARACTER_MODAL_ACTIONS}`) — id ("accounts.players.row-menu"), kind
  (EntityMenu|ModalActions), context_keys (["id"]); tags its Table/CardGrid with
  `menu_point` + per-row/card/Content `context` ({"id":"player:<uuid>"} — entity-ref
  convention is inventory's `<type>:<uuid>`).
- CONTRIBUTOR ships pure-data `ExtensionEntry` (point, label, icon key, link template
  "inventory?owner={id}", Present::Navigate|Modal, priority) on BOTH local Item
  (`with_extensions`) and remote `ItemData.extensions` via ONE shared
  `extension_entries()` fn so topologies can't drift. Contributions ride the existing
  `admin.adminData` wire — contrib slots never cross processes.
- PORTAL (modules/admin) merges per request: natives → separator → extensions
  (normalized order, user decision), uniform `{key}` interpolation, auto-appends
  `from=<page>` (back-chip is admin chrome, slug-validated), Modal entries render as
  htmx `hx-get ...&partial=modal` into `#modal-root` with full-page fallback when
  `HX-Request` absent and `HX-Redirect` on expired session.

**Key contract changes:** `AdminData::admin_data(params: Params)` (params now cross
the wire on GET — enables scoped remote sub-pages) with FOREIGN-PARAMS TOLERANCE:
impls must never Err on unknown params (portal forwards every page's params to every
provider). Any adminapi change ⇒ `verifyctl -- --bless-public-api` (+contract-golden).

**Guards:** `cargo run -p admincheck` (topiccheck-shaped, advisory verify stage) —
entries target declared points, kind/present compat, {keys} ⊆ context_keys; known
gap: menu_point/modal_point live in render output, covered by split-proof
[ADX1-3b] instead. Extend split-proof with a named ADX when adding a point.

**Run:** `cargo run -p devctl -- up monolith` → http://localhost:8080/admin,
login admin/admin (devctl seeds via adminctl; ADMIN_USER/PASS no longer exist).
Split: `up split` → gateway :8082/admin.

Inert Edit/Delete menu entries + op-backed mutating actions = deliberate later
phase. See [[follow-uilayout-mockup-faithfully]] for the restyle workflow and
[[timing-sensitive-tests-doctrine]] for the test campaign the rollout triggered.
