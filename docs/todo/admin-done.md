# Admin

Last updated: 2026-11-21

- [x] **Disabled claim ignores the refusal kind** (2026-11-21, admin-control-plane-redesign step 13d7c2)
  Every disabled action control the portal draws carries `data-refusal` (`denied`,
  `not-implemented`, `read-only`, `module-down`) from the action's `ActionItem`; a
  `PortalModal::Disabled` claim holds only on a control of its action refused as
  `not-implemented`, and any other kind fails it naming the host, the action and the kind.
- [x] **Disabled claim matched by label** (2026-11-21, admin-control-plane-redesign step 13d7c2)
  Every disabled action control carries `data-refused-action` (`<module>/<action>`);
  `PortalModal::Disabled` names only the action and the crawl matches the control by it. The
  label field and the label match are deleted.
