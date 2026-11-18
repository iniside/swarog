# 11. `old-rail` / `dual-path` / `migration-shim` — *the new thing landed, the old thing still runs*

**What.** A step introduces a replacement (function, type, schema, event version,
subscription, config key, instruction file, code path) and leaves the previous
rail alive: dual-write, `if split` / `if monolith` in a module, a compatibility
column, a second special-case beside an earlier fix, an alias file next to the
new name, “kept so it compiles between steps”.

**Attack.** Name what the step introduces. Name what must therefore be dead. Is
the dead chain still in the tree (symbol, call sites, `use`, crate dep, SQL,
env key, topic/subscription id, docs that still teach the old rail)? Are there
now two runtime ways to do the same thing? Did delete stop at the entry point
and leave the rest compiling? On a plan: does step (a) name what DIES, and is
razing in the same step as the replacement (or earlier)? “We’ll remove it
later” / “migrate” / “for backward compatibility” is this class — wipe is the
strategy, not a data-migration bridge.

**Authority.** `.agents/shared/core-rules.md` (no dual-write / no topology
branch / wipe) and Fix the Authority rule 2 (no hack-on-hack). The
`core-reviewer` always-on class runs this **before** the other classes on
every plan and every diff.
