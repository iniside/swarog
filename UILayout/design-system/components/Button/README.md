# Button

Buttons in five variants and two sizes; icons sit left of the label.

## When to use

- `primary`: the one main action of a page or modal ("Create key", "Save revision").
- default: secondary actions.
- `danger`: the confirm button of a destructive modal only.
- `danger-ghost`: a destructive entry point in a toolbar ("Force unlock", "Revoke all").
- `ghost`: icon buttons and low-weight actions in tables.

Sizes: 28px default, `sm` 24px in panel tools and table rows; `icon` makes it square.

## What the consumer provides

Plain markup: `<button class="btn primary">`, `<button class="btn sm ghost icon">` with `Swarog.icon(name)` inside.

## Do and don't

- Do label with a verb that says what happens.
- Don't put two primary buttons in one view.
