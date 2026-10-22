# AttentionItem

A module's claim that something needs an operator now, sorted by severity across modules on the dashboard.

## When to use

`dashboard.attention`: a service down, a paused subscription, stale locks, a failing schedule. Each item links to where it is resolved.

## What the consumer provides

`{ severity: "err" | "warn", icon, title, text, link, action }`. Markup: `.pop-row` with a `.modal-icon.danger` or `.modal-icon.warn` well, title, detail line and a small button naming the next step ("Resolve", "Review").

## Do and don't

- Do state the consequence in the detail line ("grants and ledger are offline").
- Don't add an item for things that are merely informational; that is a notification.
