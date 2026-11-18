"""code-scout navigation guard: LSP before a wide grep, LSP evidence behind every confirmed anchor.

Modes (argv[1]), all no-ops unless agent_type == "code-scout":
  record  PostToolUse LSP, PostToolUseFailure LSP   append the LSP call and its file:line hits to state
  gate    PreToolUse Grep|Bash          deny a wide Rust search before a position-based LSP call (capped)
  report  PreToolUse SubagentHandback   deny a report whose anchors lack the LSP evidence their label claims

Rules: .claude/agents/code-scout.md; .agents/shared/research-navigation.md Navigation.
"""

import sys

POSITION_OPS = {
    "findReferences", "incomingCalls", "outgoingCalls", "goToImplementation",
    "hover", "goToDefinition", "prepareCallHierarchy",
}
CONFIRM_OPS = {"goToDefinition", "hover", "findReferences", "incomingCalls", "goToImplementation"}
COVER_OPS = CONFIRM_OPS | {"workspaceSymbol"}
RUST_EXT = (".rs",)
WIDE_DIRS = {"", ".", "core", "modules", "api", "cmd", "tools", "demos"}
NO_RUST_DIRS = {
    "docs", "uilayout", "memory", ".agents", ".claude", ".github", ".grok", "clients", "experiments", "scripts",
    "bin", "deploy", "run", "target", "nocommit", "mutants.out",
}
MIN_SCOPED_DEPTH = 2
LINE_SLACK = 2
DENY_CAP = 2
REPO_PREFIX = "g:/projects/swarog/"
REPO_ROOT = "g:/projects/swarog"
DEFAULT_CWD = "G:/Projects/swarog"

GATE_REASON = (
    "code-scout: make a position-based LSP call (findReferences, goToImplementation, incomingCalls or hover "
    "at the symbol, position from workspaceSymbol) before a wider search; a grep in the file or crate folder "
    "you expect it in (modules/<name>, api/<name>, core/<crate>, weles, opscatalog) is allowed, not one over the "
    "repo root or a bare core, modules, api, cmd, tools or demos. .claude/agents/code-scout.md; "
    ".agents/shared/research-navigation.md Navigation."
)

SEPARATORS = ("&&", ";", "||", "|", "&")
REDIRECTS = {">", ">>", "<", "<<", ">&", "&>", "&>>", ">|", "<&"}
# Short flags that take a value, per tool; rg's -E/-r take one, grep's do not.
VALUE_SHORT = {"rg": set("efgtTABCmjMrEd"), "grep": set("efABCmdD"), "git": set("efABCm")}
VALUE_LONG = {
    "--regexp", "--file", "--glob", "--iglob", "--type", "--type-not", "--type-add", "--max-count",
    "--context", "--after-context", "--before-context", "--max-depth", "--replace", "--encoding",
    "--threads", "--max-columns", "--sort", "--sortr", "--color", "--colors", "--max-filesize",
    "--pre", "--pre-glob", "--path-separator", "--context-separator", "--field-match-separator",
    "--ignore-file", "--include", "--exclude", "--exclude-dir", "--label", "--devices", "--directories",
}
FILTER_SHORT = {"g", "t"}
FILTER_LONG = {"--glob", "--iglob", "--type", "--include"}


def setup():
    global json, os, posixpath, re, shlex, tempfile
    global PATH_LINE, HEADER, LINE_ENTRY, CALLS_AT, CONFIRMED, GREP_ONLY, ANCHORS_HEAD, SECTION_HEAD, TAIL_HEAD
    import json
    import os
    import posixpath
    import re
    import shlex
    import tempfile

    PATH_LINE = re.compile(r"((?:[A-Za-z]:)?[^\s:\"'<>|`*(),;\[\]]+\.[A-Za-z0-9_]+):(\d+)")
    HEADER = re.compile(r"^\s*((?:[A-Za-z]:)?[^\r\n]*?\.[A-Za-z0-9_]+):\s*$")
    LINE_ENTRY = re.compile(r"\bLine\s+(\d+)")
    CALLS_AT = re.compile(r"\[calls at:([^\]]*)\]", re.I)
    CONFIRMED = re.compile(r"(?<!not )(?<!un)\bconfirmed\b", re.I)
    GREP_ONLY = re.compile(r"grep[- ]only", re.I)
    ANCHORS_HEAD = re.compile(r"^\W*(?:\d+\.\s*)?\W*anchors\b", re.I)
    SECTION_HEAD = re.compile(r"^\W*(?:\d+\.\s*)?\W*(?:method|open|answer)\b", re.I)
    TAIL_HEAD = re.compile(r"^\W*(?:\d+\.\s*)?\W*(?:method|open)\b", re.I)


def drive_path(path):
    path = path.strip().strip("\"'").replace("\\", "/")
    match = re.match(r"^/([a-zA-Z])/", path)
    return match.group(1) + ":/" + path[3:] if match else path


def norm(path):
    p = drive_path(path)
    p = posixpath.normpath(p).lower() if p else p
    if p == REPO_ROOT:
        return ""
    if p.startswith(REPO_PREFIX):
        p = p[len(REPO_PREFIX):]
    return p


def resolve(path, cwd):
    path = drive_path(path)
    if not (path.startswith("/") or re.match(r"^[A-Za-z]:", path)):
        path = drive_path(cwd).rstrip("/") + "/" + path
    return path


def same_file(a, b):
    return bool(a) and bool(b) and (a == b or a.endswith("/" + b) or b.endswith("/" + a))


def near(a, b):
    return a is not None and b is not None and abs(a - b) <= LINE_SLACK


def state_path(agent_id):
    agent = re.sub(r"[^A-Za-z0-9_.-]", "_", str(agent_id))
    folder = os.path.join(tempfile.gettempdir(), "scout-nav")
    os.makedirs(folder, exist_ok=True)
    return os.path.join(folder, agent + ".jsonl")


def load_state(path):
    records = []
    if os.path.isfile(path):
        with open(path, encoding="utf-8") as handle:
            for raw in handle:
                raw = raw.strip()
                if raw:
                    try:
                        records.append(json.loads(raw))
                    except ValueError:
                        pass
    return records


def append_state(path, record):
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(record) + "\n")


def emit(output):
    print(json.dumps({"hookSpecificOutput": dict({"hookEventName": "PreToolUse"}, **output)}))


def lsp_hits(text):
    hits = []
    current = None
    for line in text.splitlines():
        header = HEADER.match(line)
        if header:
            current = norm(header.group(1))
            continue
        if current:
            entry = LINE_ENTRY.search(line)
            if entry:
                hits.append([current, int(entry.group(1))])
            calls = CALLS_AT.search(line)
            if calls:
                for number in re.findall(r"(\d+):\d+", calls.group(1)):
                    hits.append([current, int(number)])
        for path, number in PATH_LINE.findall(line):
            hits.append([norm(path), int(number)])
    return hits


def tokens(command):
    lexer = shlex.shlex(command, posix=False, punctuation_chars=True)
    lexer.whitespace_split = True
    words, skip = [], False
    for word in lexer:
        if skip:
            skip = False
            continue
        if word in REDIRECTS:
            if words and words[-1].isdigit():
                words.pop()
            skip = True
            continue
        words.append(word)
    return words


def head_name(word):
    name = word.strip("\"'").replace("\\", "/").split("/")[-1].lower()
    return name[:-4] if name.endswith(".exe") else name


def segments(command):
    """Yield (words, piped) per pipeline/sequence segment."""
    current, piped = [], False
    for word in tokens(command):
        if word in SEPARATORS:
            if current:
                yield current, piped
            current, piped = [], word == "|"
        else:
            current.append(word)
    if current:
        yield current, piped


def grep_invocations(command, cwd):
    """Return [(tool, args, cwd, piped)] for every rg/grep/git grep segment."""
    found = []
    for words, piped in segments(command):
        head = head_name(words[0])
        if head == "cd":
            if len(words) > 1:
                cwd = resolve(words[1].strip("\"'"), cwd)
            continue
        if head in ("rg", "grep", "egrep", "fgrep"):
            found.append(("rg" if head == "rg" else "grep", words[1:], cwd, piped))
        elif head == "git":
            index, run_cwd = 1, cwd
            while index < len(words) and words[index].startswith("-"):
                if words[index] in ("-C", "-c") and index + 1 < len(words):
                    if words[index] == "-C":
                        run_cwd = resolve(words[index + 1].strip("\"'"), cwd)
                    index += 2
                else:
                    index += 1
            if index < len(words) and words[index] == "grep":
                found.append(("git", words[index + 1:], run_cwd, piped))
    return found


def parse_args(tool, args):
    """Return (paths, filters) of a grep invocation."""
    positionals, filters, explicit_pattern = [], [], False
    after_dashdash, take = False, None
    for arg in args:
        if take is not None:
            if take:
                filters.append(arg.strip("\"'"))
            take = None
            continue
        if after_dashdash or not arg.startswith("-") or arg == "-":
            positionals.append(arg)
            continue
        if arg == "--":
            after_dashdash = True
            continue
        if arg.startswith("--"):
            name, _, value = arg.partition("=")
            if name in ("--regexp", "--file"):
                explicit_pattern = True
            if name in FILTER_LONG and value:
                filters.append(value.strip("\"'"))
            if name in VALUE_LONG and not value:
                take = name in FILTER_LONG
            continue
        flags = arg[1:]
        for index, flag in enumerate(flags):
            if flag in ("e", "f"):
                explicit_pattern = True
            if flag in VALUE_SHORT[tool]:
                is_filter = tool == "rg" and flag in FILTER_SHORT
                if index == len(flags) - 1:
                    take = is_filter
                elif is_filter:
                    filters.append(flags[index + 1:].strip("\"'"))
                break
    paths = positionals if explicit_pattern else positionals[1:]
    return [p.strip("\"'") for p in paths], filters


def ext_of(text):
    match = re.search(r"\.([A-Za-z0-9]+)\W*$", text)
    return match.group(1).lower() if match else None


def rust_filter(value):
    value = value.strip("\"'").lower()
    if re.search(r"(?<![a-z0-9])rs(?![a-z0-9])|rust", value):
        return True
    last = value.replace("\\", "/").rstrip("/").split("/")[-1]
    return "*" in last and not re.search(r"\.[a-z0-9{\[]", last)


def rust_search(filters):
    positive = [f for f in filters if not f.strip("\"'").startswith("!")]
    return not positive or any(rust_filter(f) for f in positive)


def is_wide(paths, cwd):
    for path in paths or ["."]:
        full = resolve(path, cwd)
        if os.path.isfile(full):
            continue
        if ext_of(path) not in (None, "rs") and not os.path.isdir(full):
            continue
        scoped = norm(full)
        if scoped.rstrip("/").split("/")[-1] in NO_RUST_DIRS:
            continue
        if scoped in WIDE_DIRS:
            return True
        depth = len([part for part in re.sub(r"^[a-z]:", "", scoped).split("/") if part])
        if depth == 1 and os.path.isfile(os.path.join(full, "Cargo.toml")):
            continue
        if depth < MIN_SCOPED_DEPTH:
            return True
    return False


def has_position_lsp(records):
    return any(r.get("kind") == "lsp" and r.get("op") in POSITION_OPS for r in records)


def do_record(payload, path):
    tool = payload.get("tool_name")
    tool_input = payload.get("tool_input") or {}
    failed = payload.get("hook_event_name") == "PostToolUseFailure"
    response = None if failed else payload.get("tool_response")
    if tool == "LSP":
        if isinstance(response, dict):
            text = str(response.get("result") or "")
            count = response.get("resultCount")
        else:
            text, count = str(response or ""), None
        ok = count > 0 if isinstance(count, (int, float)) else bool(text.strip()) and not re.match(r"\s*no\b", text, re.I)
        append_state(path, {
            "kind": "lsp", "op": tool_input.get("operation"), "file": norm(str(tool_input.get("filePath") or "")),
            "line": tool_input.get("line"), "ok": bool(ok and not failed), "hits": lsp_hits(text),
        })


def do_gate(payload, path):
    tool = payload.get("tool_name")
    tool_input = payload.get("tool_input") or {}
    cwd = payload.get("cwd") or DEFAULT_CWD
    wide = False
    if tool == "Grep":
        target = tool_input.get("path")
        paths = [str(target)] if target else []
        filters = [str(tool_input[k]) for k in ("glob", "type") if tool_input.get(k)]
        wide = rust_search(filters) and is_wide(paths, cwd)
    elif tool == "Bash":
        for kind, args, run_cwd, piped in grep_invocations(str(tool_input.get("command") or ""), cwd):
            if kind == "rg" and "--files" in args:
                continue
            paths, filters = parse_args(kind, args)
            if piped and not paths:
                continue
            if rust_search(filters) and is_wide(paths, run_cwd):
                wide = True
    if not wide:
        return
    records = load_state(path)
    if has_position_lsp(records):
        return
    if sum(1 for r in records if r.get("kind") == "gate-deny") >= DENY_CAP:
        return
    append_state(path, {"kind": "gate-deny"})
    emit({"permissionDecision": "deny", "permissionDecisionReason": GATE_REASON})


def anchor_lines(message):
    lines = message.splitlines()
    start = next((i for i, line in enumerate(lines) if ANCHORS_HEAD.match(line)), None)
    if start is None:
        end = next((i for i, line in enumerate(lines) if TAIL_HEAD.match(line)), len(lines))
        return [line for line in lines[:end] if re.match(r"^\s*[-*\u2022]\s", line)]
    section = []
    for line in lines[start:]:
        if section and SECTION_HEAD.match(line):
            break
        section.append(line)
    return section


def parse_anchors(message):
    anchors = []
    for line in anchor_lines(message):
        found = PATH_LINE.search(line)
        if not found:
            continue
        label = "grep-only" if GREP_ONLY.search(line) else "confirmed" if CONFIRMED.search(line) else None
        anchors.append({"raw": found.group(1), "file": norm(found.group(1)), "line": int(found.group(2)), "label": label})
    return anchors


def lsp_covers(anchor, lsp_calls):
    for call in lsp_calls:
        if not call.get("ok") or call.get("op") not in COVER_OPS:
            continue
        if call.get("op") in CONFIRM_OPS and same_file(call.get("file") or "", anchor["file"]) and near(call.get("line"), anchor["line"]):
            return True
        for hit_file, hit_line in call.get("hits") or ():
            if same_file(hit_file, anchor["file"]) and near(hit_line, anchor["line"]):
                return True
    return False


def violations(message, records):
    lsp_calls = [r for r in records if r.get("kind") == "lsp"]
    found = []
    for anchor in parse_anchors(message):
        if lsp_covers(anchor, lsp_calls):
            continue
        where = "{0}:{1}".format(anchor["raw"].replace("\\", "/"), anchor["line"])
        if anchor["label"] == "confirmed":
            found.append(where + " labelled confirmed but no LSP call covers it: run goToDefinition at the hit or relabel grep-only")
        elif anchor["label"] is None and anchor["file"].endswith(RUST_EXT):
            found.append(where + " has no label: label confirmed or grep-only")
    return found


def do_report(payload, path):
    tool_input = payload.get("tool_input") or {}
    message = str(tool_input.get("message") or "")
    records = load_state(path)
    found = violations(message, records)
    if not found:
        if os.path.isfile(path):
            os.remove(path)
        return
    denials = sum(1 for r in records if r.get("kind") == "deny")
    if denials < DENY_CAP:
        append_state(path, {"kind": "deny"})
        emit({
            "permissionDecision": "deny",
            "permissionDecisionReason": "code-scout report anchors lack LSP evidence (.claude/agents/code-scout.md What you return):\n- "
            + "\n- ".join(found),
        })
        return
    updated = dict(tool_input)
    updated["message"] = message.rstrip() + "\n\n**Hook:** unverified anchors: " + "; ".join(found)
    if os.path.isfile(path):
        os.remove(path)
    emit({"permissionDecision": "allow", "updatedInput": updated})


def main():
    try:
        raw = sys.stdin.buffer.read().decode("utf-8", "replace")
        if '"code-scout"' not in raw:
            return
        setup()
        payload = json.loads(raw)
        if not isinstance(payload, dict) or payload.get("agent_type") != "code-scout" or not payload.get("agent_id"):
            return
        mode = sys.argv[1] if len(sys.argv) > 1 else ""
        path = state_path(payload["agent_id"])
        if mode == "record":
            do_record(payload, path)
        elif mode == "gate":
            do_gate(payload, path)
        elif mode == "report":
            do_report(payload, path)
    except Exception:
        pass


if __name__ == "__main__":
    main()
    sys.exit(0)
