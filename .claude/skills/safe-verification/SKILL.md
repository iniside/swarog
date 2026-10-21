---
name: safe-verification
description: Run tests/verification on this machine WITHOUT tripping the one-test-rollout-at-a-time constraint (shared local Postgres, events-plane advisory locks). Use BEFORE any cargo test, devctl fleet, verifyctl rollout, or dispatching a subagent that will run tests — and whenever a test run hangs, fails with "tuple concurrently updated", or seems stuck on migration. Also selects the minimal verification stage for the change instead of defaulting to full verify.
---

Read and follow the canonical skill at `../../../.agents/skills/safe-verification/SKILL.md`. Do not maintain a second copy.
