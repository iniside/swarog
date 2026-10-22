# TimeSeriesChart

One or two line series over shared buckets; the second may use a right axis.

## When to use

Usage over time (seats and store TB per month). Always pair it with a table of the exact values.

## What the consumer provides

`labels` (bucket names), `series: [{ values, max, color, unit }]` (max sets the axis top), `right: true` for a second axis, `aria` text. Colors are `var(--chart-1)` and `var(--chart-2)`; gridlines use `chart-grid`.

## Do and don't

- Do put the legend in the panel tools.
- Don't add area fills, more than two series or stacked bars.
