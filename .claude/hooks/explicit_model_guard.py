#!/usr/bin/env python
"""PreToolUse(Agent) guard: denies an Agent call with no explicit `model`
(planning-dispatch.md Implementation Mode). Any internal error
exits 0 with no output, so a hook bug never blocks every dispatch.
"""
import json
import os
import sys

REASON = (
    "planning-dispatch.md Implementation Mode: every Agent call passes "
    "an explicit `model` — there is no inherit path. Lanes per "
    ".agents/adapters/claude.md → Dispatch Mapping: [opus]/[independent] → "
    "core-implementer model:\"opus\"; [fable] → model:\"fable\"; [sonnet]/[mechanical] "
    "→ model:\"sonnet\"; [test-author] → test-author per the [test-author] rule in "
    ".agents/shared/planning-dispatch.md (top tier for gate tests and a novel "
    "harness, sonnet for ordinary tests); [docs] → docs-writer model:\"sonnet\"; review → "
    "core-reviewer at model ≥ the author's tier; read-only research → "
    "model:\"sonnet\"; listing-only research → model:\"haiku\". Add the model field "
    "and retry."
)

EFFORT_REASON = (
    "Agent Effort: a built-in agent type has no frontmatter effort, so the call "
    "must carry one - set effort: \"medium\" (or \"low\" for listing/search) and retry."
)


def has_repo_agent(subagent_type):
    name = str(subagent_type or "").strip()
    if not name or "/" in name or "\\" in name or ".." in name:
        return False
    root = os.environ.get("CLAUDE_PROJECT_DIR") or os.getcwd()
    return os.path.isfile(os.path.join(root, ".claude", "agents", name + ".md"))


def deny(reason):
    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    }))


def main():
    try:
        data = json.load(sys.stdin)
        if (data.get("tool_name") or "") != "Agent":
            return
        tool_input = data.get("tool_input") or {}
        model = tool_input.get("model")
        if model is None or not str(model).strip():
            deny(REASON)
            return
        effort = tool_input.get("effort")
        if (effort is None or not str(effort).strip()) and not has_repo_agent(tool_input.get("subagent_type")):
            deny(EFFORT_REASON)
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
