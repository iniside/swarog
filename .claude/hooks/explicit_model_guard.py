#!/usr/bin/env python
"""PreToolUse(Agent) guard: denies an Agent call with no explicit `model`
(planning-dispatch.md Implementation Mode - MANDATORY). Any internal error
exits 0 with no output, so a hook bug never blocks every dispatch.
"""
import json
import sys

REASON = (
    "planning-dispatch.md Implementation Mode (MANDATORY): every Agent call passes "
    "an explicit `model` — there is no inherit path. Lanes per "
    ".agents/adapters/claude.md → Dispatch Mapping: [opus]/[independent] → "
    "core-implementer model:\"opus\"; [fable] → model:\"fable\"; [sonnet]/[mechanical] "
    "→ model:\"sonnet\"; [test-author] → test-author model:\"sonnet\" (opus/fable only "
    "for a novel harness); [docs] → docs-writer model:\"sonnet\"; review → "
    "core-reviewer at model ≥ the author's tier; read-only research → "
    "model:\"sonnet\"; listing-only research → model:\"haiku\". Add the model field "
    "and retry."
)


def main():
    try:
        data = json.load(sys.stdin)
        if (data.get("tool_name") or "") != "Agent":
            return
        model = (data.get("tool_input") or {}).get("model")
        if model is not None and str(model).strip():
            return
        print(json.dumps({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": REASON,
            }
        }))
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
