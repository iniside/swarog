#!/usr/bin/env python
"""Fix brief guard: an implementation call is a plan step or carries a diagnosis.

PreToolUse Agent. .agents/skills/implementation-dispatch/SKILL.md **Fixes outside a plan**.
A core-implementer, mockup-implementer or test-author prompt passes when it
  (a) says "Do Step <id>" or "Implement Step <id>" with an existing plan file (docs/plans/...-plan.md) named in
      the same sentence (or as the only plan path in the prompt), for a step of that plan: a "### Step <id>"
      heading, a bold "**<id>" or "**Step <id>" at a line start, or the id in the plan's "Execution order"
      paragraph (a part hand-off repeats this line); every such step must resolve, or
  (b) holds a "Diagnosis:" section with at least one path:line anchor and a "Change:" line (a review punch
      list passes as the Diagnosis with the reviewer's anchors).
Anything else is denied with the diagnosis route.
A continuation brief (core-implementer, test-author, docs-writer or mockup-implementer: "step <id>" followed on
the same line by "part <k>" with k >= 2, "part <k> of step", "part <k> of <n>", "this is part <k>", or
"continuing the part hand-off") also needs the hand-off pasted in: a "Done:" line and a "Remaining:" section
with at least one path:line anchor and a "Change:" line after it (the Long steps paragraph of each agent
definition). Other agent types pass. Any error exits 0 with no output.
"""

import json
import os
import re
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DIAG_LANES = {"core-implementer", "mockup-implementer", "test-author"}
CONT_LANES = {"core-implementer", "test-author", "docs-writer", "mockup-implementer"}
# Wider than commit_format.STEP_ID, which stops before a trailing letter (13d2b).
STEP = r"(?:[0-9]+[0-9a-z]*(?:\.[0-9a-z]+)*|[A-Z])"
STEP_END = r"(?![\w-]|\.\w)"
PLAN = re.compile(r"(docs/plans/[\w./-]+?-plan\.md)")
DO_STEP = re.compile(r"\b(?:Do|Implement)\s+Step\s+(" + STEP + r")" + STEP_END, re.I)
DIAG = re.compile(r"^\s*(?:#+\s*)?(?:\*\*)?Diagnosis\b(?:\*\*)?\s*:", re.I | re.M)
ANCHOR = re.compile(r"[\w./\\-]*\w\.(?=[A-Za-z0-9]*[A-Za-z])[A-Za-z0-9]{1,10}:\d+")
SENTENCE_END = re.compile(r"[.;!?](?=\s|$)|\n")
CHANGE = re.compile(r"^\s*(?:[-*]\s*)?(?:\*\*)?Change\b(?:\*\*)?\s*:", re.I | re.M)
PART_K = r"part\s+(?:[2-9]|\d{2,})[a-z]?\b"
CONT = re.compile(r"\bstep\s+" + STEP + STEP_END + r"[^\n]{0,120}?\b" + PART_K +
                  r"|\bpart\s+(?:[2-9]|\d{2,})[a-z]?\s+of\s+(?:the\s+)?(?:step\b|\d+\b)"
                  r"|\bthis\s+is\s+" + PART_K +
                  r"|\bcontinu\w*\s+(?:from\s+)?(?:the\s+|a\s+)?part(?:[- ]\d+)?\s+hand-?off\b", re.I)
DONE = re.compile(r"^\s*(?:#+\s*)?(?:[-*]\s*)?(?:\*\*)?Done\b(?:\*\*)?\s*:", re.I | re.M)
REMAIN = re.compile(r"^\s*(?:#+\s*)?(?:[-*]\s*)?(?:\*\*)?Remaining\b(?:\*\*)?\s*:", re.I | re.M)

REASON = (
    "{lane} outside a plan step needs a diagnosis first (.agents/skills/implementation-dispatch/SKILL.md "
    "**Fixes outside a plan**). Dispatch code-scout with a diagnosis question (subagent_type:\"code-scout\", "
    "model:\"sonnet\", effort:\"medium\"; the symptom, the expected behaviour and how it shows), then put its result "
    "in this prompt as a 'Diagnosis:' section: the cause with path:line anchors and a 'Change:' line per change "
    "point. A plan step passes as 'Do Step <id>' (or 'Implement Step <id>') with the plan path; a review punch "
    "list goes in as the Diagnosis with the reviewer's anchors, one 'Change:' line per finding."
)
CONT_REASON = (
    "A continuation brief carries the part hand-off itself (the Long steps paragraph of the agent definition; "
    ".agents/skills/implementation-dispatch/SKILL.md **Step parts**): paste its 'Done:' (commit shas, build and "
    "test state) and its 'Remaining:' section, one point per line with a path:line anchor and a 'Change:' line "
    "saying what to edit there. A pointer to the previous agent's output or a summary without anchors makes the "
    "new agent re-read whole files."
)


def step_in_plan(step, text):
    sid = re.escape(step)
    if re.search(r"^#{2,4}\s*Steps?\s+" + sid + STEP_END, text, re.M | re.I):
        return True
    if re.search(r"^\s*(?:[-*]\s+)?\*\*(?:Steps?\s+)?" + sid + STEP_END, text, re.M | re.I):
        return True
    order = re.search(r"^Execution order\b.*?(?:\n\s*\n|\Z)", text, re.M | re.S | re.I)
    return bool(order and re.search(r"(?<![\w.])" + sid + STEP_END, order.group(0)))


def sentence_plan(prompt, at):
    start = max((m.end() for m in SENTENCE_END.finditer(prompt, 0, at)), default=0)
    end = SENTENCE_END.search(prompt, at)
    found = PLAN.search(prompt[start:end.start() if end else len(prompt)])
    return found and found.group(1)


def plan_step_ok(prompt):
    steps = list(DO_STEP.finditer(prompt))
    if not steps:
        return False
    plans = set(PLAN.findall(prompt))
    for m in steps:
        path = sentence_plan(prompt, m.start()) or (len(plans) == 1 and next(iter(plans)))
        full = path and os.path.join(REPO, path)
        if not full or not os.path.isfile(full):
            return False
        with open(full, encoding="utf-8", errors="replace") as f:
            if not step_in_plan(m.group(1), f.read()):
                return False
    return True


def diagnosis_ok(prompt):
    m = DIAG.search(prompt)
    if not m:
        return False
    rest = prompt[m.end():]
    return bool(ANCHOR.search(rest)) and bool(CHANGE.search(rest))


def continuation_ok(prompt):
    if not CONT.search(prompt):
        return True
    m = REMAIN.search(prompt)
    if not m or not DONE.search(prompt):
        return False
    rest = prompt[m.end():]
    return bool(ANCHOR.search(rest)) and bool(CHANGE.search(rest))


def deny(reason):
    print(json.dumps({"hookSpecificOutput": {
        "hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}}))


def main():
    payload = json.loads(sys.stdin.buffer.read().decode("utf-8", "replace"))
    if not isinstance(payload, dict) or payload.get("tool_name") != "Agent":
        return
    inp = payload.get("tool_input") or {}
    kind = str(inp.get("subagent_type") or "").strip()
    prompt = str(inp.get("prompt") or "")
    if kind in DIAG_LANES and not (plan_step_ok(prompt) or diagnosis_ok(prompt)):
        deny(REASON.format(lane=kind))
    elif kind in CONT_LANES and not continuation_ok(prompt):
        deny(CONT_REASON)


if __name__ == "__main__":
    try:
        main()
    except Exception:
        if os.environ.get("FIX_BRIEF_DEBUG") == "1":
            raise
    sys.exit(0)
