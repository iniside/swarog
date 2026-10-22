# KpiTile

One number with its unit, a comparison and an optional sparkline or capacity bar.

## When to use

Dashboard tiles (`dashboard.tile`) and the summary row at the top of a module page. Use four per row on a page, up to eight on the dashboard. Open with tiles only when the numbers are the point.

## What the consumer provides

- `label`, `value` (preformatted, tabular), optional `unit`.
- `delta`: HTML using `.up` / `.down` / `.bad-up` / `.flat` spans for direction and meaning.
- `spark`: numbers oldest first; `sparkColor` a token (`var(--chart-2)`, `var(--warn)`); or `bar`: 0–100 for capacity.
- `href` to drill in; `state` (`loading`, `empty`, `stale`, `error`, `denied`). An error tile keeps the last value in its detail line.

## Do and don't

- Do say what "good" is: a rising queue is `.bad-up`, not `.up`.
- Don't put more than one number in a tile.
