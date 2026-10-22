# MenuAction

An action one module hangs on another module's entity: row menus, header actions and bulk actions, grouped by contributing module.

## When to use

Every `entity.<kind>.row_action` and `entity.<kind>.header_action`. The portal groups entries under the module name, shows unavailable ones disabled with the reason (module down, missing permission), and routes a click to a Form, a ConfirmAction or a direct invoke.

## What the consumer provides

Register a menu with `Swarog.defineMenu(id, arg => groups)` where each group is `{ c, label, note?, items: [{ label, icon, danger?, disabled?: reason, hint?, run }] }`, then open it with `Swarog.openMenu(event, id, arg)` from a `Swarog.rowMenuBtn(id, arg)` button.

## Do and don't

- Do end destructive items with an ellipsis when they open a confirmation ("Delete account…").
- Do keep disabled items visible, with the reason as the hover title.
