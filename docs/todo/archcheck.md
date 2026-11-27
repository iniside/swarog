# Archcheck

Last updated: 2026-11-27

## In progress

_None._

## Open

### Known limits (verify-reds-fix step 1)

- [ ] **Rule 22 accepts a DDL template by its name alone**
  Rule 22 (`stray_ddl_text`, tools/archcheck/src/main.rs:1939; `ddl_spans` :1873) exempts any item whose name
  ends in `_DDL`. `SCHEMA_TEMPLATE_DDL` (modules/config/src/lib.rs:113) is a template with an unfilled
  `@MAX_REVISIONS@`, so a future `.sql(SCHEMA_TEMPLATE_DDL)` passes archcheck and fails only when the schema is
  set up at boot. `ddl_ident_escapes` (:1961) also lets any `*_DDL` identifier be used inside any other
  `*_DDL` item. Fix in the rule: a template suffix accepted only inside a `*_DDL` initialiser, or keep
  `_DDL` for SQL that runs as written.
  Source: core-reviewer of fcb351f1 (2026-11-27).

### Known limits (verify-reds-fix step 2)

- [ ] **Rule 15 reads SQL comments inside DDL literals as code**
  `foreign_schema_sql_refs` (tools/archcheck/src/main.rs:2190) skips Rust comment lines but not `--` comments
  inside an SQL string, so a comment like "into <schema>." is flagged as a foreign-schema write. Step 2
  reworded the inventory comment instead. Fix in the rule: drop SQL comments inside literals.
  Source: core-reviewer of b5d559fa (2026-11-27).
