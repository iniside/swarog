#!/usr/bin/env python
"""PreCompact gate: auto-compaction waits for a step boundary.

The auto-compact window is set in .claude/settings.json (autoCompactWindow). On an auto trigger:
  - a subagent's own compaction is blocked: subagents end long work with a part hand-off
    (context_budget_guard.py), not a summary of half a step;
  - the main session's compaction is blocked while any subagent of the session is still working
    (no finished flag, transcript written in the last ACTIVE_SECONDS), so it fires at the first
    turn past the window after the step's agents have returned.
A blocked proactive compaction is skipped and retried on the next turn past the window. Nothing is
blocked at CEILING tokens or above, or when the context size is unknown: a blocked recovery
compaction (after a real context-limit error) would fail the request. When not
blocking, stdout is added to the summary instructions (manual /compact too). Any error exits 0 with
no output, so compaction is never blocked by a hook bug.
"""
import glob
import json
import os
import sys
import time

ACTIVE_SECONDS = 60 * 60
CEILING = 400_000
TAIL_BYTES = 1_000_000

SUMMARY_INSTRUCTIONS = (
    "Keep, verbatim where short: the owner's decisions and rulings with their dates; the active plan "
    "file and the current and next step ids; commit shas of the steps landed this session; every open "
    "punch list or leftover and its origin (local/structural); the ids, lanes and tasks of subagents "
    "still running or awaiting review; any rule the owner restated in this conversation."
)


def session_dir(transcript_path):
    path = str(transcript_path or "")
    cut = path.replace("\\", "/").rfind("/subagents/")
    if cut >= 0:
        return path[:cut]
    return path[:-6] if path.endswith(".jsonl") else None


def last_context(path):
    if not path or not os.path.isfile(path):
        return None
    with open(path, "rb") as f:
        f.seek(0, os.SEEK_END)
        f.seek(max(0, f.tell() - TAIL_BYTES))
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


def own_transcript(payload):
    path = str(payload.get("transcript_path") or "")
    agent_id = payload.get("agent_id")
    if agent_id and "/subagents/" not in path.replace("\\", "/"):
        base = session_dir(path)
        return os.path.join(base, "subagents", "agent-" + str(agent_id) + ".jsonl") if base else None
    return path


def is_subagent(payload):
    return bool(payload.get("agent_id")) or "/subagents/" in str(payload.get("transcript_path") or "").replace("\\", "/")


def running_subagents(base, now=None):
    now = time.time() if now is None else now
    found = []
    for meta_path in glob.glob(os.path.join(glob.escape(base), "subagents", "*.meta.json")):
        try:
            with open(meta_path, encoding="utf-8") as f:
                meta = json.load(f)
        except (OSError, ValueError):
            continue
        if meta.get("finished"):
            continue
        transcript = meta_path[:-len(".meta.json")] + ".jsonl"
        try:
            fresh = now - os.path.getmtime(transcript) < ACTIVE_SECONDS
        except OSError:
            continue
        if fresh:
            found.append(meta.get("description") or os.path.basename(transcript))
    return found


def block(reason):
    print(json.dumps({"decision": "block", "reason": reason}))


def main():
    try:
        payload = json.loads(sys.stdin.buffer.read().decode("utf-8", "replace"))
        if not isinstance(payload, dict):
            return
        auto = payload.get("trigger") == "auto"
        if auto:
            ctx = last_context(own_transcript(payload))
            auto = bool(ctx) and ctx < CEILING
        if is_subagent(payload):
            if auto:
                block("subagent: no auto-compaction; finish with a part hand-off past the context budget")
            return
        if auto:
            base = session_dir(payload.get("transcript_path"))
            busy = running_subagents(base) if base else []
            if busy:
                block("waiting for the step boundary; subagents still working: " + ", ".join(busy[:5]))
                return
        print(SUMMARY_INSTRUCTIONS)
    except Exception:
        if os.environ.get("COMPACT_GATE_DEBUG") == "1":
            raise


if __name__ == "__main__":
    main()
    sys.exit(0)
