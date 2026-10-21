#!/usr/bin/env python
"""PreToolUse(Bash|PowerShell) guard: denies shell grep/find sweeps that bypass
.gitignore (recursive grep, unbounded find, `rg --no-ignore`) or reach a build
artifact tree (target/, run/, deploy/, __pycache__/), including PowerShell
Select-String / Get-ChildItem -Recurse into those trees. Override: put
`force-grep` in the command. Any internal error exits 0 with no output.
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

ARTIFACT_DIRS = ("target", "run", "deploy", "__pycache__")
ARTIFACT_ALT = "|".join(re.escape(d) for d in ARTIFACT_DIRS)
ARTIFACT_PATH = re.compile(r"(?:[/\\]|^|\s|['\"])(?:" + ARTIFACT_ALT + r")[/\\]")
ARTIFACT_TOKEN = re.compile(
    r"(?:^|\s|,)['\"]?(?:\.[/\\])?(" + ARTIFACT_ALT + r")['\"]?(?=\s|$|,)")
PS_VALUE_PARAMS = re.compile(
    r"-(?:pattern|filter|include|exclude|simplematch|encoding)\s+"
    r"(?:'[^']*'|\"[^\"]*\"|\S+)")

SWEEP_TOOLS = {"grep", "egrep", "fgrep", "find", "ack", "ag"}
PS_SEARCH_TOOLS = {"select-string", "sls"}
PS_LIST_TOOLS = {"get-childitem", "gci", "dir", "ls"}
RECURSIVE_FLAGS = re.compile(
    r"(?:^|\s)(?:--(?:recursive|dereference-recursive)|-(?!-)[a-zA-Z]*[rR][a-zA-Z]*)(?:\s|$)"
)
PS_RECURSE = re.compile(r"(?:^|\s)-r(?:e(?:c(?:u(?:r(?:se?)?)?)?)?)?(?:\s|$|:)")

REDIRECT = (
    "Use LSP / rust-analyzer first, then the Grep tool (ripgrep — it honours "
    ".gitignore, so target/ and run/ are excluded); narrow with its `glob`/`type` "
    "params. A grep sweep is a labelled lower bound, never the answer "
    "(docs/reference/research-mode.md). Genuinely need the raw shell sweep? "
    "Re-send with `force-grep` in the command."
)


def first_token(segment):
    match = re.match(r"[\"'&]?\s*([^\s\"']+)", segment)
    if not match:
        return ""
    token = re.split(r"[/\\]", match.group(1))[-1].lower()
    return token[:-4] if token.endswith(".exe") else token


def artifact_hit(low, bare_tokens):
    hit = ARTIFACT_PATH.search(low)
    if hit:
        return hit.group(0).strip("/\\ '\"")
    if bare_tokens:
        hit = ARTIFACT_TOKEN.search(PS_VALUE_PARAMS.sub(" ", low))
        if hit:
            return hit.group(1)
    return None


def artifact_reason(name):
    return (
        "This shell sweep reaches a build-artifact tree ({0}/). Hits there are stale "
        "generated files or runtime state and prove nothing about live source. {1}"
    ).format(name, REDIRECT)


def verdict(command):
    if "force-grep" in command.lower():
        return None

    for segment, preceded_by_pipe in shell_data.split_commands(command):
        tool = first_token(segment)
        low = segment.lower()

        if preceded_by_pipe and (tool in SWEEP_TOOLS or tool in PS_SEARCH_TOOLS):
            continue

        if tool == "rg":
            if re.search(r"(^|\s)(--no-ignore\S*|-u|-uu|-uuu)(\s|$)", low):
                return ("rg with --no-ignore/-u disables the .gitignore filtering that "
                        "keeps target/ and run/ out of results. " + REDIRECT)
            continue

        if tool in PS_SEARCH_TOOLS:
            name = artifact_hit(low, True)
            if name:
                return artifact_reason(name)
            continue

        if tool in PS_LIST_TOOLS:
            if PS_RECURSE.search(low):
                name = artifact_hit(low, True)
                if name:
                    return artifact_reason(name)
            continue

        if tool not in SWEEP_TOOLS:
            continue

        name = artifact_hit(low, False)
        if name:
            return artifact_reason(name)

        is_recursive_grep = tool != "find" and RECURSIVE_FLAGS.search(segment)
        is_find_sweep = tool == "find" and not re.search(r"(^|\s)-maxdepth\s", low)
        if is_recursive_grep or is_find_sweep:
            return ("Recursive shell sweeps bypass .gitignore, so they silently include "
                    "target/, run/ and deploy/. " + REDIRECT)

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
