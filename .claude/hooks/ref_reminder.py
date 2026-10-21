#!/usr/bin/env python
"""PreToolUse(Edit|Write|MultiEdit|NotebookEdit and Bash|PowerShell): injects a
once-per-session reminder to read the reference doc or skill relevant to the
path being edited or the rollout command being run. Markers live under
%TEMP%/claude/ref-hook keyed by session id + doc. Any internal error exits 0
with no output.
"""
import json
import os
import re
import sys

EDIT_TOOLS = {"Edit", "Write", "MultiEdit", "NotebookEdit"}
SHELL_TOOLS = {"Bash", "PowerShell"}

TAXONOMY = "docs/reference/core-failure-taxonomy.md"
EVENT_PLANE = "docs/reference/event-plane-ops.md"
PUSH_HUB = "docs/reference/push-hub.md"
MOCKUP_SKILL = ".agents/skills/mockup-implementation/SKILL.md"
SAFE_VERIFY_SKILL = ".agents/skills/safe-verification/SKILL.md"
PLATFORM_NOTES = "docs/reference/platform-notes.md"

PATH_TRIGGERS = (
    ("core/asyncevents/", (EVENT_PLANE,)),
    ("core/edge/", (PUSH_HUB,)),
    ("modules/gateway/", (PUSH_HUB,)),
    ("core/", (TAXONOMY,)),
    ("uilayout/", (MOCKUP_SKILL,)),
    ("modules/admin/templates/", (MOCKUP_SKILL,)),
)

ROLLOUT_COMMAND = re.compile(
    r"\bcargo(?:\.exe)?\s+(?:\+\S+\s+)?test\b|\bverifyctl\b|\bdevctl(?:\.exe)?\s+(?:--\s+)?up\b",
    re.IGNORECASE,
)


def relative_path(path, data):
    norm = path.replace("\\", "/")
    for root in (os.environ.get("CLAUDE_PROJECT_DIR"), data.get("cwd")):
        if not root:
            continue
        base = root.replace("\\", "/").rstrip("/") + "/"
        if norm.lower().startswith(base.lower()):
            return norm[len(base):].lower(), True
    return norm.lower(), False


def path_docs(path, data):
    rel, anchored = relative_path(path, data)
    docs = []
    for prefix, targets in PATH_TRIGGERS:
        hit = rel.startswith(prefix) if anchored else ("/" + prefix) in ("/" + rel)
        if hit:
            docs.extend(targets)
    return docs


def main():
    try:
        data = json.load(sys.stdin)
        tool = data.get("tool_name") or ""
        session = str(data.get("session_id") or "nosession")
        tool_input = data.get("tool_input") or {}

        docs = []
        if tool in EDIT_TOOLS:
            path = tool_input.get("file_path") or tool_input.get("notebook_path") or ""
            if isinstance(path, str) and path:
                docs = path_docs(path, data)
        elif tool in SHELL_TOOLS:
            command = tool_input.get("command") or ""
            if isinstance(command, str) and ROLLOUT_COMMAND.search(command):
                docs = [SAFE_VERIFY_SKILL, PLATFORM_NOTES]

        seen = set()
        docs = [d for d in docs if not (d in seen or seen.add(d))]
        if not docs:
            return

        temp = os.environ.get("TEMP") or os.environ.get("TMP") or "/tmp"
        marker_dir = os.path.join(temp, "claude", "ref-hook")
        try:
            os.makedirs(marker_dir, exist_ok=True)
        except Exception:
            pass

        fresh = []
        for d in docs:
            key = re.sub(r"[^A-Za-z0-9._-]", "_", session + "-" + d)
            marker = os.path.join(marker_dir, key)
            if os.path.exists(marker):
                continue
            try:
                open(marker, "w").close()
            except Exception:
                pass
            fresh.append(d)

        if not fresh:
            return

        print(json.dumps({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": (
                    "Reference docs relevant to this action - read them this session "
                    "if you have not yet: " + ", ".join(fresh)
                ),
            }
        }))
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
