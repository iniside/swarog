# Annotation

The contribution layer: every fragment carries its module, extension point and primitive, and the "Show contributions" toggle outlines and labels them.

## When to use

On every contributed fragment, through the `c: { module, point, prim }` option of any primitive or `Swarog.frag(c, html, opts)` directly. Toggle the layer with the `annot` class on `body`. Inline fragments (nav entries, tabs, facts, header actions) show only the module chip until hovered.

## What the consumer provides

`module` (a module id such as `lore`, `wallet`, `core.events`), `point` (an extension point such as `entity.user.tab`, `dashboard.tile`, `page`), `prim` (a primitive name from this system). Module colors come from `Swarog.MODULE_COLORS`.

## Do and don't

- Do annotate host frames too (`entity.user (host)`) so the owner of the frame is visible.
- Don't ship the layer on by default; it is an operator and developer tool.
