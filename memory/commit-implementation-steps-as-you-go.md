# Commit implementation steps as they are completed

For every repository-changing task, commit each completed, verified task or
independently reviewable part immediately instead of waiting until the entire
rollout is finished. This applies to planning and documentation work as well as
implementation. Use the plan's Conventional Commit scope, keep tests serialized,
and never bundle unrelated changes into one large final commit. The user explicitly
prefers ongoing granular history in this solo repository; only pushing requires a
separate request.

Durable repo guidance and plans use execution-shape tags (`[inline]`,
`[subagent-complex]`, `[subagent-mechanical]`), never provider-specific model names
or versions. Read `AGENTS.md` directly rather than inferring its rules from a
provider-specific companion file.
