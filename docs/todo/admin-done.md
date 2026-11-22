# Admin

Last updated: 2026-11-22

- [x] **Local-render pages run `block_in_place` that the request timeout does not cancel** (2026-11-22, admin-control-plane-redesign step 17)
  ADMIN-02. Obsolete: the pages it named are control-plane providers now. Friends'
  provider (`modules/friends/src/admin.rs`, ad923bfd) and the wallet and mail providers
  (`modules/wallet/src/admin.rs`, `modules/mail/src/admin.rs`) hold no `block_in_place`;
  apikeys, groups and notifications have no `admin.rs` (Grep in each module's `src`, lower bound).
- [x] **Seed the avatar palette from the canonical lowercase id** (2026-11-22, admin-control-plane-redesign step 17)
  ADMIN-03. Obsolete: no module's `src` holds an avatar seed (Grep in apikeys, friends,
  groups, mail, notifications, storage and wallet, lower bound); friends' tab renders
  the other player as a link cell (`modules/friends/src/admin.rs:146`, `user_cell`).
- [x] **Derive `PARAM_PLAYER` and `PLAYER_REF_PREFIX` instead of redeclaring them** (2026-11-22, admin-control-plane-redesign step 17)
  ADMIN-04. Obsolete: neither name, nor `PLAYERS_ROW_MENU`, appears in `modules/{apikeys,friends,groups,mail,notifications,storage,wallet}/src`
  or `core/admin/src` (Grep, lower bound); the providers read the player from the panel
  query (`modules/friends/src/admin.rs:76`, `player_id`).
- [x] **admindiff tests name a real excluded tab and hand-list crawled hosts** (2026-11-22, admin-control-plane-redesign step 17)
  The tests take their example tab from the first player tab in `EXCLUDED_TABS`
  (`tests::excluded_player_tab`) and read the crawled player hosts from `PAGE_MAP`
  (`crawl_tests::player_hosts`), so a tab port edits only `pages.rs`.
- [x] **One accept line covers any number of same-shaped unmatched openers** (2026-11-22, admin-control-plane-redesign step 16)
  Same-label leftovers under one parent are one counted line, `portal-only ×2
  button.menu-item#bd58cd` in the 5 user-menu accept files; one more opener is NEW plus STALE.
- [x] **Every purges-pending link copies core admin's Events page address** (2026-11-21, admin-control-plane-redesign step 14a3)
  `controlplane::EVENTS_PAGE_MODULE`/`EVENTS_PAGE` name the address; core admin's Events
  provider and the characters purges item and its test use them.
- [x] **The crawl follows the newest player, who may have no characters** (2026-11-21, 56126e2a)
  A tab follower now crawls the first list row whose tab page lists a row; with none, it keeps
  the first row and says so.
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
