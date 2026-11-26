#!/usr/bin/env python
"""PreToolUse(Bash|PowerShell) guard: denies a private cargo target directory
(`CARGO_TARGET_DIR=...`, `export`/`set`/`$env:` of it, or `cargo ... --target-dir`).
Every such directory is a full rebuild of the workspace that nobody deletes.
Override: put `force-target-dir` in the command. Any internal error exits 0
with no output.
"""
import json
import os
import re
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
try:
    import shell_data
except Exception:
    shell_data = None

SHELL_TOOLS = {"Bash", "PowerShell"}

ASSIGNMENTS = r"(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*"
SET_VAR = re.compile(
    r"^(?:env\s+|export\s+|set\s+)?" + ASSIGNMENTS + r"CARGO_TARGET_DIR\s*=", re.I)
PS_SET_VAR = re.compile(r"^\$env:CARGO_TARGET_DIR\s*=", re.I)
CARGO_FLAG = re.compile(
    r"^(?:env\s+)?" + ASSIGNMENTS + r"[\"'&]?\s*(?:\S*[/\\])?cargo(?:\.exe)?[\"']?\s.*?"
    r"(?:^|\s)--target-dir(?:[=\s]|$)", re.I)

REASON = (
    "A private cargo target directory is a full rebuild of the workspace that nobody "
    "deletes; these piled up to 250+ GB under target/. Use the shared target/ and "
    "serialize instead: one rollout at a time "
    "(.agents/skills/safe-verification/SKILL.md). Owner-approved exception? Re-send "
    "with `force-target-dir` in the command."
)


def verdict(command):
    if "force-target-dir" in command.lower():
        return None
    for segment, _ in shell_data.split_commands(command):
        text = segment.strip()
        if SET_VAR.search(text) or PS_SET_VAR.search(text) or CARGO_FLAG.search(text):
            return REASON
    return None


def main():
    try:
        data = json.load(sys.stdin)
        tool = data.get("tool_name") or ""
        if tool and tool not in SHELL_TOOLS:
            return
        command = (data.get("tool_input") or {}).get("command") or ""
        if shell_data is None or not isinstance(command, str):
            return
        reason = verdict(command)
        if not reason:
            return
        print(json.dumps({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        }))
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
