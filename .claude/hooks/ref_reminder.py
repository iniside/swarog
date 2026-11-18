#!/usr/bin/env python
"""PreToolUse(Bash|PowerShell): injects a once-per-session reminder to read the
safety skill and platform notes when a rollout command is run. Markers live under
%TEMP%/claude/ref-hook keyed by session id + doc. Any internal error exits 0
with no output.
"""
import json
import os
import re
import sys

SHELL_TOOLS = {"Bash", "PowerShell"}

SAFE_VERIFY_SKILL = ".agents/skills/safe-verification/SKILL.md"
PLATFORM_NOTES = "docs/reference/platform-notes.md"

ROLLOUT_COMMAND = re.compile(
    r"\bcargo(?:\.exe)?\s+(?:\+\S+\s+)?test\b|\bverifyctl\b|\bdevctl(?:\.exe)?\s+(?:--\s+)?up\b",
    re.IGNORECASE,
)


def main():
    try:
        data = json.load(sys.stdin)
        tool = data.get("tool_name") or ""
        session = str(data.get("session_id") or "nosession")
        tool_input = data.get("tool_input") or {}

        docs = []
        if tool in SHELL_TOOLS:
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
