# 8. `hand-maintained-list-drift` — *an allowlist/enumeration with no self-check against the source of truth*
- **Recurred:** Windows `BUILD_ENV_ALLOWLIST` missing a var per incident, never enumerated —
  and `b50c553` bolting on a SECOND uncoordinated authority (a directory scanner) instead of
  fixing the list's self-check (`1190001`, `54dea2e`, `b50c553`); pool-budget test
  spot-checking 2-3 services not all (`5a0ab31`); binary-resolution duplicated in `csharp.rs`
  vs the shared `WorkspaceLayout` (`c2546dc`); package-name constant vs real crate name
  (`3cbc2f0`).
- **Attack:** does this list have a test that catches the **next** missing entry, or only one
  pinning the entry just added? (Repo memory: *"didn't-forget" tooling must self-check* —
  diff against the real source of truth and die pre-work.) A second hand-rolled authority for
  the same problem is the smell that the first authority is wrong — replace, don't add.
- **Authority:** derive from the real source (loop over `fleet.services()`; one
  `WorkspaceLayout`; a completeness scan that fails on an unenumerated case).
