# Architecture enforcement (Rust workspace)

The architecture is enforced by Rust tools in the workspace and orchestrated by
`verifyctl`. The source of truth for the rules is [AGENTS.md](../../AGENTS.md);
this page maps those rules to their executable gates.

## Run the gates

```sh
cargo run -p verifyctl -- --fast
```

`--fast` is the blocking manifest. Use `--all` to add advisory checks,
`--all --strict` to make advisory failures blocking, or `--slow` to run blocking
plus advisory checks and add mutation testing. Under `--slow`, advisory failures
remain non-blocking unless `--strict` is also present. Do not run individual
checkers as a substitute for the manifest: the
runner serializes the shared-Postgres rollout, freezes the environment, preserves
logs, and reports one authoritative result table.

## Blocking architecture gates

- **fortress** builds every `cmd/*-svc` through `tools/checkmodules` and runs
  `archcheck`. `archcheck` rejects foundation-to-domain dependencies,
  module-to-module implementation dependencies, foreign RPC-glue dependencies,
  topology-aware transport state inside modules, missing service composition
  roots, and illegal consumers of `demos/*`.
- **routecheck** compares the declared HTTP/player route surface with the process
  profiles that host it, catching missing, duplicate, or wrongly exposed routes.
- **codegen-freshness** regenerates committed RPC and external-client output in a
  temporary location and fails on drift.
- **contract-golden** compares the generated wire-contract inventory with the
  committed golden file.
- **conformance** checks the centralized convention inventory in
  `tools/conformance/src/policy.rs`, denies module/convention drift, and runs the
  repository-owned executable probes. It is a behavioral supplement to
  `archcheck`; an `Applies` stance needs a biting fixture and a `NotApplicable`
  stance needs a concrete reason.
- **split-proof** boots the real split fleet, drives traffic through the gateway,
  reruns the monolith for parity, and proves owned graceful shutdown.

Build, clippy, tests, and dependency audit are also blocking. An audit invocation,
installation, or network error is a failure; it is never converted into a green
skip. An explicitly selected `--no-install` is the sole missing-tool exception and
is reported as SKIP rather than PASS.

## Advisory contract gates

`--all` adds:

- **public-api**, which diffs every contract crate against
  `docs/reference/public-api-baseline/` so breaking changes require an explicit
  review and additive changes remain visible;
- **topiccheck**, which validates durable topic versions, subscription identifiers,
  allowed sinkless topics, and exactly one subscription host per deployment
  profile;
- **csharp-client**, an external generated-client and live player-QUIC proof; and
- **fuzz**, where supported by the current platform.

Intentional baseline changes are explicit actions:

```sh
cargo run -p verifyctl -- --bless-public-api
cargo run -p verifyctl -- --bless-contract-golden
```

Both actions take the same rollout lease and replace baseline sets recoverably.

## What the compiler does and does not prove

Cargo already rejects Rust crate dependency cycles and type errors, but a Cargo
workspace can still compile architecture violations such as one domain importing
another domain's implementation crate. That is why the graph and source checks in
`archcheck`, the every-service fortress build, and the live topology proof are all
required. A green compilation alone is not an architecture proof.
