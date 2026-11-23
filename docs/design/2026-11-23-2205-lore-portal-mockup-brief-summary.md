# Lore Portal: mockup brief

## What this is

Design static HTML mockups for **Lore Portal**: the web app where members of a game studio browse their Lore repositories and do code review. It plays the role of GitHub, GitLab or Gitea, but for Lore, a version-control system for game projects that holds large binary assets (Unreal `.uasset`/`.umap`, textures, audio) next to code and uses exclusive file locks.

The portal is a sibling of **Swarog Admin** (`UILayout/redesign/`) and uses the **same design system** (`UILayout/design-system/`: `tokens.css`, `components/bundle.css`, `components/bundle.js`). Read `UILayout/design-system/README.md` first; its content rules, visual foundations, panel states and icon set all apply unchanged. Swarog Admin is for the deployment's operators. Lore Portal is for everyone in the studio: programmers, artists, designers, leads. It keeps the same dense, quiet, Linear-like character and must feel like the same product family.

One deployment means one studio and one Lore server. There is no tenant or organisation switcher. Reuse the fake world from `UILayout/redesign/assets/data.js`: deployment `emberline-prod`, studio Emberline, game Ashfall, repository `ashfall-game` and its sibling repositories, and users such as `aiko.tanaka` and `sam.o`. Keep names, branches and locks consistent with the admin mockups.

## Deliverables

- `UILayout/portal/`: one HTML file per screen, a shared `assets/data.js` with the fake data, a `README.md` with the screen table, built the same way as `UILayout/redesign/`. The pages load `../design-system/`; never copy styles into the portal folder.
- **New primitives go into the design system first**, with a README and `preview.html` per component, as the existing components do. Expected new ones:
  - `FileTree`
  - `CodeView`, with line numbers and the per-line attribution gutter
  - `DiffView`: unified and split views, with inline comment threads
  - `CommentThread`, which can be resolved
  - `ReviewState` badge: approved / changes requested / commented / pending
  - `MergeBox` (merge status and instructions; the portal never merges)
  - `RefPicker`, for choosing a branch or revision
  - `AssetCard`, the preview and metadata of a binary file

  Reuse `Panel`, `DataTable`, `Timeline`, `EntityDetail`, `PropertyList`, `Badge`, `MenuAction`, `ConfirmAction`, `Form`, `KpiTile` and `AttentionItem` wherever they fit.
- Light and dark theme, the topbar state switcher and the seven panel states, as in the admin mockups. `denied` here means "you have no access to this repository".

## Shell

- The sidebar holds:
  - Home;
  - Repositories, with pinned repositories under it;
  - Merge requests, with counters for "Review requested" and "Mine";
  - My locks;
  - Notifications.
- The topbar holds breadcrumbs (`ashfall-game / main / Content/Maps`), a global search palette over repositories, files, merge requests and users, the notification bell, and the account menu: profile, signed-in devices, theme, sign out.
- Brand mark: the same typographic monogram as Swarog Admin, labelled "Lore Portal", with the deployment id in mono.

## Screens

1. **Sign in.** Sign-in goes through Swarog: one button, "Continue with Swarog". Add a state for the redirect back.
2. **Device login approval.** "Enter the code shown in your terminal" (`ABCD-EFGH`), then a screen to confirm "Allow `lore` CLI on WS-AIKO-01 to act as aiko.tanaka?", and the approved and denied results.
3. **Home.** Review requests for me, my open merge requests, my locks (marking stale ones), recent activity in my repositories (pushes, merges, comments), and pinned repositories.
4. **Repositories list.** Name, description, default branch, last push, my access level (read / write / maintain), open merge requests, locks.
5. **Repository: Files.** A `RefPicker` and the file tree. Clicking a directory opens that folder: clickable breadcrumb segments, a ".." row, its children, its README if it has one, and "History of this folder". The README renders under the tree. Directory rows show the last revision and its author. Show the clone command `lore clone lore://lore.emberline.gg/ashfall-game`.
6. **Repository: File view.**
   - Text files: `CodeView` with a "Show attribution" toggle, the per-line author and revision gutter.
   - Binary assets: `AssetCard` with a preview where one exists, size, type, lock state (held by whom and since when, and Lock / Release for me) and this file's history.
7. **Repository: History.** Revisions as a timeline grouped by day: id in mono, message, author, size of the change (files and bytes), and filters by branch, path and author.
8. **Revision detail.** Message, parents and changed files, with a `DiffView` for text and before/after metadata (size, and a preview where one exists) for binaries. Readers can comment on lines and on whole files.
9. **Repository: Merge requests and Locks.** The repository tab bar (Files, History, Branches, Merge requests, Locks) stays inside the repository: Merge requests lists all of this repository's requests (same tabs, filters and rows as screen 11); Locks lists every lock in the repository, held by anyone, with "Release lock" on mine and "Ask to release" on others'. The sidebar entries stay personal.
10. **Repository: Branches.** Name, head, updated, ahead/behind against the default branch, a protected badge, open merge request, and "New merge request" from a branch row.
11. **Compare.** Base … head `RefPicker`s, the summary, revisions and the diff. Ends in "Create merge request".
12. **Merge requests list.**
    - Tabs Open / Merged / Closed, with filters for author, reviewer, label and target branch.
    - Each row: title, `!42`, source → target, author, reviewers with their `ReviewState`, comment count, updated.
13. **New merge request.** Source and target branches, title, description (markdown with a preview), reviewers, labels, a draft toggle, and the change summary beneath.
14. **Merge request detail.**
    - Header: title, state, source → target, author.
    - Tabs:
      - **Conversation**: the description, then a timeline of comments, reviews, pushes and state changes, then the reply box.
      - **Files changed**: `DiffView` with inline threads, a "viewed" checkbox per file, and binary files listed with their size changes.
      - **Revisions**.
    - Submitting a review: a popover with Comment / Approve / Request changes and a summary.
    - The portal does not merge. Merging is done with the Lore client; the portal detects the merge from the push to the target branch. `MergeBox` states:
      - ready, showing the commands to merge locally (`lore branch switch main`, `lore sync`, `lore branch merge feature/x`, `lore push`) with a copy button;
      - blocked by missing approvals (2 of 2 required);
      - blocked by changes requested;
      - conflicts, listing the files, with the commands to merge `main` into the source branch and push it again;
      - warning that files in this request are locked by someone else, naming the files and owners, with "Ask to release";
      - target branch protected and you lack the right;
      - merged, with the revision id and who pushed it;
      - closed.14. **My locks.** My held locks across repositories, marking stale ones, with bulk release. A second tab shows locks others hold on files I touch in open merge requests.
15. **Notifications.** Review requested, mentioned, comment on my merge request, merged, lock released, and a "release request" for a lock I hold. Mark as read.
16. **Profile and devices.** Name, handle, avatar initials, signed-in devices and CLI sessions with Revoke, and personal access tokens for CI with Create (secret shown once) and Revoke.

## Out of scope

Everything an operator does stays in Swarog Admin and must not appear here:
- editing roles and permissions;
- force-unlocking someone else's lock;
- obliterating files;
- server health;
- billing;
- audit.

Where a lead would need such an action, show the read-only fact and nothing else.

## Writing

Follow the content rules of the design system:
- sentence case;
- buttons that say what happens ("Request changes", "Release lock", "Copy merge commands");
- confirmations phrased as questions;
- identifiers in mono, written `kind:id` where they reference an entity;
- units on every number;
- no emoji and no exclamation marks.
