# Documentation map

Start with the repository-level documents:

- [README](../README.md) — architecture overview and current run/verify commands.
- [AGENTS](../AGENTS.md) — authoritative repository constraints and working
  agreements.
- [CLAUDE](../CLAUDE.md) — the same project constraints plus Claude-specific
  memory and review workflow.

## Current reference

- [Architecture enforcement](reference/architecture-enforcement.md)
- [Gateway](reference/gateway.md) and
  [edge/gateway QUIC](reference/edge-gateway-quic.md)
- [Event-plane operations](reference/event-plane-ops.md)
- [External C# client](reference/csharp-client.md)
- [Hetzner deploy checklist](reference/hetzner-deploy-checklist.md)
- [Commit format](reference/commit-format.md)
- [Research mode](reference/research-mode.md),
  [implementation mode](reference/implementation-mode.md), and
  [plan-writing workflow](reference/plan-writing-workflow.md)

Committed public-API snapshots and contract golden files under `reference/` are
machine-checked baselines, not narrative guidance.

## Historical material

`plans/` contains dated implementation plans. Treat completed plans as historical
records: do not rewrite their commands or conclusions to look current; use a new
plan or an explicit erratum when the present design changes.

The Go-era BaaS gap matrix and the JVM testing/Quarkus notes under `reference/`
are retained research snapshots rather than instructions for the Rust workspace.

`../experiments/` contains the retired Go original and JVM sketches. Their local
READMEs describe those archived implementations and must not be used as current
Rust workspace instructions.
