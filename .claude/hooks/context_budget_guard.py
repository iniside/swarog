#!/usr/bin/env python
"""Context budget guard for subagents (implementation-dispatch skill, Prompts and hand-offs;
the Long steps section of each agent definition).

Modes (argv[1]):
  send  PreToolUse SendMessage   main session: deny resuming a FINISHED subagent whose context is
                                 over SEND_LIMIT tokens; dispatch a fresh agent with the punch list
  work  PreToolUse (every tool)  subagent: over SOFT_LIMIT deny new exploration (Read, Grep, Glob,
                                 LSP, Agent, web); over HARD_LIMIT deny everything but git, REPL,
                                 Poll and the hand-off, so the agent commits a part and returns

Context is the last API turn's input + cache tokens in the subagent transcript. Any error exits 0
with no output, so a hook bug never blocks work.
"""
import json
import os
import re
import sys

SEND_LIMIT = 100_000
SOFT_LIMIT = 180_000
HARD_LIMIT = 240_000
TAIL_BYTES = 1_000_000
AGENT_ID = re.compile(r"^a[0-9a-f]{8,}$")
EXPLORE = {"Read", "Grep", "Glob", "LSP", "Agent", "WebFetch", "WebSearch"}
ALWAYS = {"REPL", "Poll", "SubagentHandback", "SendMessage"}
GIT_ONLY = re.compile(r"^\s*(?:cd\s+\S+\s*(?:&&|;)\s*)?git\s+(?:-C\s+\S+\s+)?(?:status|diff|add|commit|log|show|rev-parse)\b")

SEND_REASON = (
    "Context budget: subagent {to} already holds {ctx}k tokens of context, and every turn of a resumed "
    "agent re-reads all of it. Dispatch a FRESH agent of the same lane instead: give it the punch list or "
    "decision, the step's commit sha(s) and the files to touch (implementation-dispatch skill, Prompts and "
    "hand-offs). Resume by SendMessage only below {limit}k."
)
SOFT_REASON = (
    "Context budget: you hold {ctx}k tokens; past {soft}k no new exploration ({tool}). Finish the edit in "
    "hand, reach a coherent point, commit it as a part and return a part hand-off (your agent definition, "
    "Long steps). The main session continues in a fresh agent. Past {hard}k only git and the hand-off run."
)
HARD_REASON = (
    "Context budget: you hold {ctx}k tokens (limit {hard}k). Only git status/diff/add/commit/log/show and "
    "the hand-off run now. Commit what is coherent as a part (your agent definition, Long steps) and "
    "return a part hand-off naming what remains and the build/test state."
)


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


def deny(reason):
    print(json.dumps({"hookSpecificOutput": {
        "hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}}))


def do_send(payload):
    if payload.get("agent_id"):
        return
    to = str((payload.get("tool_input") or {}).get("to") or "").strip()
    if not AGENT_ID.match(to):
        return
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
    if tool in ALWAYS:
        return
    ctx = last_context(subagent_file(payload.get("transcript_path"), agent_id))
    if not ctx or ctx <= SOFT_LIMIT:
        return
    values = dict(ctx=ctx // 1000, soft=SOFT_LIMIT // 1000, hard=HARD_LIMIT // 1000, tool=tool)
    if ctx > HARD_LIMIT:
        command = str((payload.get("tool_input") or {}).get("command") or "")
        if tool in ("Bash", "PowerShell") and GIT_ONLY.match(command):
            return
        deny(HARD_REASON.format(**values))
    elif tool in EXPLORE:
        deny(SOFT_REASON.format(**values))


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
