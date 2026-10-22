# Timeline

Time-ordered entries, newest first, grouped by day, each with an icon, a summary, an optional detail line and a tone.

## When to use

Event feeds (pushes, locks, branch changes), recent operator activity, scheduler fires, config revisions, a user's inbox. Cursor-paginated backwards with "Load older".

## What the consumer provides

Entries `{ t, day?, icon, tone?: "ok" | "warn" | "err" | "info" | "accent", html, meta? }`. Order them newest first; the component draws a day divider whenever `day` changes. An empty list renders the empty state.

## Do and don't

- Do start the summary with the actor and a past-tense verb: "diego.r pushed 3 commits".
- Do use tone for meaning (a forced unlock is `err`), not for variety.
