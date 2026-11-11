---
name: linkedin-posts-edit-facts-not-style
description: "LinkedIn series about Swarog — user's draft IS the template; fact-check against the repo and fix minimally, never rewrite the style; dual audience (technical readers + non-technical buyers)"
metadata:
  node_type: memory
  type: feedback
  originSessionId: f554884e-b923-4bb6-a5c3-8d01ac81b5d7
  modified: 2026-11-03T06:53:49.313Z
---

Lukasz writes a LinkedIn post series based on this repo (post 1: "4 months, 1 engineer, $1,200"; post 2: the audit story, published 2026-11-03).

His own draft is the template. My job: fact-check claims against the repo (git log, docs/reviews, docs/plans) and make minimal edits to wrong or overclaimed sentences. Don't restyle or rewrite the post.

**Why:** On 2026-11-03 I rewrote post 2 three times in different styles (plain-engineer, then business-only), and he lost patience: "tak to my do niczego nie dojdziemy... miałeś pierwszą wersję jako wzór". He posted his own version.

**How to apply:** The post must reach two audiences at once:
- technical readers, who should see it is not total vibe-coding (so concrete, checkable details matter)
- non-technical people with money who might hire him (so the LinkedIn style with emoji and CTA is deliberate, not a flaw)

Don't call the style "bełkot" and don't propose a different register unless he asks. Give fact corrections as small sentence-level swaps on his text. He also corrects facts from memory (e.g. the audit started when he spot-read files and found modules hardcoded to each other plus monolith-vs-split if/else); verify those with git, then use them.
