# Architecture enforcement (`go-arch-lint`)

The three seams (module registry, service registry, event bus) plus the contribution slot are
only as good as the boundaries around them. The hard constraints in `CLAUDE.md` — *core never
imports a module*, *modules never import each other's impl*, *cross-module comms go through the
`<module>events`/`adminapi` contract or a registry interface* — were, until now, **discipline**:
nothing stopped a careless `import "gamebackend/modules/characters"` from another module. A single
Go module compiles them all together, so the compiler won't catch it.

`.go-arch-lint.yml` makes those constraints **machine-checked**.

## Run it

```
go install github.com/fe3dback/go-arch-lint@latest   # one-time
go-arch-lint check                                    # from repo root; exit 1 on any violation
```

Wire it into CI next to `go vet` / `go test`.

## What the config encodes

- **Components.** Each module impl (`modules/<name>`) is its own component, so cross-impl imports
  can be forbidden. The shared contract surface — every `<module>events` package + `admin/adminapi`
  — is one `contracts` component. `core` is a `commonComponent` (importable by all). `cmd` is the
  composition root.
- **Rules.** `core` and `contracts` have no `deps` entry, so they may use only commons (`core`) —
  i.e. they never import a module impl. Each module impl `mayDependOn: [contracts]` (plus `core`,
  free as a common) — never another module's impl. Only `cmd` may depend on the concrete modules.
- **Vendor imports are ignored** (`allow.depOnAnyVendor: true`) — we enforce *internal* architecture
  only; which third-party libs a module uses is its own business.

## Why no cycle rule

Import cycles need no linter here: **the Go compiler already rejects circular package imports**.
(On the JVM this isn't free — the Kotlin sketch in `experiments/` needs an explicit ArchUnit
`slices().should().beFreeOfCycles()` rule for the same guarantee.)

## Enforcement is at lint time, not compile time

A bad cross-module impl import still **compiles** (`go build` is happy) — `go-arch-lint check` is
what turns it red. Verified: adding `import "gamebackend/modules/characters"` to `inventory`
builds fine but produces *"Component inventory shouldn't depend on gamebackend/modules/characters"*.
The only way to get compile-time enforcement would be splitting each module into its own Go module
(separate `go.mod`), which we deliberately don't — one module keeps the build trivial, and the lint
gate is enough.

## Provenance

Chosen over `depguard` (the more widely-used option, since it ships inside `golangci-lint`) because
this repo has no `golangci-lint` yet, so depguard's "already integrated" advantage didn't apply, and
`go-arch-lint` is the purpose-built, declarative fit for "modular monolith component boundaries" —
the direct equivalent of the ArchUnit rules in `experiments/jvm-kotlin-sketch`. If general-purpose
Go linting is adopted later, folding these rules into `golangci-lint` + `depguard` is the mainstream
alternative.
