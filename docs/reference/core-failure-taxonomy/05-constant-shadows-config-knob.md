# 5. `constant-shadows-config-knob` / `hardcoded-threshold` / `duplicated-authority`
A threshold/limit/derivation exists in more than one place, or duplicates a value already
computed elsewhere, or is a bare literal with an explanatory comment that reads as intentional.
- **Recurred:** `RETENTION_STALL_MAX` hardcoded 3h shadowing the configurable housekeep
  interval (`47a986c`, the landmark class); rate-pair `*_BURST` parsed without the policy its
  `*_RPS` sibling got (`14cd49e → e5c1b7f`); overflow guard `> u64::MAX` that excludes nothing
  (`e546c08`); scheduler interval ceiling triplicated across DDL + two SQL strings, only 2 of 3
  pinned (`ae749d6 → fb23e4e`); apikeys key-length cap split across two fortress modules
  (`710c0a3`); pool budget `HARNESS_RESERVE=10`/`97` bare literals + spot-checked services
  (`d96e4bf → 5a0ab31`); credential-admission `0` with ambiguous disable-vs-instant semantics
  (`6fe744a`).
- **Attack:** for every literal threshold, **grep for a second source of the same value** and
  hand-evaluate the comparison operator at its exact boundary (`>` vs `>=` vs equal-to-MAX). A
  constant with a good doc comment is the disguise, not the exemption. For a cross-module cap,
  read the write-path limit and the read-path limit side by side.
- **Authority:** one derivation (`plane.retention_stall_after()`, `env_rate_pair`, a shared
  `apikeysapi::MAX_KEY_BYTES` contract const + a DDL CHECK so raw psql is covered too).
