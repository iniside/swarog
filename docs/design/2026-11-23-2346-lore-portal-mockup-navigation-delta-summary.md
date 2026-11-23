# Lore Portal mockup: v3 delta (repository navigation)

Apply to the mockups in `UILayout/portal/` after the v2 merge delta
(`docs/design/2026-11-23-2332-lore-portal-mockup-merge-delta-summary.md`). Change
only what is listed; keep everything else as it is.

- **Repository tabs go to repository-scoped pages.** In `repo-files.html` and the
  other repository pages, the "Merge requests" tab links to
  `merge-requests.html` and "Locks" to `my-locks.html`, which are the
  personal views. Add two repository pages with the same repository header and
  tab bar as Files/History/Branches:
  - **`repo-merge-requests.html`:** all merge requests of `ashfall-game`,
    Open / Merged / Closed tabs, filters for author, reviewer, label and target
    branch, same row layout as the global list. The tab counter is the repo's
    open count.
  - **`repo-locks.html`:** every lock in `ashfall-game`, held by anyone: file,
    holder, branch, since, stale badge; my own rows get "Release lock", others
    "Ask to release"; filters for holder, path and stale. Force unlock stays
    out (read-only fact: "Only a lead can force-unlock it in Swarog Admin").
  The sidebar "Merge requests" entries and "My locks" keep linking to the
  personal views.
- **Folder view.** Clicking a directory row in the file tree must open that
  folder. Add `repo-folder.html` (or make `repo-files.html` show two states):
  - the repository root, with the root README;
  - a subfolder such as `Content/Maps/Frostreach`: breadcrumbs
    `ashfall-game / main / Content / Maps / Frostreach` with each segment
    clickable, a ".." row, the folder's children with last revision, author
    and age, lock icons on locked files, the folder's README only if it has
    one, and "History of this folder" linking to History filtered by path.
  Every directory row on every page links to the folder view; every file row
  links to the file view (text or asset).
