---
name: verify-repo-state-before-resuming
description: "After any gap, read git log before acting — my context is a snapshot, not the state of the world"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 3dcd09ee-8844-4095-a44d-a83c5ff1a9f8
  modified: 2026-09-06T16:15:14.025Z
---

On 2026-09-06 I resumed a rollout from context that ended at Step 10 of seq #3b and
dispatched an audit as though the work were mid-flight. It was not: Steps 11-13 had
landed, `verifyctl` had passed, the tracker row was ✅, and **68 commits plus two whole
later rollouts** had happened in the six days my context did not cover. I reported
progress on a finished rollout and told the user I was "continuing" it.

**Why:** a compacted or resumed context preserves what I saw, not what is true now. The
date jumping is the tell, and so is a user instruction that assumes a state ("finish the
rest") — an instruction is evidence about their mental model, never about the repo.

**How to apply:** before the first action of a resumed session, run `git log --oneline`
(and check the tracker/plan status when a rollout is involved). If the head is not where
my context left it, say so before doing anything else. Cheap, one command, and it is the
same discipline as [[prose-about-code-is-not-evidence]] applied to my own memory: a
summary describing the repo is a lead, not a citation. Related:
[[scope-claims-to-what-was-verified]].
