#!/usr/bin/env python
"""PreToolUse(ExitPlanMode) reminder: injects the plan finalization checklist as
additionalContext. Never denies; always exits 0.
"""
import json
import sys

CHECKLIST = (
    "PLAN FINALIZATION CHECKLIST (planning-dispatch.md Plan Writing Workflow + "
    "Implementation Mode - MANDATORY). Before presenting: "
    "1) The plan was reviewed by ONE `core-reviewer` subagent (subagent_type: "
    "\"core-reviewer\", never general-purpose/Explore/Plan) at model >= the session "
    "tier — state which model ran it and how its punch list was addressed. "
    "2) Every step names its lane tag ([inline]/[opus]/[fable]/[sonnet]/"
    "[test-author]/[docs]), the explicit `model:` and the `effort:` value. "
    "3) Every step touching testable production code is followed by a SEPARATE, "
    "later [test-author] step naming the landed behaviour, the previously-wrong "
    "branch each test exercises, and the at-risk topology (split, not just monolith)."
)


def main():
    try:
        sys.stdin.read()
    except Exception:
        pass
    try:
        print(json.dumps({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": CHECKLIST,
            }
        }))
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
