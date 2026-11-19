#!/usr/bin/env python
"""Context budget guard for subagents (implementation-dispatch skill, Prompts and hand-offs;
the Long steps section of each agent definition).

Modes (argv[1]):
  send  PreToolUse SendMessage   main session: deny resuming a FINISHED subagent (by id or name)
                                 whose context is over SEND_LIMIT tokens; dispatch a fresh agent
  work  PreToolUse (every tool)  subagent over WORK_LIMIT: deny exploration (Read, Grep, Glob, LSP,
                                 Agent, web, and shell readers such as rg/cat/Get-Content); Edit,
                                 Write, cargo and git still run, so the agent finishes the edit in
                                 hand, checks the build, commits a part and returns a hand-off

Context is the last API turn's input + cache tokens in the subagent transcript. Any error exits 0
with no output, so a hook bug never blocks work.
"""
import json
import os
import re
import sys

SEND_LIMIT = 100_000
WORK_LIMIT = 180_000
TAIL_BYTES = 1_000_000
AGENT_ID = re.compile(r"^a[0-9a-f]{8,}$")
EXPLORE = {"Read", "Grep", "Glob", "LSP", "Agent", "WebFetch", "WebSearch", "NotebookRead"}
SHELLS = {"Bash", "PowerShell"}
SHELL_READERS = {
    "rg", "grep", "egrep", "fgrep", "cat", "sed", "awk", "head", "tail", "less", "more", "find", "ls",
    "tree", "wc", "get-content", "gc", "type", "select-string", "sls", "get-childitem", "gci", "dir",
}
SEGMENT_SPLIT = re.compile(r"&&|\|\||[;|&\n]")
PREFIX_WORDS = {"cd", "set-location", "sl", "pushd", "time", "env", "sudo"}

SEND_REASON = (
    "Context budget: subagent {to} already holds {ctx}k tokens of context, and every turn of a resumed "
    "agent re-reads all of it. Dispatch a FRESH agent of the same lane instead: give it the punch list or "
    "decision, the step's commit sha(s) and the files to touch (implementation-dispatch skill, Prompts and "
    "hand-offs). Resume by SendMessage only below {limit}k."
)
WORK_REASON = (
    "Context budget: you hold {ctx}k tokens; past {limit}k no new reading or search ({what}). Finish the edit "
    "in hand, run the check or test it needs, commit a coherent part and return a part hand-off (your agent "
    "definition, Long steps). Edit, Write, cargo and git still run. The main session continues in a fresh agent."
)


def shell_reader(command):
    for segment in SEGMENT_SPLIT.split(command or ""):
        words = segment.strip().split()
        while words and (words[0].lower() in PREFIX_WORDS or "=" in words[0]):
            words = words[2:] if words[0].lower() in {"cd", "set-location", "sl", "pushd"} else words[1:]
        if words:
            head = re.split(r"[\\/]", words[0].strip("'\""))[-1].lower()
            if head.endswith(".exe"):
                head = head[:-4]
            if head in SHELL_READERS:
                return head
    return None


def last_context(path):
    if not path or not os.path.isfile(path):
        return None
    with open(path, "rb") as f:
        f.seek(0, os.SEEK_END)
        size = f.tell()
        f.seek(max(0, size - TAIL_BYTES))
        lines = f.read().decode("utf-8", "replace").splitlines()
    for line in reversed(lines):
        if '"usage"' not in line:
            continue
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        message = entry.get("message") or {}
        if entry.get("type") != "assistant" or message.get("model") == "<synthetic>":
            continue
        usage = message.get("usage") or {}
        total = sum(int(usage.get(k) or 0) for k in (
            "input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"))
        if total:
            return total
    return None


def session_dir(transcript_path):
    path = str(transcript_path or "")
    cut = path.replace("\\", "/").rfind("/subagents/")
    if cut >= 0:
        return path[:cut]
    return path[:-6] if path.endswith(".jsonl") else None


def subagent_file(transcript_path, agent_id):
    base = session_dir(transcript_path)
    if not base or not agent_id:
        return None
    return os.path.join(base, "subagents", "agent-" + agent_id + ".jsonl")


def finished(path):
    try:
        with open(path[:-6] + ".meta.json", encoding="utf-8") as f:
            return bool(json.load(f).get("finished"))
    except (OSError, ValueError):
        return False


def resolve_target(transcript_path, to):
    to = re.sub(r"\s*\[[^\]]*\]\s*$", "", str(to or "")).strip()
    if AGENT_ID.match(to):
        return to
    base = session_dir(transcript_path)
    if not to or not base:
        return None
    folder = os.path.join(base, "subagents")
    found = None
    for name in sorted(os.listdir(folder)) if os.path.isdir(folder) else ():
        if not name.endswith(".meta.json"):
            continue
        try:
            with open(os.path.join(folder, name), encoding="utf-8") as f:
                meta = json.load(f)
        except (OSError, ValueError):
            continue
        if meta.get("name") == to:
            found = name[len("agent-"):-len(".meta.json")]
    return found


def deny(reason):
    print(json.dumps({"hookSpecificOutput": {
        "hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}}))


def do_send(payload):
    if payload.get("agent_id"):
        return
    to = resolve_target(payload.get("transcript_path"), (payload.get("tool_input") or {}).get("to"))
    path = subagent_file(payload.get("transcript_path"), to)
    if not path or not finished(path):
        return
    ctx = last_context(path)
    if ctx and ctx > SEND_LIMIT:
        deny(SEND_REASON.format(to=to, ctx=ctx // 1000, limit=SEND_LIMIT // 1000))


def do_work(payload):
    agent_id = payload.get("agent_id")
    if not agent_id:
        return
    tool = str(payload.get("tool_name") or "")
    if tool in EXPLORE:
        what = tool
    elif tool in SHELLS:
        what = shell_reader(str((payload.get("tool_input") or {}).get("command") or ""))
        what = what and "{0} via {1}".format(what, tool)
    else:
        return
    if not what:
        return
    ctx = last_context(subagent_file(payload.get("transcript_path"), agent_id))
    if ctx and ctx > WORK_LIMIT:
        deny(WORK_REASON.format(ctx=ctx // 1000, limit=WORK_LIMIT // 1000, what=what))


def main():
    try:
        payload = json.loads(sys.stdin.buffer.read().decode("utf-8", "replace"))
        if not isinstance(payload, dict):
            return
        mode = sys.argv[1] if len(sys.argv) > 1 else ""
        if mode == "send":
            do_send(payload)
        elif mode == "work":
            do_work(payload)
    except Exception:
        if os.environ.get("CONTEXT_BUDGET_DEBUG") == "1":
            raise


if __name__ == "__main__":
    main()
    sys.exit(0)
