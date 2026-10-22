# Form

Typed fields rendered from a schema, with help, field errors, and Cancel plus a primary submit.

## When to use

Configurators and edits: config values (compare-and-swap on a revision), branch protection, grants, creating an API key. In a modal when it is short and opened from an action.

## What the consumer provides

`fields: [{ label, type, typeName, value, required?, help?, error?, disabled?, options? }]` where `type` is `text` (default), `select`, `bool`, `checks`, `textarea`, `user`, `money`; `typeName` is shown under the label (`int (1..8)`, `ref user`, `enum LoreRole`). `submit` label, optional `note` beside the buttons, `actions: false` to hide them. Pass `true` as the second argument to render read-only.

## Do and don't

- Do show the type under every label; operators edit typed config.
- Do name the CAS revision in the note ("Saves only if still rev 12").
- Don't queue writes when the module is down; disable the form and say why.
