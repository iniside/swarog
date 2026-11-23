# CommentThread

A review discussion: comments with avatar, author and time, then a reply field and Resolve.

## When to use

- Line comments inside a DiffView (`tr.thr`) and general comments on a merge request.
- `.thread.resolved` dims a resolved thread.

## What the consumer provides

Plain markup: `<div class="thread">` with `.cmt` rows (an avatar, then `.h` with `<b>author</b> · time` and the body) and a `.thread-foot`.

## Do and don't

- Do keep the reply field in the foot of an open thread.
- Don't nest threads.
