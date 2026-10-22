# ConfirmAction

The gate in front of any destructive or expensive action: a question, the impact computed when it opens, an optional reason for the audit log and an optional typed confirmation.

## When to use

Force unlock, revoke a key, delete an account, restart a server, skip a poison event, obliterate. Use typed confirmation when the action cannot be undone or affects more than one object.

## What the consumer provides

`Swarog.confirm({ title, body, impact: [[label, valueHtml]], reason?: placeholder, typeToConfirm?: text, confirm, tone?: "danger" | "warn", icon, footNote?, done })`. `done` is the toast text after confirming.

## Do and don't

- Do phrase the title as a question and the button as the verb ("Force unlock").
- Do show who and what is affected in the impact list.
- Don't use it for reversible, everyday changes.
