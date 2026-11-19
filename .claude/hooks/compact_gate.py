#!/usr/bin/env python
"""PreCompact gate: auto-compaction waits for a step boundary.

The auto-compact window is set in .claude/settings.json (autoCompactWindow). On an auto trigger:
  - a subagent's own compaction is blocked: subagents end long work with a part hand-off
    (context_budget_guard.py), not a summary of half a step;
  - the main session's compaction is blocked while any subagent of the session is still working
    (no finished flag, transcript written in the last ACTIVE_SECONDS), so it fires at the first
    turn past the window after the step's agents have returned.
A blocked proactive compaction is skipped and retried on the next turn past the window. When not
blocking, stdout is added to the summary instructions (manual /compact too). Any error exits 0 with
no output, so compaction is never blocked by a hook bug.
"""
import glob
import json
import os
import sys
import time

ACTIVE_SECONDS = 20 * 60

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
