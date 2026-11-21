# Admin

Last updated: 2026-11-21

- [x] **Menu and modal diffs without an accept section stay red** (2026-11-21, admin-control-plane-redesign step 14)
  Accept files for `players.menu.user`, `player.menu.user`, `player.characters.menu.user` and
  `events.modal.skip-poison`; the refused item's `span.hint` went in 84fc8662. Monolith and
  split crawls diff clean on all four.
- [x] **Pin `PLAYERS_ROW_MENU` ordering across topologies** (2026-11-21, b8435c9a)
  ADMIN-06. characters, friends, notifications and wallet use priority 0 in their
  src/admin.rs; inventory uses 10. Source:
  docs/plans/2026-09-05-1921-friends-module-plan.md:601 (2026-09-05).
  Obsolete: Step 1 razed the per-module admin pages and `PLAYERS_ROW_MENU`; no port declares it.
- [x] **A bare grid panel keeps the mockup's `min-width: 0`** (2026-11-21, admin-control-plane-redesign step 13e6)
  `core/admin/src/portal.css` gives every child of a `.grid > .slot` `min-width: 0`, as the
  mockup's `.grid > *` rule does for a panel drawn straight in the grid.
- [x] **The dashboard draws its single panels bare** (2026-11-21, admin-control-plane-redesign step 13e6)
  The g-21 row and `#health` no longer wrap their panels in `div.stack`
  (`core/admin/src/templates/shell.html.tmpl`); the two accept lines in
  `tools/admindiff/accept/index.txt` are gone.
- [x] **Disabled claim ignores the refusal kind** (2026-11-21, admin-control-plane-redesign step 13d7c2)
  Every disabled action control the portal draws carries `data-refusal` (`denied`,
  `not-implemented`, `read-only`, `module-down`) from the action's `ActionItem`; a
  `PortalModal::Disabled` claim holds only on a control of its action refused as
  `not-implemented`, and any other kind fails it naming the host, the action and the kind.
- [x] **Disabled claim matched by label** (2026-11-21, admin-control-plane-redesign step 13d7c2)
  Every disabled action control carries `data-refused-action` (`<module>/<action>`);
  `PortalModal::Disabled` names only the action and the crawl matches the control by it. The
  label field and the label match are deleted.
