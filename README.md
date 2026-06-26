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

3. **Event bus** (`Bus`) — the default glue. "I want to react when X happens" =>
   subscribe. Publishers don't know who listens. Each publishing domain owns a
   `<module>events` package (pure data, depends on nothing) that holds its topic
   constants and payload types — the only deliberately shared surface.

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
