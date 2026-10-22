# ServiceStatus

Health of processes or streams: a state dot, the name in mono, a 4-hour uptime strip, latency and a note when something is wrong.

## When to use

Dashboard service health, the topbar health popover, a module's own process (ProcessStatus is the single-item variant with ports, restarts and the health-check log).

## What the consumer provides

Items `{ name, state: "running" | "degraded" | "restarting" | "crashed" | "down", p99, note? }`; options `{ bars: false }` to hide the uptime strip.

## Do and don't

- Do list unhealthy items first in popovers.
- Don't invent a state; use the five above.
