---
name: cite-a-precedent-only-after-reading-it
description: "Copy pattern X" in a plan or punch list requires reading ALL of X first — its supporting index, what drives it, its carve-out sentence; the name of a pattern is not the pattern
metadata:
  type: feedback
---

Naming a precedent ("copy audit's PruneHandler", "batch it like admin's login_attempts")
is an INSTRUCTION, and an implementer will follow it literally. Read the whole
precedent before citing it — including the parts that are not the code you pointed at:
the index that makes its query a range scan, what *drives* it, the doc sentence that
makes its comment true.

**Why:** twice in one rollout (2026-08-31, notifications #3a) a plausible-sounding
"copy X" instruction of mine MOVED a defect instead of closing it, and a reviewer paid
to find it both times.
- "Prune copies audit's PruneHandler" — audit also ships `CREATE INDEX log_at_idx ON
  audit.log(at)` precisely so its identical predicate range-scans. Mine seq-scanned the
  whole table inside a 10s-bounded handler: timeout → terminate backend → retry the same
  unbounded delete → pause after 20 tries, retention dead until an operator intervenes.
- "Bound the batch like admin's login_attempts" — admin's is called every 256 login
  attempts, so its drain rate tracks its INSERT rate. A once-a-day scheduler fire has no
  such coupling, so a 256-cap meant the backlog never drains at any real inflow:
  unbounded storage, retention silently unenforced. The repo's actual retention authority
  (`core/asyncevents/src/retention.rs`) LOOPS batches; audit and accounts drain fully.
- Same class one layer down: an implementer copied audit's env-reading code and dropped
  audit's carve-out sentence, so the comment then promised a typo would stop the process
  while the code silently defaulted.

**How to apply:** before writing "like X" into a plan step or a punch list, open X and
account for (a) what supports it — indexes, constraints, the surrounding DDL; (b) what
drives it — a timer, an inflow counter, a request; (c) what its comments carve out. If
you have not read those three, cite the requirement instead of the precedent and let the
implementer find the shape. Sibling sweep: when you learn how a precedent really works,
check whether the OTHER copies of it in the tree share the same gap.

Same family as [[prose-about-code-is-not-evidence]] and [[grep-counts-are-lower-bounds]]:
a name that stands for code is a lead, not a citation. Enforced in practice by the review
pass — see [[adversarial-subagent-review]].
