# DiffView

A changed file in a merge request or compare: a header with status, path and line counts, then a unified diff table.

## When to use

- Every text file in a diff. A binary asset puts an AssetCard (`.adiff`) in the same `.dfile` instead of the table.
- `tr.thr` places a CommentThread on a line.
- `.st` (`M`, `A`, `D`, `R`) and `.chg` are reused by FileTree rows and lists.

## What the consumer provides

Plain markup: `<div class="dfile"><div class="dfile-head">` with `.st`, `.path` and `.chg` (`.a` added, `.d` deleted), then `<table class="dv">`. Rows are `tr.hunk`, `tr.add`, `tr.del` or plain context, with cells `.o`, `.n`, `.s` and `.c`.

## Do and don't

- Do keep old and new line numbers both visible.
- Don't color the row text; the background carries add and delete.
