#!/usr/bin/env python
"""PreToolUse(Agent|EnterWorktree) guard: denies EnterWorktree and an Agent call
with isolation "worktree". The shell `git worktree add` path is covered by
git_safety_hook.py. Any internal error exits 0 with no output.
"""
import json
import sys

REASON = (
    "core-rules.md Git Safety (MANDATORY): never use a git worktree — a second "
    "tree means a second `target/` and a second build/test rollout against the "
    "one shared local Postgres (migrate advisory lock, concurrent DDL). Run the "
    "agent in the primary tree (omit worktree isolation) or work without "
    "EnterWorktree."
)


def deny(reason):
    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    }))
    sys.exit(0)


def main():
    try:
        data = json.load(sys.stdin)
        tool = data.get("tool_name") or ""
        tool_input = data.get("tool_input") or {}
        if tool == "EnterWorktree":
            deny(REASON)
        if tool == "Agent":
            isolation = str(tool_input.get("isolation") or "").strip().lower()
            if isolation == "worktree":
                deny(REASON)
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
