---
name: grep-counts-are-lower-bounds
description: A grep COUNT is a lower bound like any other grep sweep — anchor the pattern and read the hits before writing a number into code or docs
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 30210c15-a320-4466-9755-a4dd2b968c51
  modified: 2026-08-30T22:16:03.933Z
---

`grep -c` is a single grep sweep, so Research/Search Mode's "lower bound, not the
answer" applies to it too — and a count is worse than a listing, because the
hits that make it wrong are invisible.

**2026-08-31, seq #2a Step 16.** I counted the accounts ops with
`grep -c '#\[http' api/accounts/api/src/lib.rs` → 9, and "corrected" a false
"six ops" comment in `modules/accounts/src/ops.rs` to a false "nine". The real
count is **7**: two of the nine hits were the string `#[http]` inside doc-comment
prose. Caught by the `docs-writer` agent, not by me — and the step whose whole
purpose was removing lying prose had added a lie.

**Why:** counting the *marker* is not counting the *thing*. In this repo the
marker (`#[http]`, `on_tx`, `emit_tx`, `registry::provide`) is also written
constantly in prose that documents it, so the doc-comment density that makes this
codebase readable is exactly what poisons a naive count.

**How to apply:** anchor to the syntactic position (`^\s*#\[http(`) and **print
the hits, not the count** — then count what you read. Cross-check against a second
authority when one exists: the generated `opscatalog::OPERATIONS`, `routecheck`'s
"fronts N ops" line, a golden file. Any number destined for a comment, a doc or a
commit message needs that second source. See [[prose-about-code-is-not-evidence]]
(the comment you are replacing is not evidence either) and
[[not-pinned-lists-are-unexecuted-prose]] (no gate reads a count — `docs-current`
validates paths, never claims, so a wrong number rots silently forever).
