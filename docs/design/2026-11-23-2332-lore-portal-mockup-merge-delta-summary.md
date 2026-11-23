# Lore Portal mockup: v2 delta (client-side merge)

Apply to the existing mockups in `UILayout/portal/` (commit 4faccfbb, built from
v1 of `docs/design/2026-11-23-2205-lore-portal-mockup-brief-summary.md`). Change
only what is listed; keep everything else as it is. The brief itself is already
at v2.

## What changed and why

The portal no longer merges. Lore has no general server-side merge, so a merge
request is merged with the Lore client and pushed; the portal notices the push to
the target branch and marks the request merged. Consequences:

- Nothing in the portal says "Merge", "Merge into main", "Squash" or "Delete
  source branch".
- Approvals and locks no longer block a button. They are the review outcome the
  author and reviewers act on; the box says whether the request is ready to merge
  and how to do it.
- Approval requirements are a studio convention shown by the portal, not
  something Lore enforces. Write "2 approvals expected", not "required by the
  main branch rule".

## Changes per file

- **`merge-box-states.html`**
  - Subtitle: drop "merge right" from the check order; the checks are
    conflicts, approvals, changes requested, locks held by others.
  - **Ready:** title "Ready to merge"; detail "Merge it with the Lore client."
    Below the checks, a mono command block with a "Copy merge commands" button:
    `lore branch switch main`, `lore sync`,
    `lore branch merge feature/frostreach-lighting --message "Merge !42"`,
    `lore push`. No merge button, no merge options.
  - **Approvals missing / changes requested:** same checks, no command block;
    detail says what is still expected ("1 of 2 approvals", "diego.r requested
    changes").
  - **Conflicts:** list the files, then the commands to bring `main` into the
    source branch and push it again: `lore branch switch feature/…`,
    `lore branch merge main`, resolve, `lore commit "Merge main"`,
    `lore push`.
  - **Locked by others:** a warning, not a block: name the files and owners,
    keep "Ask to release", and say the merge push may wait on the lock.
  - **Protected target:** replace with "You cannot push to `main`": the
    reader lacks the push-protected right; someone with it merges. Show who
    holds the right only if the data has it; otherwise just the fact.
  - **Merged:** "Merged by sam.o in `rev:…`, detected from the push to
    `main`", with a link to the revision.
  - **Closed:** unchanged.
- **`merge-request.html`**: the MergeBox at the bottom of Conversation uses
  the new "Locked by others" warning variant with the command block visible but
  marked "Waiting for 1 more approval" above it; sidebar "2 approvals required
  by the main branch rule" becomes "2 approvals expected".
- **`merge-request-files.html`**: the lock banner on `Frostreach_P.umap` says
  "Locked by diego.r · lock:L-2291 · 6 h. Ask before merging." instead of
  "Merge waits until it is released."
- **`new-merge-request.html`**: the lock note ends "Ask them to release it
  before you merge." instead of "it cannot merge until the lock is released."
- **`my-locks.html`**: second tab, "Blocks the merge" becomes "Needed for the
  merge"; "Touched, not blocking yet" becomes "Touched".
- **`merge-requests.html`**: Merged tab keeps "Merged by" and "Merge revision";
  add nothing.
- **`README.md`**: note in the MergeBox row that the portal shows merge status
  and commands and never merges.

## Navigation fixes from owner review

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

## Also fix from v1

v1 asked for new primitives in `UILayout/design-system/components/` first
(`FileTree`, `CodeView`, `DiffView`, `CommentThread`, `ReviewState`,
`MergeBox`, `RefPicker`, `AssetCard`, each with a README and
`preview.html`). The pages define these styles inline instead. Move them into
the design system and have the pages use them; no page-local copies.
