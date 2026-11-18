#!/usr/bin/env python
"""PreToolUse(Agent) guard: denies an Agent call with no explicit `model`
(planning-dispatch.md Implementation Mode), and an implementation-lane prompt
that sends the subagent to read the whole plan or whole precedent commits
(implementation-dispatch skill, Prompts and hand-offs). Any internal error
exits 0 with no output, so a hook bug never blocks every dispatch.
"""
import json
import os
import re
import sys

REASON = (
    "planning-dispatch.md Implementation Mode: every Agent call passes "
    "an explicit `model` — there is no inherit path. Lanes per "
    ".agents/adapters/claude.md → Dispatch Mapping: [opus]/[independent] → "
    "core-implementer model:\"opus\"; [fable] → model:\"fable\"; [sonnet]/[mechanical] "
    "→ model:\"sonnet\"; [test-author] → test-author per the [test-author] rule in "
    ".agents/skills/implementation-dispatch/SKILL.md Lanes (top tier by default, sonnet "
    "only when copying a top-tier reference); [docs] → docs-writer model:\"sonnet\"; review → "
    "core-reviewer at model ≥ the author's tier; read-only research → "
    "model:\"sonnet\"; listing-only research → model:\"haiku\". Add the model field "
    "and retry."
)

EFFORT_REASON = (
    "Agent Effort: a built-in agent type has no frontmatter effort, so the call "
    "must carry one - set effort: \"medium\" (or \"low\" for listing/search) and retry."
)


IMPLEMENT_LANES = {"core-implementer", "test-author", "docs-writer", "mockup-implementer"}
READ_VERB = r"(?<![\w-])(?:read|study|load|go\s+through)\s+"
WHOLE_PLAN = re.compile(
    READ_VERB + r"(?:through\s+)?(?:the\s+)?(?:whole|entire|full|complete)\s+(?:of\s+)?(?:[\w`./-]+\s+){0,2}?"
    r"[\w`./-]*plan(?:\.md)?`?(?![\w-])"
    r"|" + READ_VERB + r"(?:the\s+)?[\w`./-]*plan(?:\.md)?`?(?:\s+(?:file|document))?\s+(?:in\s+full|end\s+to\s+end)\b",
    re.I,
)
NEGATED = re.compile(r"\b(?:not|never|don['\u2019]t|no\s+need\s+to)\b[^.;:\n]{0,40}$", re.I)
SHA = re.compile(r"\b(?=[0-9a-f]*[a-f])(?=[0-9a-f]*[0-9])[0-9a-f]{7,40}\b")

PROMPT_REASON = (
    "implementation-dispatch skill, Prompts and hand-offs (.agents/skills/implementation-dispatch/SKILL.md; "
    "read it again, it may have changed since you last read it): {what} Everything the subagent reads stays in "
    "its context for every later turn. Rewrite the prompt and retry."
)
WHOLE_PLAN_WHAT = (
    "this prompt tells the implementer to read the whole plan. Paste the step text and the plan sections it "
    "depends on (invariants, step preamble), or name them by heading."
)
PRECEDENT_WHAT = (
    "this prompt names precedent commits without saying what to copy from them. For each, name the file and "
    "function to copy and tell the subagent to read `git show --stat <sha>` and only those hunks."
)


def prompt_problem(subagent_type, prompt):
    if str(subagent_type or "").strip() not in IMPLEMENT_LANES:
        return None
    text = str(prompt or "")
    for found in WHOLE_PLAN.finditer(text):
        if not NEGATED.search(text[max(0, found.start() - 60):found.start()]):
            return WHOLE_PLAN_WHAT
    if re.search(r"\bprecedent", text, re.I) and SHA.search(text) and "--stat" not in text:
        return PRECEDENT_WHAT
    return None


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
            return
        problem = prompt_problem(tool_input.get("subagent_type"), tool_input.get("prompt"))
        if problem:
            deny(PROMPT_REASON.format(what=problem))
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
