# Lore

Last updated: 2026-11-24

## In progress

_None._

## Open

### Known limits (admin-second-pass step 5a)

- [ ] **Audit of Lore operator actions**
  Not built: no audit topic for Lore actions and no repository audit tab.
  The mockup's repository Audit tab was removed on 2026-11-24 because no code
  declares an `entity.repository.tab` audit contribution.

### Deferred

- [ ] **Merge button in the portal (Swarog-side merge worker)**
  Owner decision 2026-11-23: merge requests are merged from the Lore client for
  now and the portal is review-only. Revisit if Lore does not ship server-side
  merge. Recipe: a worker linking `lore-revision` (`diff3_collect` against the
  real merge base, `merge_tree_changes`, upload, `BranchPush` with
  `fast_forward_merge`, no `force`); also enables squash. Must check locks
  itself. Do not use a bare `BranchPush` of A's head with
  `fast_forward_merge=true`: it merges only A's last revision. Open questions
  and costs: [server-side merge research](../research/2026-11-23-2325-lore-server-side-merge-summary.md).
- [ ] **Upstream server-side merge to Lore**
  Third option: add a merge RPC (or fix the merge base in
  `try_fast_forward_merge`) in Lore and send a PR upstream, instead of the
  Swarog worker. Stock `loreserver` stays unmodified locally either way.

- [ ] **Binary asset previews in the portal**
  Owner decision 2026-11-23: previews of maps, textures and meshes (the
  `AssetCard` thumbnails and the before/after previews in revision and merge
  request diffs) come in the second or third portal iteration. The first
  iteration shows binary metadata only: size, type, lock state, history and
  size changes. The mockups already show the preview design.

## Notes

- Lore's roadmap promises a review web client for 2027 (`docs/roadmap.md:81-85`
  in the Lore repo); check each Lore release for server-side merge before
  starting either deferred item.
