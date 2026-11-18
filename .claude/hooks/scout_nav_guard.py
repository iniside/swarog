"""Navigation guard: LSP before a wide Rust search; for code-scout also LSP evidence behind its anchors.

Modes (argv[1]):
  record  PostToolUse LSP|Agent, PostToolUseFailure LSP   append the LSP call and its file:line hits, or a
                                        code-scout dispatch, to state
  gate    PreToolUse Grep|Bash          code-scout: deny a wide Rust search before a position-based LSP call;
                                        core-implementer, test-author, docs-writer: deny it before such an
                                        LSP call or a code-scout dispatch (a Bash search carrying '# nav-ok'
                                        passes), and deny a third sed/head/tail slice of one file (all capped)
  report  PreToolUse SubagentHandback   code-scout: deny a report whose anchors lack the LSP evidence their
                                        label claims; the other agents: drop their state
Every other agent and the main session are no-ops.

Rules: .claude/agents/code-scout.md and the implementation lanes' .claude/agents/<name>.md;
.agents/shared/research-navigation.md Navigation.
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
RUST_TYPES = {"rust", "rs"}
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

NAV_AGENTS = {"core-implementer", "test-author", "docs-writer"}
RAW_MARKERS = ('"code-scout"',) + tuple('"{0}"'.format(name) for name in sorted(NAV_AGENTS))
SLICE_LIMIT = 2

NAV_REASON = (
    "{agent}: this Rust search spans the repo root or a bare core, modules, api, cmd, tools or demos. Ask LSP "
    "first: workspaceSymbol for the NAME position, then findReferences, goToImplementation or incomingCalls at it; "
    "or dispatch code-scout with the one question (subagent_type \"code-scout\", model \"haiku\", effort \"medium\"; "
    "model \"sonnet\" for a multi-hop question), it returns only anchors. A grep in the file or crate folder you "
    "work in is allowed. A deleted symbol or a plain-text search: run it as a Bash rg or grep with the comment "
    "'# nav-ok'. .claude/agents/{agent}.md; .agents/shared/research-navigation.md Navigation."
)

SLICE_REASON = (
    "{agent}: {file} was already sliced {count} times with sed -n/head/tail; read the file once with the Read tool "
    "(whole file, or offset/limit around the anchor) instead of slicing it again. .claude/agents/{agent}.md."
)

SEPARATORS = ("&&", ";", "||", "|", "&")
REDIRECTS = {">", ">>", "<", "<<", ">&", "&>", "&>>", ">|", "<&"}
WRITE_REDIRECTS = {">", ">>", ">&", "&>", "&>>", ">|"}
OUTPUT_DIRS = {"target", "run", "deploy", "bin", "dist"}
# Short flags that take a value, per tool; rg's -E/-r take one, grep's do not.
VALUE_SHORT = {"rg": set("efgtTABCmjMrEd"), "grep": set("efABCmdD"), "git": set("efABCm")}
VALUE_LONG = {
    "--regexp", "--file", "--glob", "--iglob", "--type", "--type-not", "--type-add", "--max-count",
    "--context", "--after-context", "--before-context", "--max-depth", "--replace", "--encoding",
    "--threads", "--max-columns", "--sort", "--sortr", "--color", "--colors", "--max-filesize",
    "--pre", "--pre-glob", "--path-separator", "--context-separator", "--field-match-separator",
    "--ignore-file", "--include", "--exclude", "--exclude-dir", "--label", "--devices", "--directories",
}
FILTER_SHORT = {"g": "glob", "t": "type"}
FILTER_LONG = {"--glob": "glob", "--iglob": "glob", "--type": "type", "--include": "glob"}


def setup():
    global fnmatch, json, os, posixpath, re, shlex, tempfile
    global PATH_LINE, HEADER, LINE_ENTRY, CALLS_AT, CONFIRMED, GREP_ONLY, ANCHORS_HEAD, SECTION_HEAD, TAIL_HEAD
    global NAV_ESCAPE, SED_RANGE, LINE_COUNT
    import fnmatch
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
    NAV_ESCAPE = re.compile(r"#\s*nav-ok\b")
    SED_RANGE = re.compile(r"^(?:\d+|\$)(?:\s*,\s*(?:[+~]?\d+|\$))?\s*p$")
    LINE_COUNT = re.compile(r"^(?:-n\s*[+-]?\d+|-\d+|--lines=[+-]?\d+)$")


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


def tokens(command, sinks=None):
    lexer = shlex.shlex(command, posix=False, punctuation_chars=True)
    lexer.whitespace_split = True
    words, skip = [], None
    for word in lexer:
        if skip is not None:
            if skip and sinks is not None:
                sinks.append(word.strip("\"'"))
            skip = None
            continue
        if word in REDIRECTS:
            if words and words[-1].isdigit():
                words.pop()
            skip = word in WRITE_REDIRECTS
            continue
        words.append(word)
    return words


def head_name(word):
    name = word.strip("\"'").replace("\\", "/").split("/")[-1].lower()
    return name[:-4] if name.endswith(".exe") else name


def segments(command, sinks=None):
    """Yield (words, piped) per pipeline/sequence segment."""
    current, piped = [], False
    for word in tokens(command, sinks):
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


def slice_targets(command, cwd):
    """Return the repository source file of every `sed -n '<range>p' <file>` and `head|tail -n N <file>` segment."""
    found, sinks, cwds = [], [], [cwd]
    for words, _ in segments(command, sinks):
        head = head_name(words[0])
        if head == "cd":
            if len(words) > 1:
                cwd = resolve(words[1].strip("\"'"), cwd)
                cwds.append(cwd)
            continue
        args = [word.strip("\"'") for word in words[1:]]
        target = sed_file(args) if head == "sed" else head_file(args) if head in ("head", "tail") else None
        if target:
            found.append(norm(resolve(target, cwd)))
    written = {norm(resolve(sink, where)) for sink in sinks for where in cwds}
    return [target for target in found if target not in written and repo_source(target)]


def repo_source(target):
    if not target or target.startswith("/") or re.match(r"^[a-z]:", target) or target.startswith(".."):
        return False
    return target.split("/")[0] not in OUTPUT_DIRS


def sed_file(args):
    quiet, script, files, index = False, None, [], 0
    while index < len(args):
        arg = args[index]
        if arg in ("-n", "--quiet", "--silent"):
            quiet = True
        elif arg in ("-e", "--expression") and index + 1 < len(args) and script is None:
            script = args[index + 1]
            index += 1
        elif arg.startswith("-"):
            return None
        elif script is None:
            script = arg
        else:
            files.append(arg)
        index += 1
    return files[0] if quiet and script and SED_RANGE.match(script.strip()) and len(files) == 1 else None


def head_file(args):
    counted, files, index = False, [], 0
    while index < len(args):
        arg = args[index]
        if arg == "-n" and index + 1 < len(args):
            counted = True
            index += 1
        elif LINE_COUNT.match(arg):
            counted = True
        elif arg.startswith("-"):
            return None
        else:
            files.append(arg)
        index += 1
    return files[0] if counted and len(files) == 1 else None


def parse_args(tool, args):
    """Return (paths, [(kind, value)]) of a grep invocation."""
    positionals, filters, explicit_pattern = [], [], False
    after_dashdash, take = False, None
    for arg in args:
        if take is not None:
            if take:
                filters.append((take, arg.strip("\"'")))
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
                filters.append((FILTER_LONG[name], value.strip("\"'")))
            if name in VALUE_LONG and not value:
                take = FILTER_LONG.get(name, "")
            continue
        flags = arg[1:]
        for index, flag in enumerate(flags):
            if flag in ("e", "f"):
                explicit_pattern = True
            if flag in VALUE_SHORT[tool]:
                kind = FILTER_SHORT.get(flag, "") if tool == "rg" else ""
                if index == len(flags) - 1:
                    take = kind
                elif kind:
                    filters.append((kind, flags[index + 1:].strip("\"'")))
                break
    paths = positionals if explicit_pattern else positionals[1:]
    return [p.strip("\"'") for p in paths], filters


def ext_of(text):
    match = re.search(r"\.([A-Za-z0-9]+)\W*$", text)
    return match.group(1).lower() if match else None


def brace_expand(text):
    match = re.search(r"\{([^{}]*)\}", text)
    if not match:
        return [text]
    expanded = []
    for option in match.group(1).split(","):
        expanded.extend(brace_expand(text[:match.start()] + option + text[match.end():]))
    return expanded


def rust_glob(value):
    for option in brace_expand(value.lower()):
        last = option.replace("\\", "/").rstrip("/").split("/")[-1]
        literal = re.match(r"[^*?\[]*", last).group(0)
        if literal == last:
            if last.endswith(".rs"):
                return True
            continue
        probes = ("x.rs", literal + "x.rs", literal + ".rs", literal + "rs", literal + "s")
        if any(fnmatch.fnmatchcase(probe, last) for probe in probes):
            return True
    return False


def rust_filter(kind, value):
    value = value.strip("\"'")
    if kind == "type":
        return value.lower() in RUST_TYPES
    return rust_glob(value)


def rust_search(filters):
    positive = [(k, v) for k, v in filters if not (k == "glob" and v.strip("\"'").startswith("!"))]
    return not positive or any(rust_filter(k, v) for k, v in positive)


def git_pathspec(spec):
    """Return (exclude, top, path) of a git pathspec."""
    if not spec.startswith(":"):
        return False, False, spec
    if spec.startswith(":("):
        magic, _, rest = spec[2:].partition(")")
        words = {word.strip().split(":")[0].lower() for word in magic.split(",")}
        return "exclude" in words, "top" in words, rest
    signature = re.match(r":([/!^]*):?", spec)
    return bool(set(signature.group(1)) & set("!^")), "/" in signature.group(1), spec[signature.end():]


def literal_prefix(path):
    parts = []
    for part in path.split("/"):
        if re.search(r"[*?\[]", part):
            break
        parts.append(part)
    return "/".join(parts) or "/"


def holds_repo(scoped):
    return scoped in ("/", "") or re.match(r"^[a-z]:/?$", scoped) is not None or REPO_ROOT.startswith(scoped.rstrip("/") + "/")


def is_wide(paths, cwd, git=False):
    judged = []
    for path in paths:
        if git and path.startswith(":"):
            exclude, top, rest = git_pathspec(path)
            if not exclude:
                judged.append((resolve(rest or ".", DEFAULT_CWD if top else cwd), False))
        else:
            judged.append((path, True))
    for path, shortcut in judged or [(".", True)]:
        full = resolve(path, cwd)
        if os.path.isfile(full):
            continue
        if shortcut and ext_of(path) not in (None, "rs") and not os.path.isdir(full):
            continue
        scoped = norm(literal_prefix(full))
        if scoped in WIDE_DIRS or holds_repo(scoped):
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
    elif tool == "Agent" and str(tool_input.get("subagent_type") or "") == "code-scout" and not failed:
        append_state(path, {"kind": "scout"})


def wide_search(payload):
    tool = payload.get("tool_name")
    tool_input = payload.get("tool_input") or {}
    cwd = payload.get("cwd") or DEFAULT_CWD
    if tool == "Grep":
        target = tool_input.get("path")
        paths = [str(target)] if target else []
        filters = [(k, str(tool_input[k])) for k in ("glob", "type") if tool_input.get(k)]
        return rust_search(filters) and is_wide(paths, cwd)
    if tool == "Bash":
        for kind, args, run_cwd, piped in grep_invocations(str(tool_input.get("command") or ""), cwd):
            if kind == "rg" and "--files" in args:
                continue
            paths, filters = parse_args(kind, args)
            if piped and not paths:
                continue
            if rust_search(filters) and is_wide(paths, run_cwd, kind == "git"):
                return True
    return False


def gate_denials(records):
    return sum(1 for r in records if r.get("kind") == "gate-deny")


def do_gate(payload, path):
    if not wide_search(payload):
        return
    records = load_state(path)
    if has_position_lsp(records):
        return
    if gate_denials(records) >= DENY_CAP:
        return
    append_state(path, {"kind": "gate-deny"})
    emit({"permissionDecision": "deny", "permissionDecisionReason": GATE_REASON})


def do_nav_gate(payload, path, agent):
    command = str((payload.get("tool_input") or {}).get("command") or "") if payload.get("tool_name") == "Bash" else ""
    records = load_state(path)
    if not (command and NAV_ESCAPE.search(command)) and wide_search(payload):
        opened = has_position_lsp(records) or any(r.get("kind") == "scout" for r in records)
        if not opened and gate_denials(records) < DENY_CAP:
            append_state(path, {"kind": "gate-deny"})
            emit({"permissionDecision": "deny", "permissionDecisionReason": NAV_REASON.format(agent=agent)})
            return
    if not command:
        return
    targets = slice_targets(command, payload.get("cwd") or DEFAULT_CWD)
    if not targets:
        return
    counts = {}
    for record in records:
        if record.get("kind") == "slice":
            counts[record.get("file")] = counts.get(record.get("file"), 0) + 1
    for target in targets:
        if counts.get(target, 0) >= SLICE_LIMIT:
            if sum(1 for r in records if r.get("kind") == "slice-deny") < DENY_CAP:
                append_state(path, {"kind": "slice-deny", "file": target})
                reason = SLICE_REASON.format(agent=agent, file=target, count=counts[target])
                emit({"permissionDecision": "deny", "permissionDecisionReason": reason})
                return
            break
        counts[target] = counts.get(target, 0) + 1
    for target in targets:
        append_state(path, {"kind": "slice", "file": target})


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
        if not any(marker in raw for marker in RAW_MARKERS):
            return
        setup()
        payload = json.loads(raw)
        if not isinstance(payload, dict) or not payload.get("agent_id"):
            return
        agent = payload.get("agent_type")
        if agent != "code-scout" and agent not in NAV_AGENTS:
            return
        mode = sys.argv[1] if len(sys.argv) > 1 else ""
        path = state_path(payload.get("agent_id"))
        if agent in NAV_AGENTS:
            if mode == "record":
                do_record(payload, path)
            elif mode == "gate":
                do_nav_gate(payload, path, agent)
            elif mode == "report" and os.path.isfile(path):
                os.remove(path)
            return
        if mode == "record":
            do_record(payload, path)
        elif mode == "gate":
            do_gate(payload, path)
        elif mode == "report":
            do_report(payload, path)
    except Exception:
        from os import environ

        if environ.get("SCOUT_NAV_DEBUG") == "1":
            raise


if __name__ == "__main__":
    main()
    sys.exit(0)
