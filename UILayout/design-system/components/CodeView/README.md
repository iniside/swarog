# CodeView

A text file with line numbers, an optional annotation (blame) column and highlighted lines.

## When to use

- The file view of a text file at a revision.
- `.att` cells show who last changed a block; `tr.blk` starts a new block; `tr.hl` highlights linked lines.

## What the consumer provides

Plain markup: `<div class="file-head">` above `<table class="cv">` with `td.att`, `td.ln` and `td.src` per row. Token spans `.tk-k`, `.tk-s`, `.tk-c` and `.tk-t` color keywords, strings, comments and types.

## Do and don't

- Do keep `.src` as `white-space: pre`.
- Don't wrap long lines; let the panel scroll.
