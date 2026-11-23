# MergeBox

The merge status of a merge request: whether it is ready to merge, what is still expected, and the Lore client commands that merge it. The portal shows merge status and commands and never merges; it marks a request merged when it sees the push to the target branch.

## When to use

- At the bottom of a merge request's Conversation tab, and on the states page.
- Checks, in order: conflicts, approvals, changes requested, locks held by others. Approvals and locks are review outcomes; nothing in the box is a merge button.
- `.mbox-cmd` shows the commands to run with the Lore client (merge, or bring the target into the source branch on conflicts) with a Copy button.

## What the consumer provides

Plain markup: `<div class="mbox">` with `.mbox-head` (`.mbox-ico ok|warn|err|merged|muted`, `.mbox-title`, `.mbox-detail`), optional `.mbox-checks` of `.mbox-check ok|warn|err|muted` rows (`.mbox-files` lists files under a check), optional `.mbox-cmd` (`.mbox-cmd-head` and a `pre`; `.note` marks a step that is not a command) and an optional `.mbox-foot`.

## Do and don't

- Do say what is still expected ("1 of 2 approvals") and name lock owners.
- Don't add Merge, Squash or Delete-branch controls; merging happens in the Lore client.
