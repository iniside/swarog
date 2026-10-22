# EntityDetail

The host frame for one entity: header with title, status and facts, header actions, and a tab strip where every tab is contributed by a module.

## When to use

User detail, repository detail, any entity kind a module declares. The portal owns the frame; tabs (`entity.<kind>.tab`), facts (`entity.<kind>.summary_field`) and actions (`entity.<kind>.header_action`) are contributions. A tab whose module is down shows a red dot; a read-only one shows a lock.

## What the consumer provides

Markup with `.panel > .entity-head` (avatar or icon well, `h1`, status badges, `.entity-facts`, `.page-actions`) and `.tabs > .tab` / `.tab.on`. Each contributed piece wrapped with `Swarog.frag`.

## Do and don't

- Do load only the active tab; keep it in the URL (`?tab=lore`).
- Do keep facts to one short line each.
