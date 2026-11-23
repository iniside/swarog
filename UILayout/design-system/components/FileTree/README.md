# FileTree

File listings: a repository folder (`.ft`) and the changed-files tree of a merge request or compare (`.ftree` with `.fl-row`).

## When to use

- `.ft` with `.ft-head` and `.ft-row`: browsing a repository folder; `.lockchip` marks a locked file.
- `.ftree`: the left column of a diff view; `.dir` groups rows, `.fl-row.on` is the open file, `.vw` marks a viewed file.

## What the consumer provides

Plain markup. `.ft-row` holds an icon (`.ft-ico`, `.ft-ico.dir`), name (`.nm`), last message (`.msg`), size (`.sz`) and time (`.when`). `.fl-row` holds a DiffView status letter (`.st`), the path (`.p`) and an optional trailing element.

## Do and don't

- Do show paths in mono and truncate the message column.
- Don't put actions in tree rows; they belong in the file view.
