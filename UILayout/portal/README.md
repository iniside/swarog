# Lore Portal mockups

Static mockups of Lore Portal, the studio web app for Lore repositories and code
review. It is a sibling of Swarog Admin: the same design system and dense
layout, signed in through Swarog, with no tenant switcher. The portal shows
read-only facts about locks; operator actions such as force-unlock stay in
Swarog Admin.

Brief: `docs/design/2026-11-23-2205-lore-portal-mockup-brief-summary.md`.

## Serve

```
python -m http.server 8765 --directory UILayout
```

Open `http://localhost:8765/portal/`. The pages link
`../design-system/tokens.css` and `../design-system/components/bundle.css`, so
serve `UILayout/`, not this folder. Change the design system, not copies here.

## Theme

`?theme=light` or `?theme=dark` forces the theme. Otherwise the topbar button
switches it and the choice is saved; the first visit follows the OS setting.

## Screens

| Screen | File |
| --- | --- |
| Sign in with Swarog | `sign-in.html` |
| Device approval for the desktop client | `device-approval.html` |
| Home | `index.html` |
| Repositories | `repositories.html` |
| Repository files (root, with the root README) | `repo-files.html` |
| Repository folder (`Content/Maps/Frostreach`) | `repo-folder.html` |
| Repository merge requests | `repo-merge-requests.html` |
| Repository locks (every holder) | `repo-locks.html` |
| Text file | `file-text.html` |
| Binary asset with lock states | `file-asset.html` |
| History | `history.html` |
| Revision | `revision.html` |
| Branches | `branches.html` |
| Compare | `compare.html` |
| Merge requests | `merge-requests.html` |
| New merge request | `new-merge-request.html` |
| Merge request, conversation | `merge-request.html` |
| Merge request, files | `merge-request-files.html` |
| Merge box states (MergeBox: the portal shows merge status and Lore client commands and never merges) | `merge-box-states.html` |
| My locks | `my-locks.html` |
| Notifications | `notifications.html` |
| Profile | `profile.html` |
| Panel states (ok, loading, empty, stale, error, read-only, denied) | `states.html` |

## Fake data

Tenant `emberline-prod`, studio Emberline, game Ashfall. The signed-in user is
Aiko Tanaka (`aiko.tanaka`). Ids are shown as `kind:id`, for example
`rev:3fe88a1` and `lock:L-2290`.
