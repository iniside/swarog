# PermissionMatrix

A two-axis grid of small enum values: boolean (roles × actions) or override (users × resources with inherit, a value, or none). Edits are staged and applied as one change set.

## When to use

Lore role definitions and per-repository overrides; any permission model with a base value and exceptions. Pair it with an effective-permissions preview that explains one cell.

## What the consumer provides

Rows (users or roles), columns (actions or resources), cells `{ value, source: inherit | explicit }`, the allowed values, locked rows (built-in roles). Markup: `.matrix` table, `.check-cell` / `.check-cell.on` for booleans, `.pcell.inherit` / `.override` / `.none` / `.pending` for overrides, and a `warn` banner listing unsaved changes with Discard and Apply.

## Do and don't

- Do make inherit visibly different (dashed) from an explicit value.
- Do show staged changes before they apply.
