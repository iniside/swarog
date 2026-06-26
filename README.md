# GameBackend

A modular-monolith game backend. One repo, one binary — but built so features are
added by **writing new code, not modifying existing code** (Open/Closed at the
architecture level).

## Core idea

Three seams, and nothing else, carry all extensibility:

1. **Module registry** (`core`) — every feature implements the `Module` interface
   and self-registers. The core never imports a module; modules import the core.
   Dependencies between modules are declared (`DependsOn`) and topologically
   ordered at startup; cycles and missing deps fail loudly.

2. **Service registry** (`Provide` / `Require`) — for *synchronous* needs ("I must
   ask B right now and get an answer"). The consumer asserts the service to its
   own local interface, so it depends on a capability, not on a package.

3. **Event bus** (`Bus`) — the default glue, **asynchronous and fire-and-forget**.
   "I want to react when X happens" => subscribe. `Publish` never blocks and
   returns nothing, so it structurally cannot be used for a synchronous answer
   (use a service interface for that). Each subscriber has its own goroutine and
   FIFO mailbox: per-subscriber order is preserved, a slow/panicking subscriber
   is isolated, and `Close` drains in-flight events on shutdown. State built from
   events is therefore **eventually consistent**. Each publishing domain owns a
   `<module>events` package (depends only on the core foundation) that declares
   its events with `core.Define[Payload]("topic")`. Publish and subscribe go
   through the typed `core.Emit` / `core.On`, so topic-vs-payload mismatches are
   compile errors — the only deliberately shared surface is that descriptor.

## Lifecycle

Modules have three phases. `Init` (required) only wires things up — register
services, subscribe, mount routes; no I/O. `Start` and `Stop` are optional
capabilities (implement `core.Starter` / `core.Stopper` only if you need them):
`Start` kicks off background work in dependency order, `Stop` tears down in
reverse so a module's dependencies outlive it. Shutdown is: stop HTTP, drain the
bus, then `Stop` modules.

## Dependency rules

- Implementations never import each other.
- Implementations may import `*events` contract packages and the core.
- `*events` packages depend on nothing.
- Synchronous dependency only "downward", toward foundations; sideways reactions
  go through the bus.
- Evolve events additively (new field / `FooV2`), never mutate a published shape.

## Layout

```
cmd/server/main.go            # the only place that lists all modules
core/                         # Module, Context, Registry, Bus — no game knowledge
modules/
  match/
    matchevents/              # published events of the match domain (pure data)
    match.go                  # impl: depends on "rating", emits match.finished
  rating/rating.go            # impl: provides the "rating" service, reacts to matches
  leaderboard/leaderboard.go  # impl: pure listener, zero dependencies
```

## Run

```
go run ./cmd/server
```

```
curl -X POST localhost:8080/match/report -d '{"Winner":"alice","Loser":"bob"}'
curl localhost:8080/leaderboard
```

## Status

Baseline architecture for iterating on constraints before planning anything larger.
