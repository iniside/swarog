# ReviewState

A reviewer's verdict on a merge request: approved, changes requested, commented or pending.

## When to use

- Reviewer lists, timeline events and merge-request rows.

## What the consumer provides

Plain markup: `<span class="rs approved"><span class="dot">icon</span>label</span>`; the kinds are `approved`, `changes`, `commented` and `pending`.

## Do and don't

- Do label with the reviewer or the verdict, never color alone.
- Don't use it for merge status; that is MergeBox.
