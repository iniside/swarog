# Badge

Status and category labels: a toned pill with a dot for status, a plain pill for categories.

## When to use

Every status in the portal goes through `Swarog.status(value)`, which maps the value to its tone once: running, active, sent, ok → ok; pending → info; retrying, lagging, degraded, suspended → warn; failed, paused, down, crashed, banned → err; parked, disabled, revoked → neutral. Use `Swarog.badge(text, tone, plain)` for categories (roles, kinds).

## What the consumer provides

The raw status value. Never pick a tone per screen.

## Do and don't

- Do keep the word: color alone never carries the status.
- Don't use a badge for counts in nav; those are plain numbers.
