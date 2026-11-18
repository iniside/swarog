# Failure taxonomy history

Archive of how `docs/reference/core-failure-taxonomy.md` was derived and why double review did not fix core. Not a rulebook.

**Provenance.** Mined from the 2026-07-11 → 2026-07-13 remediation window (~217 commits,
of which ~110 are `fix(...)` commits). Six parallel research subagents each read the FULL
diffs of one core-layer slice (lifecycle/app · durable event plane · edge/remote/RPC ·
rollout tooling · verify net · domain modules) and returned records under one schema
(defect → failure class → what hostile review missed → the deciding authority → the
failing-branch proof → fix-on-fix chain). This document is the synthesis. It is the
source that the `core-implementer` / `core-reviewer` agent checklists derive from — update
it when a NEW failure class shows up in a real commit, not from guesswork.

## The one finding that reframes the problem

**Fix-on-fix dominates the window.** Not a few cases — the majority:

- **processctl: 16 of 18 fixes** belong to multi-commit chains against ONE authority —
  the rollout lock/lease (8: `b879fc3 062a5e7 7611a65 aa304a3 db02527 18dcb65 26a7abc
  baa8bbd`), guardian/Windows-spawn (3), build-env allowlist (4).
- **RPC retry classification: 4 commits** (`4ac75cd → ace9e96 → f4b4060 → c829a21`) because
  the first fix classified on a *derived* value (`opsapi::Status`) instead of the raw
  `edge::Error`/Quinn error where the information still existed.
- **devctl teardown status: 6 commits** on one struct (`FleetStatus`/`ManagedStatus`), each
  adding a new input to "is this teardown actually a success".
- Retention (`7cf9957 → e546c08`), golden (`be3aae3 → 606d321`), archcheck rules
  (`23ec41a → 5e8ae52`, `971cf2c → 428182d`), docs scanner (`0d4ae71 → ed2742e`),
  pool budget (`d96e4bf → 5a0ab31`), inventory quantity (`5bfe36b → 2c01e3c`) — all
  round-2 patches of a round-1 fix's OWN new seam.

**What this means.** The hostile-review discipline was *working* — nearly every round-2 fix
exists because an adversarial pass caught a hole. The disease is upstream of review: fixes
were authored **symptom-first**, so each one created a fresh boundary (a new constant, a new
error branch folded into success, a resource owned by the wrong scope, a lock acquired in the
wrong place) that the next review then broke. **More review rounds ≠ fewer bugs; they were a
symptom of the same authorless fixes.** The lever is to make the implementer *locate the
deciding authority before writing*, and to make the reviewer *attack the fix's own new seam
by class*, keyed to what the change touched — not generic hostility.

This is exactly why generic agents kept missing it: the failure classes below are subtle,
repo-specific, and invisible to "does it look plausible / does it compile / does the test
pass" review. A test passing is the #1 disguise in this catalog.

## Implications for the specialized agents (the bridge)

The evidence says the two agents must encode, not just "be hostile":

1. **`core-implementer` — authority-first.** Before writing a fix it must name, in one
   sentence, the single deciding place the change belongs (class → authority column above),
   and state the minimal-sufficient closure. It refuses to add a second special case beside an
   earlier fix (the `build_env` / `b50c553` anti-pattern). It ships a test that runs the
   *previously-wrong branch* on the *at-risk topology* (split for class 9). This directly
   attacks the fix-on-fix disease at its root.
2. **`core-reviewer` — class-keyed, attack the fix's own new seam.** Not generic. It routes by
   the files in the diff to the 3-5 classes above and runs those specific attacks, opening the
   files (never trusting a summary), confirming the negative-path test exercises the failing
   branch, and stating the fix's failure mode out loud.

Both are vehicles for THIS catalog. Keep the catalog current; the agents are only as good as
the classes they carry.
