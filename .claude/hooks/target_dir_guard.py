#!/usr/bin/env python
"""PreToolUse(Bash|PowerShell) guard: denies a private cargo target directory
(`CARGO_[BUILD_]TARGET_DIR` set in the shell, `cargo ... --target-dir` or
`--config build.target-dir=...`); `cargo clean --target-dir` stays allowed.
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

VARS = r"CARGO_(?:BUILD_)?TARGET_DIR"
ASSIGNMENTS = r"(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*"
WRAPPERS = re.compile(
    r"^(?:[({]\s*|(?:time|sudo|nice|command|exec|env(?:\s+-\S+(?:\s+[A-Za-z_]\w*(?=\s))?)*)\s+)+")
SET_VAR = re.compile(
    r"^(?:export\s+|set\s+|declare\s+-\S+\s+|typeset\s+-\S+\s+)?" + ASSIGNMENTS + VARS + r"\s*=",
    re.I)
PS_SET_VAR = re.compile(
    r"^(?:\$env:" + VARS + r"|\$\{env:" + VARS + r"\})\s*="
    r"|^(?:set-item|si|new-item|ni)\s+(?:-path\s+)?['\"]?env:[/\\]?" + VARS + r"\b"
    r"|setenvironmentvariable\(\s*['\"]" + VARS + r"['\"]", re.I)
CARGO = re.compile(
    ASSIGNMENTS + r"[\"'&]?\s*(?:\S*[/\\])?(?:cargo|rustup)(?:\.exe)?[\"']?\s+(\S+)?(.*)$", re.I)
CARGO_TARGET_FLAG = re.compile(
    r"(?:^|\s)--target-dir(?:[=\s]|$)|build\.target-dir\s*=", re.I)
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
        text = WRAPPERS.sub("", segment.strip())
        if SET_VAR.search(text) or PS_SET_VAR.search(text):
            return REASON
        cargo = CARGO.match(text)
        if not cargo or (cargo.group(1) or "").lower() == "clean":
            continue
        else:
            args = re.split(r"\s--(?:\s|$)", " " + (cargo.group(1) or "") + cargo.group(2), 1)[0]
            if CARGO_TARGET_FLAG.search(args):
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
