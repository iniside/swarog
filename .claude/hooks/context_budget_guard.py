#!/usr/bin/env python
"""Context budget guard for subagents (implementation-dispatch skill, Prompts and hand-offs;
the Long steps section of each agent definition).

Modes (argv[1]):
  send  PreToolUse SendMessage   main session: deny resuming a FINISHED subagent (by agent id)
                                 whose context is over SEND_LIMIT tokens; dispatch a fresh agent
  work  PreToolUse (every tool)  subagent over WORK_LIMIT: deny exploration (Read, Grep, Glob, LSP,
                                 Agent, web, shell readers such as rg/cat/Get-Content, and REPL
                                 scripts that read: file(), tools.Read/Grep/Glob/LSP/WebFetch/
                                 WebSearch, tools.Agent, or a $`...` command that starts with a
                                 reader); Edit, Write, edit(), write(), cargo and git still run, so
                                 the agent finishes the edit in hand, checks the build, commits a
                                 part and returns a hand-off. A read-only lane (READ_ONLY_AGENTS) is
                                 told to return its report with a 'Not checked:' list instead.
                                 Best effort: git show/grep and readers hidden in python -c,
                                 bash -c, xargs or tools.Bash inside REPL still read; a $(...)
                                 call counts when its first argument is a literal (or a variable
                                 assigned one) that starts with a reader

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
READ_ONLY_AGENTS = {"general-purpose", "Explore", "Plan", "code-scout", "core-reviewer", "proof-auditor"}
REPL_READ = re.compile(
    r"(?<![\w.])file\s*\(|\btools\s*\.\s*(?:Read|Grep|Glob|LSP|WebFetch|WebSearch|NotebookRead|Agent)\b")
REPL_SHELL_CALL = re.compile(r"(?<![\w.])\$\s*\(")
REPL_SHELL = re.compile(r"(?<![\w.])\$\s*`((?:\\.|\$\{[^}]*\}|[^`\\])*)`", re.S)
REPL_SHELL_LITERAL = re.compile(r"(?<![\w.])\$\s*\(\s*(['\"`])((?:\\.|(?!\1).)*)\1", re.S)
REGEX_PREV = set("(,=:[!&|?{};+-*%<>~^")
SHELLS = {"Bash", "PowerShell"}
SHELL_READERS = {
    "rg", "grep", "egrep", "fgrep", "cat", "sed", "awk", "head", "tail", "less", "more", "find", "ls",
    "tree", "wc", "get-content", "gc", "type", "select-string", "sls", "get-childitem", "gci", "dir",
}
SEGMENT_SPLIT = re.compile(r"(&&|\|\||[;|&\n])")
HEREDOC = re.compile(r"<<-?\s*(['\"]?)(\w+)\1[^\n]*\n.*?\n\s*\2\s*(?=\n|$)", re.S)
QUOTED = re.compile(r"'[^']*'|\"(?:\\.|[^\"\\])*\"|@'.*?'@|@\".*?\"@", re.S)
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
    "definition, Long steps). Edit, Write, edit(), write(), cargo and git still run. The main session continues "
    "in a fresh agent."
)
READ_ONLY_REASON = (
    "Context budget: you hold {ctx}k tokens; past {limit}k no new reading or search ({what}). Return your report "
    "now: the findings you have, each with its anchors, and a 'Not checked:' list of what you did not reach, so the "
    "main session can send it to a fresh agent."
)


def shell_reader(command):
    """Best effort: the first word of each unquoted command segment. Heredoc bodies and quoted text
    are dropped first, and a reader fed by a pipe (cargo test | tail) is a filter, not a read."""
    text = QUOTED.sub("''", HEREDOC.sub("", command or ""))
    pieces = SEGMENT_SPLIT.split(text)
    separator = None
    for index in range(0, len(pieces), 2):
        words = pieces[index].strip().split()
        piped = separator == "|"
        separator = pieces[index + 1] if index + 1 < len(pieces) else None
        while words and (words[0].lower() in PREFIX_WORDS or "=" in words[0]):
            words = words[2:] if words[0].lower() in {"cd", "set-location", "sl", "pushd"} else words[1:]
        if not words or piped:
            continue
        head = re.split(r"[\\/]", words[0].strip("'\""))[-1].lower()
        if head.endswith(".exe"):
            head = head[:-4]
        if head in SHELL_READERS:
            return head
    return None


def scan_literals(code):
    """Spans (start, end, kind, text) of '...', "..." and `...` literals ("command" for a $`...` command, with
    each ${...} as X in its text), // and /* */ comments and /.../ regex literals. Literals inside a ${...}
    are spans too. None when a literal, comment or ${...} does not close."""
    spans = []
    n = len(code)

    def template(i):
        j, out = i + 1, []
        while j < n:
            c = code[j]
            if c == "\\":
                out.append(code[j:j + 2])
                j += 2
            elif c == "`":
                return j, "".join(out)
            elif c == "$" and code[j + 1:j + 2] == "{":
                j = scan(j + 2, True)
                if j is None:
                    return None
                out.append("X")
            else:
                out.append(c)
                j += 1
        return None

    def scan(i, nested):
        depth, prev = 0, ""
        while i < n:
            ch = code[i]
            nxt = code[i + 1] if i + 1 < n else ""
            if ch == "/" and nxt == "/":
                j = code.find("\n", i)
                j = n if j < 0 else j
                spans.append((i, j, "comment", ""))
                i = j
                continue
            if ch == "/" and nxt == "*":
                j = code.find("*/", i + 2)
                if j < 0:
                    return None
                spans.append((i, j + 2, "comment", ""))
                i = j + 2
                continue
            if ch == "/" and (prev == "" or prev in REGEX_PREV or code[max(0, i - 6):i].rstrip().endswith("return")):
                j, in_class = i + 1, False
                while j < n and code[j] != "\n" and (code[j] != "/" or in_class):
                    if code[j] == "\\":
                        j += 1
                    elif code[j] == "[":
                        in_class = True
                    elif code[j] == "]":
                        in_class = False
                    j += 1
                if j >= n or code[j] != "/":
                    return None
                spans.append((i + 1, j, "regex", ""))
                i = j + 1
                prev = "/"
                continue
            if ch == "`":
                found = template(i)
                if found is None:
                    return None
                j, text = found
                command = re.search(r"(?<![\w.])\$\s*$", code[max(0, i - 8):i])
                spans.append((i + 1, j, "command" if command else "string", text))
                i = j + 1
                prev = ch
                continue
            if ch in "'\"":
                j = i + 1
                while j < n and code[j] != ch and code[j] != "\n":
                    j += 2 if code[j] == "\\" else 1
                if j >= n or code[j] != ch:
                    return None
                spans.append((i + 1, j, "string", code[i + 1:j]))
                i = j + 1
                prev = ch
                continue
            if nested and ch == "{":
                depth += 1
            elif nested and ch == "}":
                if depth == 0:
                    return i + 1
                depth -= 1
            if not ch.isspace():
                prev = ch
            i += 1
        return None if nested else n

    return spans if scan(0, False) is not None else None


def mask_strings(code, spans):
    """Blank the given spans (string, command, comment and regex text, string arguments of edit() and
    write() among them), keeping offsets, so text that only mentions file( or rg is not a call."""
    chars = list(code)
    for start, end, _kind, _text in spans:
        for k in range(start, end):
            if chars[k] != "\n":
                chars[k] = " "
    return "".join(chars)


def shell_call_literals(masked, literals):
    """The literal text each $(...) call runs: its first argument when that is a literal, or the literal a
    const/let/var of that name is assigned."""
    for m in REPL_SHELL_CALL.finditer(masked):
        k = m.end()
        while k < len(masked) and masked[k].isspace():
            k += 1
        if k in literals:
            yield literals[k]
            continue
        name = re.match(r"[A-Za-z_$][\w$]*", masked[k:])
        if not name:
            continue
        for d in re.finditer(r"(?<![\w.$])(?:const|let|var)\s+" + re.escape(name.group(0)) + r"\s*=\s*", masked):
            if d.end() in literals:
                yield literals[d.end()]


def repl_reader(code):
    """Best effort: a REPL script reads when it calls file(), a reading tool or tools.Agent, or runs a
    $`...` command or a $(...) call whose first argument (a literal, or a variable assigned one) has a
    segment that starts with a reader. A script that does not parse has only its commands checked."""
    spans = scan_literals(code)
    if spans is None:
        commands = [m.group(1) for m in REPL_SHELL.finditer(code)]
        commands += [m.group(2) for m in REPL_SHELL_LITERAL.finditer(code)]
    else:
        masked = mask_strings(code, spans)
        found = REPL_READ.search(masked)
        if found:
            return "tools.Agent" if found.group(0).endswith("Agent") else "file() or a reading tool"
        commands = [text for _start, _end, kind, text in spans if kind == "command"]
        literals = {start - 1: text for start, _end, kind, text in spans if kind in ("string", "command")}
        commands += list(shell_call_literals(masked, literals))
    for command in commands:
        reader = shell_reader(re.sub(r"\$\{[^}]*\}", "X", command))
        if reader:
            return reader
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


def resolve_target(to):
    """Only raw agent ids are checked; Claude Code records no agent name in .meta.json."""
    to = re.sub(r"\s*\[[^\]]*\]\s*$", "", str(to or "")).strip()
    return to if AGENT_ID.match(to) else None


def deny(reason):
    print(json.dumps({"hookSpecificOutput": {
        "hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}}))


def do_send(payload):
    if payload.get("agent_id"):
        return
    to = resolve_target((payload.get("tool_input") or {}).get("to"))
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
    tool_input = payload.get("tool_input") or {}
    if tool in EXPLORE:
        what = tool
    elif tool == "REPL":
        what = repl_reader(str(tool_input.get("code") or ""))
        what = what and "{0} in REPL".format(what)
    elif tool in SHELLS:
        what = shell_reader(str(tool_input.get("command") or ""))
        what = what and "{0} via {1}".format(what, tool)
    else:
        return
    if not what:
        return
    ctx = last_context(subagent_file(payload.get("transcript_path"), agent_id))
    if ctx and ctx > WORK_LIMIT:
        reason = READ_ONLY_REASON if payload.get("agent_type") in READ_ONLY_AGENTS else WORK_REASON
        deny(reason.format(ctx=ctx // 1000, limit=WORK_LIMIT // 1000, what=what))


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
