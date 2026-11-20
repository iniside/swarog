"""View guard: a whole-file view of a long source file is denied and answered with its outline.

PreToolUse Read|Bash|PowerShell|REPL. For core-implementer, test-author, docs-writer, mockup-implementer,
code-scout, core-reviewer, proof-auditor, general-purpose and the built-in Explore and Plan:
  - Read of a .rs, .py, .md, .js, .css, .html, .tmpl, .toml, .txt or .log file longer than LINE_CAP lines
    without a 'limit'. A Read with offset and no limit that starts within LINE_CAP lines of the end is a
    small view.
  - Bash/PowerShell cat/type/Get-Content/gc of such a file with no pipe in the command (a pipe into head,
    sed -n, rg or a script cuts the output) and no -TotalCount/-Head/-Tail, and sed -n ranges and head/tail
    counts wider than RANGE_CAP lines; segments come from shell_segments.py (quotes, here-docs, comments,
    launchers).
  - A call a REPL script made (a repl_ tool_use_id: a $ command as Bash, file(X).text() as a Read without
    limit) is judged by the script that resolved exactly that command or file (making_script, check_repl);
    one no script resolved, and every direct call, gets the checks above.
  - REPL: a whole long file that reaches the script's output (the last statement's value or a log()
    argument): file(X).text()/.lines(), file(X).slice(a, n) with n over RANGE_CAP, or a $`...` command
    check_bash denies, followed through methods that keep the whole text, literals, templates and names;
    X is resolved from string literals and the names this script or the agent's earlier REPL scripts
    assigned (unresolved: allowed). A value kept in a name and returned by a later script is not seen.
    file(X).slice(offset) and tools.Read with an offset and no limit follow the Read tail rule.
  - REPL budget: the file text one script returns (file().slice/text/lines, tools.Read, sed -n ranges,
    (Get-Content f)[a..b], viewers, head/tail counts, (await file(X).lines()/.text()).slice(a, b) with literal
    bounds, and calls of the script's own range helpers on a resolved file) adds up to at most SCRIPT_CAP lines; preview(x, n) with n over PREVIEW_CAP counts as
    uncut.
Plans under docs/plans/ are exempt everywhere (read whole by design).
The denial carries the file's outline (Rust: fn, impl, struct, enum, trait, mod, macro_rules with line
ranges; Markdown: headings with section ranges; Python and JS: def/class/function lines; TOML: tables;
the rest: a pointer to rg), so the next call reads only the range it needs. Escape: a Read with a limit,
or 'view-ok' in the Bash command or REPL script; a second whole read of the same file under 'view-ok' by
one agent is denied (the first copy is still in its context). Every other agent and the main session are
no-ops.

Rules: .agents/skills/code-navigation/SKILL.md Search budget.
"""

import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import shell_segments  # noqa: E402

AGENTS = {"core-implementer", "test-author", "docs-writer", "mockup-implementer", "code-scout", "core-reviewer",
          "proof-auditor", "general-purpose", "Explore", "Plan"}  # Explore and Plan: built-in research lanes
EXEMPT = set()  # lanes with Read, Bash, PowerShell or REPL that are deliberately left unguarded
RS_EXT = (".rs",)
PY_EXT = (".py",)
MD_EXT = (".md",)
JS_EXT = (".js",)
TOML_EXT = (".toml",)
OTHER_EXT = (".css", ".html", ".tmpl")
LOG_EXT = (".txt", ".log")
VIEW_EXT = RS_EXT + PY_EXT + MD_EXT + JS_EXT + TOML_EXT + OTHER_EXT + LOG_EXT
SHELLS = {"Bash", "PowerShell"}
LINE_CAP = 300
RANGE_CAP = 300
OUTLINE_CAP = 150
ESCAPE = "view-ok"
SCRIPT_CAP = 300  # lines of file text one REPL script may return in total
PREVIEW_CAP = 4000  # preview(x, n) with n above this carries the text like an uncut value
VIEWERS = {"cat", "type", "get-content", "gc", "less", "more", "bat"}
DEFAULT_CWD = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))).replace("\\", "/")

REASON = (
    "{path} has {lines} lines; a whole-file view stays in your context for the rest of the task "
    "(.agents/skills/code-navigation/SKILL.md Search budget). Read only the range you need: Read with offset "
    "and limit (about 100 lines; a Rust item or a Markdown section by its line range in the outline below). "
    "For one Rust symbol, LSP documentSymbol on the file or goToDefinition/hover at its name gives its exact "
    "position. If you need most of the file, Read it with an explicit offset 1 and limit. Outline:\n{outline}"
)


def line_count(path):
    try:
        with open(path, "rb") as f:
            data = f.read()
        return data.count(b"\n") + (0 if data.endswith(b"\n") or not data else 1)
    except OSError:
        return 0


def resolve(path, cwd):
    path = path.strip().strip("\"'").replace("\\", "/")
    m = re.match(r"^/([a-zA-Z])/", path)
    if m:
        path = m.group(1) + ":/" + path[3:]
    if not (path.startswith("/") or re.match(r"^[A-Za-z]:", path)):
        path = (cwd or DEFAULT_CWD).replace("\\", "/").rstrip("/") + "/" + path
    return path


def is_plan(path):
    return "/docs/plans/" in "/" + (path or "").replace("\\", "/")


def long_file(path, cwd):
    p = resolve(path, cwd)
    if not p.lower().endswith(VIEW_EXT) or is_plan(p) or not os.path.isfile(p):
        return None
    n = line_count(p)
    return (p, n) if n > LINE_CAP else None


def read_lines(path):
    with open(path, encoding="utf-8-sig", errors="replace") as f:
        return f.read().split("\n")


def capped(rows, what):
    if len(rows) > OUTLINE_CAP:
        rows = rows[:OUTLINE_CAP] + [f"  … {len(rows) - OUTLINE_CAP} more {what}"]
    return rows


def md_outline(path):
    rows, fence = [], False
    lines = read_lines(path)
    heads = []
    for k, line in enumerate(lines, 1):
        if line.lstrip().startswith(("```", "~~~")):
            fence = not fence
        elif not fence and re.match(r"^#{1,6}\s", line):
            heads.append((k, len(line) - len(line.lstrip("#")), line.strip()))
    for idx, (k, level, text) in enumerate(heads):
        end = next((h[0] - 1 for h in heads[idx + 1:] if h[1] <= level), len(lines))
        rows.append(f"{k:>6}-{end:<6} {text}")
    return f"{path} ({len(lines)} lines): heading, first-last line of its section\n" + "\n".join(capped(rows, "headings"))


RS_CHAR = re.compile(r"b?'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^\\'\n])'")
RS_RAW = re.compile(r"b?r(#*)\"")


def rs_mask(src):
    """Comment, string and char literal bodies blanked (newlines kept), so braces in them do not count."""
    out, i, n = list(src), 0, len(src)

    def blank(a, b):
        for k in range(a, b):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        c = src[i]
        if src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            blank(i, j)
            i = j
        elif src.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif src.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            blank(i, j)
            i = j
        elif c in "rb" and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")) and RS_RAW.match(src, i):
            m = RS_RAW.match(src, i)
            end = src.find("\"" + m.group(1), m.end())
            j = n if end < 0 else end + 1 + len(m.group(1))
            blank(m.end(), max(m.end(), j - 1 - len(m.group(1))))
            i = j
        elif c == "\"":
            j = i + 1
            while j < n and src[j] != "\"":
                j += 2 if src[j] == "\\" else 1
            blank(i + 1, min(j, n))
            i = j + 1
        elif c == "'":
            m = RS_CHAR.match(src, i)
            if m:
                blank(i + 1, m.end() - 1)
                i = m.end()
            else:
                i += 1  # a lifetime
        else:
            i += 1
    return "".join(out)


RS_ITEM = re.compile(
    r"^\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\s*\([^)]*\))?\s+)?(?:default\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?"
    r"(?:extern\s+(?:\"[^\"]*\"\s+)?)?(fn|struct|enum|union|trait|impl|mod|macro_rules!)(?![\w])")
LEAF = {"fn", "macro_rules!"}  # items inside these bodies are not listed


def rs_items(path):
    """[(start, end, depth, label)] for the Rust items of a file, from a brace scan of its masked text."""
    with open(path, encoding="utf-8-sig", errors="replace") as f:
        src = f.read()
    masked = rs_mask(src).split("\n")
    raw = src.split("\n")
    items, stack, pending, depth, nest = [], [], None, 0, 0
    for k, line in enumerate(masked, 1):
        if pending is not None and k - items[pending[0]][0] > 40:
            pending = None  # a signature that never opened a body nor ended with ';'
        if pending is None and not any(s[2] in LEAF for s in stack):
            m = RS_ITEM.match(line)
            if m:
                label = raw[k - 1].strip()
                cut = line.find("{")
                if cut >= 0:
                    label = raw[k - 1][:cut].strip()
                label = re.sub(r"^(?:#\[[^\]]*\]\s*)+", "", label).rstrip(" {")
                label = label if len(label) <= 110 else label[:107] + "..."
                pending = [len(items), depth, m.group(1), nest]
                items.append([k, k, len(stack), label])
        for c in line:
            if c in "([":
                nest += 1
            elif c in ")]":
                nest = max(0, nest - 1)
            elif c == "{":
                if pending is not None and nest == pending[3]:
                    stack.append((pending[0], depth, pending[2]))
                    pending = None
                depth += 1
            elif c == "}":
                depth = max(0, depth - 1)
                if stack and stack[-1][1] == depth:
                    items[stack.pop()[0]][1] = k
            elif c == ";" and pending is not None and nest == pending[3] and depth == pending[1]:
                items[pending[0]][1] = k  # struct X; mod x; fn decl();
                pending = None
    return [tuple(i) for i in items]


def rs_outline(path):
    items = rs_items(path)
    n = line_count(path)
    rows = [f"{a:>6}-{b:<6} {'  ' * d}{label}" for a, b, d, label in items]
    return f"{path} ({n} lines): item, first-last line\n" + "\n".join(capped(rows, "items"))


def line_outline(path, pattern, what):
    rows = [f"{k:>6} {line.rstrip()[:120]}" for k, line in enumerate(read_lines(path), 1) if re.match(pattern, line)]
    return path + ": line of each " + what + "\n" + "\n".join(capped(rows, what))


JS_DEF = (r"\s*(?:export\s+(?:default\s+)?)?(?:(?:async\s+)?function\b|class\s"
          r"|(?:const|let|var)\s+[A-Za-z_$][\w$]*\s*=\s*(?:async\s+)?(?:function\b|\([^)]*\)\s*=>|[A-Za-z_$][\w$]*\s*=>))")


def outline(path):
    low = path.lower()
    try:
        if low.endswith(RS_EXT):
            return rs_outline(path)
        if low.endswith(MD_EXT):
            return md_outline(path)
        if low.endswith(PY_EXT):
            return line_outline(path, r"\s*(?:async\s+)?(?:def|class)\s", "def/class")
        if low.endswith(JS_EXT):
            return line_outline(path, JS_DEF, "function/class")
        if low.endswith(TOML_EXT):
            return line_outline(path, r"\s*\[", "table")
    except Exception as e:  # the denial still names the file
        return "(outline unavailable: " + type(e).__name__ + ")"
    return ("(no outline for this file type: list what you need with 'rg -n <pattern> <file>' or the Grep tool, "
            "then Read that range with offset and limit)")


REPL_HINT = (
    " In a REPL script: file(X).slice(offset, limit) with a limit up to {cap}, or cut the text inside the "
    "script (.split, .match, a line range) and return only that; put 'view-ok' in the script when the "
    "whole text is only processed and never returned.\n"
)


def deny(path, lines, repl=False):
    reason = REASON.format(path=path, lines=lines, outline=outline(path))
    if repl:
        head, sep, rest = reason.partition("Outline:\n")
        reason = head + REPL_HINT.format(cap=RANGE_CAP) + sep + rest
    print(json.dumps({"hookSpecificOutput": {
        "hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}}))


def deny_budget(total, infos):
    parts = "; ".join(f"{i[2]}: {i[1]}" for i in infos[:10])
    reason = (
        f"This REPL script returns about {total} lines of file text ({parts}); one script returns at most "
        f"{SCRIPT_CAP} (code-navigation SKILL.md Search budget), because every line stays in your context for the rest "
        "of the task. Return only what the next decision needs: narrow each range to the function or block "
        "(LSP documentSymbol on a .rs file gives each item's line range), find anchors with rg -n first, or cut in the script and return preview(x, n) with n up to "
        f"{PREVIEW_CAP}. Put 'view-ok' in the script when you need all of it."
    )
    print(json.dumps({"hookSpecificOutput": {
        "hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}}))


def tail_lines(total, offset):
    """Lines a view from the 1-based offset to the end shows, or None when that is a whole view of a long file."""
    count = max(total - offset + 1, 0)
    return count if total <= LINE_CAP or total - offset < LINE_CAP else None


def check_read(inp, cwd):
    """A direct Read; a Read a REPL script made (file(X).text()) is judged by its script in main."""
    if inp.get("limit"):
        return None
    path = inp.get("file_path") or ""
    hit = long_file(path, cwd) if path else None
    try:
        offset = int(inp.get("offset") or 0)
    except (TypeError, ValueError):
        offset = 0
    if hit and offset and tail_lines(hit[1], offset) is not None:
        return None  # the tail of the file, a small view
    return hit


def files_in(tokens):
    """File arguments of a viewer: flags and redirect targets dropped."""
    out, skip = [], False
    for w in tokens[1:]:
        if skip:
            skip = False
            continue
        if re.match(r"^\d*(>>?|<)&?\d*$", w) or w in ("&>", "&>>"):
            skip = True
            continue
        if re.match(r"^\d*[<>]", w) or w.startswith("-"):
            continue
        out.append(w)
    return out


def check_bash(cmd, cwd, payload=None):
    if payload and payload.get("tool_name") == "Bash":
        k = making_script(payload)
        if k is not None:
            # a $ command of a REPL script: the script was judged as a whole, so judge it again, not the command
            return check_repl(transcript_calls(payload)[k][1], cwd, payload)
    if ESCAPE in cmd:
        return repeat_escape("Bash", cmd, cwd, payload)
    # A pipe into any program shapes or cuts the output (head, sed -n, rg, python); allowed.
    if re.search(r"(?<!\|)\|(?!\|)", shell_segments.strip_bodies(cmd)):
        return None
    for segment in shell_segments.segments(cmd):
        for words in shell_segments.programs(segment):
            if not words:
                continue
            head = shell_segments.basename(words[0]).lower()
            if head.endswith(".exe"):
                head = head[:-4]
            redirected = any(re.match(r"^(?:1?>>?|&>>?)", w) for w in words[1:])
            if head in ("head", "tail"):
                # stdout redirected to a file (a copy or an append) never enters the context
                if redirected:
                    continue
                for w in files_in(words):
                    hit = long_file(w, cwd)
                    if hit and head_tail_lines(words, hit[0]) > RANGE_CAP:
                        return hit
            elif head in VIEWERS:
                if redirected:
                    continue
                if any(re.match(r"^-(totalcount|head|tail|first|last)$", w, re.I) for w in words):
                    continue
                rng = re.search(r"\)\s*\[\s*(\d+)\s*\.\.\s*(\d+)\s*\]", cmd)
                if rng and int(rng.group(2)) - int(rng.group(1)) <= RANGE_CAP:
                    continue  # (Get-Content f)[a..b], a line range
                for w in files_in(words):
                    hit = long_file(w, cwd)
                    if hit:
                        return hit
            elif head == "sed":
                rng = re.search(r"(\d+)\s*,\s*(\d+|\$)\s*p", " ".join(words))
                if not rng:
                    continue
                a = int(rng.group(1))
                for w in files_in(words):
                    if re.search(r"\d+\s*,", w):
                        continue
                    hit = long_file(w, cwd)
                    if hit and (hit[1] if rng.group(2) == "$" else int(rng.group(2))) - a > RANGE_CAP:
                        return hit
    return None


# --- REPL scripts: whole-file text that reaches the script's output ---
# A read site is file(X).text()/.lines(), file(X).slice(a, n) with n over RANGE_CAP, or a $`...` command
# check_bash denies. Its value is followed through the script: a method chain that keeps the whole text
# (PRESERVE) and plain grouping, array or object literals, Promise.all and await carry it; any other method
# or property, or an argument to any other call (write, edit, preview, a helper), cuts it. A name assigned
# a carried value carries it too. The script is denied when a carried value reaches its output: the last
# statement's value (not a declaration) or a log()/self.log() argument.
JS_STR = r"'(?:[^'\\\n]|\\.)*'|\"(?:[^\"\\\n]|\\.)*\"|\x60(?:[^\x60\\]|\\.)*\x60"
PRESERVE = {"split", "replace", "replaceAll", "map", "join", "trim", "trimStart", "trimEnd", "concat", "toString",
            "flat", "flatMap", "toUpperCase", "toLowerCase", "padStart", "padEnd", "normalize", "reverse", "sort",
            "text", "lines", "stdout", "output", "nothrow", "quiet", "all", "values", "entries"}
CARRY_CALLS = {"Promise.all", "Promise.allSettled", "String", "JSON.stringify", "Array.from", "Object.assign",
               "Object.values", "Object.entries", "Object.fromEntries"}
SINK_CALLS = {"log", "self.log"}
DECL = re.compile(r"\s*(?:const|let|var|function|class|async\s+function|if|for|while|do|try|throw|return|import|export)\b")


def js_value(expr, env):
    """A path expression of string literals, template literals and names joined by +; None if any part is unknown."""
    out = []
    for part in re.findall(JS_STR + r"|[A-Za-z_$][\w$]*|\S", expr.strip()):
        if part == "+":
            continue
        if part[0] in "'\"":
            out.append(part[1:-1].replace("\\\\", "\\"))
        elif part[0] == "\x60":
            body, ok = part[1:-1], True

            def sub(m):
                nonlocal ok
                v = env.get(m.group(1).strip())
                ok = ok and v is not None
                return v or ""
            body = re.sub(r"\$\{([^}]*)\}", sub, body)
            if not ok:
                return None
            out.append(body)
        elif re.match(r"[A-Za-z_$]", part) and part in env:
            out.append(env[part])
        else:
            return None
    return "".join(out) if out else None


def js_env(code, env=None):
    env = dict(env or {})
    for m in re.finditer(r"(?:\b(?:const|let|var)\s+|^\s*|[;{,]\s*)([A-Za-z_$][\w$]*)\s*=\s*((?:" + JS_STR + r"|[A-Za-z_$][\w$]*)(?:[ \t]*\+\s*(?:" + JS_STR + r"|[A-Za-z_$][\w$]*))*)[ \t]*(?=[;\n,)]|$)", code, re.M):
        v = js_value(m.group(2), env)
        if v is not None:
            env[m.group(1)] = v
    return env


def subagent_transcript(payload):
    tp, aid = payload.get("transcript_path") or "", payload.get("agent_id") or ""
    if not tp or not aid:
        return None
    if "subagents" in tp.replace("\\", "/"):
        return tp
    cand = os.path.join(tp[:-6] if tp.endswith(".jsonl") else tp, "subagents", "agent-" + aid + ".jsonl")
    return cand if os.path.isfile(cand) else None


_EARLIER = {}  # one hook run's transcript reads and what is derived from them


def transcript_calls(payload):
    """The agent's REPL, Bash and PowerShell calls in its transcript as (tool, code or command, id), oldest first."""
    if "calls" in _EARLIER:
        return _EARLIER["calls"]
    out, path = [], subagent_transcript(payload or {})
    if path:
        try:
            with open(path, encoding="utf-8", errors="replace") as f:
                for line in f:
                    if '"REPL"' not in line and '"Bash"' not in line and '"PowerShell"' not in line:
                        continue
                    try:
                        o = json.loads(line)
                    except ValueError:
                        continue
                    for b in (o.get("message") or {}).get("content") or []:
                        if isinstance(b, dict) and b.get("type") == "tool_use" and b.get("name") in ("REPL", "Bash", "PowerShell"):
                            i = b.get("input") or {}
                            out.append((b.get("name"), str(i.get("code") or i.get("command") or ""), b.get("id")))
        except OSError:
            pass
    _EARLIER["calls"] = out
    return out


def earlier_codes(payload):
    """The agent's earlier REPL scripts, oldest first."""
    return [t for n, t, _ in transcript_calls(payload) if n == "REPL"]


def helpers(code):
    """Range helpers a script defines, judged by how the body uses the (path, x, y) parameters:
    'span' when x and y are the first and last line (sed -n x,yp, [x..y], y - x, .slice(x - 1, y) on
    lines), 'limit' when y is a count (file(p).slice(x, y), Read limit: y); any other shape is not counted."""
    out = {}
    pat = (r"(?:\b(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*(?:async\s*)?\(\s*([^)]*)\)\s*=>"
           r"|\bfunction\s+([A-Za-z_$][\w$]*)\s*\(\s*([^)]*)\))")
    for m in re.finditer(pat, code):
        name, params = m.group(1) or m.group(3), [p.strip() for p in (m.group(2) or m.group(4) or "").split(",")]
        if len(params) != 3 or not all(re.fullmatch(r"[A-Za-z_$][\w$]*", p) for p in params):
            continue
        p, x, y = (re.escape(q) for q in params)
        body = code[m.end():m.end() + 400]
        if re.search(r"\bfile\s*\(\s*" + p + r"\s*\)\s*\.\s*slice\s*\(\s*" + x + r"\s*,\s*" + y + r"\s*\)", body) \
                or re.search(r"\blimit\s*:\s*" + y + r"\s*[,}\)]", body):
            out[name] = "limit"
        elif re.search(r"sed\s+(?:-\w+\s+)*-n|\$\{" + x + r"\}\s*\.\.\s*\$\{" + y + r"\}|\b" + y + r"\s*-\s*" + x + r"\b"
                       r"|\.slice\s*\(\s*" + x + r"\s*(?:-\s*1\s*)?,\s*" + y + r"\s*\)", body):
            out[name] = "span"
    return out

def earlier_env(payload):
    """Names the agent's earlier REPL scripts assigned (the REPL context persists between calls)."""
    if "env" not in _EARLIER:
        env = {}
        for c in earlier_codes(payload):
            env = js_env(c, env)
        _EARLIER["env"] = env
    return _EARLIER["env"]


def earlier_helpers(payload):
    """Range helpers the agent's earlier REPL scripts defined, a newer definition winning."""
    if "helpers" not in _EARLIER:
        out = {}
        for c in earlier_codes(payload):
            out.update(helpers(c))
        _EARLIER["helpers"] = out
    return _EARLIER["helpers"]


def js_mask(code):
    """String, template and comment bodies blanked (length kept; template ${...} expressions kept), each
    bracket's partner, and for each ${ brace the index after its template."""
    out, i, n, subst = list(code), 0, len(code), {}
    while i < n:
        c = code[i]
        if code.startswith("//", i) or code.startswith("/*", i):
            j = code.find("\n", i) if code[i + 1] == "/" else code.find("*/", i + 2) + 2
            j = n if j < (1 if code[i + 1] == "/" else 2) else j
            for k in range(i, j):
                out[k] = " " if code[k] != "\n" else "\n"
            i = j
            continue
        if c in "'\"\x60":
            m = re.compile(JS_STR).match(code, i)
            j = m.end() if m else n
            k, depth = i + 1, 0
            while k < max(i + 1, j - 1):
                if c == "\x60" and depth == 0 and code.startswith("${", k):
                    subst[k + 1] = j
                    depth, k = 1, k + 2
                    continue
                if depth:
                    depth += {"{": 1, "}": -1}.get(code[k], 0)
                    if depth:
                        k += 1
                        continue
                    k += 1
                    continue
                out[k] = " " if code[k] != "\n" else "\n"
                k += 1
            i = j
            continue
        i += 1
    masked = "".join(out)
    pair, stack = {}, []
    for i, c in enumerate(masked):
        if c in "([{":
            stack.append(i)
        elif c in ")]}" and stack:
            j = stack.pop()
            pair[j], pair[i] = i, j
    return masked, pair, subst


def statements(masked, pair):
    """Top-level statement spans: a ';' or a line break outside brackets that does not continue an expression."""
    spans, start, i, n = [], 0, 0, len(masked)
    while i < n:
        c = masked[i]
        if c in "([{" and i in pair:
            i = pair[i] + 1
            continue
        if c == ";" or c == "\n":
            if c == "\n":
                prev = masked[start:i].rstrip()
                nxt = masked[i + 1:].lstrip()
                if (prev and prev[-1] in "+-*/%,=(&|?:.[{<>!") or (nxt and nxt[0] in ".?:+-*/%,)]}&|"):
                    i += 1
                    continue
            if masked[start:i].strip():
                spans.append((start, i))
            start = i + 1
        i += 1
    if masked[start:].strip():
        spans.append((start, n))
    return spans


def callee(masked, opener):
    """The dotted name called at an opening '(' (empty when it is a grouping)."""
    m = re.search(r"([A-Za-z_$][\w$]*(?:\s*\.\s*[A-Za-z_$][\w$]*)*)\s*$", masked[:opener])
    if not m or re.fullmatch(r"await|return|typeof|in|of|new|void|yield|case|else", m.group(1)):
        return ""
    return re.sub(r"\s+", "", m.group(1))


def chain_end(masked, pair, pos, lo, hi, box=False):
    """Walk a method/property chain from pos; returns (end, kept, box): kept says the whole value survives,
    box that it is still a container of reads (an index picks a read out of a box, and cuts anything else)."""
    while True:
        m = re.compile(r"\s*\??\.\s*([A-Za-z_$][\w$]*)").match(masked, pos)
        if m and m.end() <= hi:
            name, pos = m.group(1), m.end()
            call = re.compile(r"\s*\(").match(masked, pos)
            if call and masked.find("(", pos) in pair:
                o = masked.find("(", pos)
                args = masked[o + 1:pair[o]].strip()
                pos = pair[o] + 1
                if name == "slice" and "," not in args and not args.lstrip("0 "):
                    continue  # slice() or slice(0) keeps all
                if name in ("map", "flatMap") and re.search(r"=>\s*[\w$]+\s*\.\s*length\s*$", args):
                    return pos, False, False  # map(x => x.length) keeps only counts
                if name not in PRESERVE:
                    return pos, False, False
                if name == "join":
                    box = False
            elif name not in PRESERVE:
                return pos, False, False
            continue
        b = re.compile(r"\s*\[").match(masked, pos)
        if b and masked.find("[", pos) in pair and pair[masked.find("[", pos)] < hi:
            if not box:
                return pair[masked.find("[", pos)] + 1, False, False
            pos, box = pair[masked.find("[", pos)] + 1, False
            continue
        return pos, True, box


def carried(masked, pair, subst, start, pos, lo, hi, box=False):
    """Where the value at [start, pos) ends up: ('sink' | 'top' (the span's value) | 'cut', box)."""
    while True:
        pos, kept, box = chain_end(masked, pair, pos, lo, hi, box)
        if not kept:
            return "cut", False
        opener = max((o for o, c in pair.items() if o < start and c >= pos and masked[o] in "([{" and lo <= o), default=None)
        if opener is None:
            return "top", box
        ch = masked[opener]
        if ch == "{" and opener in subst:
            start, pos, box = opener, subst[opener], False  # ${...}: the template carries the text
            continue
        if ch == "(":
            name = callee(masked, opener)
            if name in SINK_CALLS:
                return "sink", box
            if name == "preview":
                cap = re.search(r",\s*(\d+)\s*$", masked[opener + 1:pair[opener]])
                if not cap or int(cap.group(1)) <= PREVIEW_CAP:
                    return "cut", False
                name = ""  # a wide preview passes the text on
            if name and name not in CARRY_CALLS:
                return "cut", False
            box = name.startswith("Promise.") or (box and not name)
            start = opener - len(name) if name else opener
        elif ch == "{" and re.search(r"(=>|\))\s*$", masked[:opener]):
            return "cut", False  # a function body
        else:
            start, box = opener, True  # an array or object literal holds the read
        pos = pair[opener] + 1


def read_sites(code, masked, pair, value, cwd, hfuncs=None, seen=None):
    """Read sites of a script; when seen is a list, the calls the script resolved are added to it: ("Read", path)
    for each file(X) and tools.Read file it resolved, ("Bash", pattern) for each $ template whose every ${...}
    it resolved (pattern matches the command with each value bare or single-quoted, as $ quotes it)."""
    hfuncs = hfuncs or {}
    sites = []
    for m in re.finditer(r"(?<![\w$.])file\s*\(", masked):
        o = m.end() - 1
        if o not in pair:
            continue
        meth = re.compile(r"\s*\.\s*(text|lines|slice)\s*\(").match(masked, pair[o] + 1)
        if not meth:
            continue
        mo = meth.end() - 1
        if mo not in pair:
            continue
        path = value(code[o + 1:pair[o]])
        if seen is not None and path:
            seen.append(("Read", norm_path(path, cwd)))
        if meth.group(1) == "slice":
            nums = [x.strip() for x in code[mo + 1:pair[mo]].split(",") if x.strip()]
            try:
                if len(nums) > 1 and int(nums[1]) <= RANGE_CAP:
                    a0 = int(nums[0])
                    sites.append((m.start(), pair[mo] + 1, ("part", int(nums[1]), f"{short(path)} {a0}-{a0 + int(nums[1]) - 1}", path)))
                    continue
                if len(nums) == 1 and int(nums[0]) > 1 and path and os.path.isfile(resolve(path, cwd)):
                    a0, total = int(nums[0]), line_count(resolve(path, cwd))
                    n = tail_lines(total, a0)
                    if n is not None:  # else a whole view, judged below
                        sites.append((m.start(), pair[mo] + 1, ("part", n, f"{short(path)} {a0}-end ({n})", path)))
                        continue
            except ValueError:
                continue
        elif path and os.path.isfile(resolve(path, cwd)):
            cut = sliced(code, masked, pair, m.start(), pair[mo] + 1, meth.group(1), resolve(path, cwd))
            if cut:
                sites.append(cut[:2] + (("part", cut[2], f"{short(path)} {meth.group(1)}().slice ({cut[2]})", path),))
                continue
        hit = long_file(path, cwd) if path else None
        if hit:
            sites.append((m.start(), pair[mo] + 1, ("long",) + hit))
        elif path and os.path.isfile(resolve(path, cwd)):
            n = line_count(resolve(path, cwd))
            sites.append((m.start(), pair[mo] + 1, ("part", n, f"{short(path)} whole ({n})", path)))
    for m in re.finditer(r"(?<![\w$.])\$\s*\x60", masked):
        t = re.compile(JS_STR).match(code, m.end() - 1)
        if not t:
            continue
        cmd = value(t.group(0))
        if seen is not None:
            pat = template_pattern(t.group(0), value)
            if pat:
                seen.append(("Bash", pat))
        hit = check_bash(cmd, cwd) if cmd else None
        if hit:
            sites.append((m.start(), t.end(), ("long",) + hit))
        elif cmd:
            n, label = bash_lines(cmd, cwd)
            if n:
                sites.append((m.start(), t.end(), ("part", n, label, None)))
    for m in re.finditer(r"(?<![\w$.])tools\s*\.\s*Read\s*\(", masked):
        o = m.end() - 1
        if o not in pair:
            continue
        args = code[o + 1:pair[o]]
        lim = re.search(r"\blimit\s*:\s*(\d+)", args)
        off = re.search(r"\boffset\s*:\s*(\d+)", args)
        fp = re.search(r"\bfile_path\s*:\s*([^,}]+)", args)
        path = value(fp.group(1)) if fp else None
        if seen is not None and path:
            seen.append(("Read", norm_path(path, cwd)))
        if lim:
            sites.append((m.start(), pair[o] + 1, ("part", int(lim.group(1)), f"{short(path)} Read limit {lim.group(1)}", path)))
        elif path and os.path.isfile(resolve(path, cwd)) and path.lower().endswith(VIEW_EXT):
            n = line_count(resolve(path, cwd))
            if off and int(off.group(1)) > 1:
                n = max(n - int(off.group(1)) + 1, 0)
            sites.append((m.start(), pair[o] + 1, ("part", n, f"{short(path)} Read ({n})", path)))
    for name, kind in hfuncs.items():
        for m in re.finditer(r"(?<![\w$.])" + re.escape(name) + r"\s*\(", masked):
            o = m.end() - 1
            if o not in pair or re.search(r"(?:const|let|var|function)\s+$", masked[:m.start()]):
                continue
            parts = [x.strip() for x in code[o + 1:pair[o]].rsplit(",", 2)]
            if len(parts) < 3 or not (parts[1].isdigit() and parts[2].isdigit()):
                continue
            a0, b0 = int(parts[1]), int(parts[2])
            hp = value(parts[0])
            if not hp or not os.path.isfile(resolve(hp, cwd)):
                continue  # only a helper called on a file it can resolve
            n = b0 - a0 + 1 if kind == "span" else b0
            if n > 0:
                sites.append((m.start(), pair[o] + 1, ("part", n, f"{short(hp)} {name}({a0}, {b0})", hp)))
    return sites


def sliced(code, masked, pair, start, end, meth, path):
    """(await file(X).lines()).slice(a, b) or (await file(X).text()).slice(a, b) with literal bounds:
    (start, end, lines of file text it keeps), or None."""
    g = re.search(r"\(\s*(?:await\s+)?$", masked[:start])
    if not g or g.start() not in pair or masked[end:pair[g.start()]].strip():
        return None
    s = re.compile(r"\s*\.\s*slice\s*\(").match(masked, pair[g.start()] + 1)
    if not s or s.end() - 1 not in pair:
        return None
    nums = [x.strip() for x in code[s.end():pair[s.end() - 1]].split(",")]
    if not nums or len(nums) > 2 or not all(re.fullmatch(r"-?\d+", x) for x in nums):
        return None
    bounds = slice(*(int(x) for x in nums))
    if meth == "lines":
        n = len(read_lines(path)[bounds])
    else:
        with open(path, encoding="utf-8-sig", errors="replace") as f:
            text = f.read()[bounds]
        n = text.count("\n") + (1 if text and not text.endswith("\n") else 0)
    return g.start(), pair[s.end() - 1] + 1, n


def exempt(info):
    """Plans are read whole by design (the reviewer reads the whole approved plan)."""
    return is_plan((info[3] if len(info) > 3 else "") or "")


def short(path):
    return "/".join((path or "?").replace("\\", "/").split("/")[-2:])


def head_count(words):
    """Lines a head/tail command prints from each file: an int, ('from', k) for tail -n +k, None when unknown."""
    n = "10"
    for k, w in enumerate(words[1:], 1):
        m = re.match(r"^(?:-n|--lines=?)(\+?\d*)$", w) or re.match(r"^-(\d+)$", w)
        if m:
            n = m.group(1) or (words[k + 1] if k + 1 < len(words) else "")
    if n.startswith("+") and n[1:].isdigit():
        return ("from", int(n[1:]))
    return int(n) if n.isdigit() else None


def head_tail_lines(words, path):
    total, n = line_count(path), head_count(words)
    if isinstance(n, tuple):
        return max(total - n[1] + 1, 0)
    return min(n, total) if n is not None else total


def bash_lines(cmd, cwd):
    """Lines of file text a shell command prints: a sed -n range, a (Get-Content f)[a..b] range, a viewer's
    files, or a trailing head/tail count; 0 when unknown (rg, grep and other programs are not counted)."""
    body = shell_segments.strip_bodies(cmd)
    ht = re.search(r"\|\s*(head|tail)((?:\s+[-+\w=]+)*)\s*$", body)
    if ht:
        # only a cut of file text: the part before the pipe is itself a viewer or a sed range on a file
        left, _ = bash_lines(cmd[:cmd.rfind("|")], cwd)
        k = head_count([ht.group(1)] + ht.group(2).split())
        n = max(left - k[1] + 1, 0) if isinstance(k, tuple) else min(k, left) if k is not None else left
        return n, ("| " + ht.group(1) + ht.group(2)) if n else ""
    if re.search(r"(?<!\|)\|(?!\|)", body):
        return 0, ""
    rng = re.search(r"sed\s+(?:-\w+\s+)*-n\s+['\"]?(\d+)\s*,\s*(\d+)\s*p", cmd)
    if rng:
        return int(rng.group(2)) - int(rng.group(1)) + 1, f"sed {rng.group(1)}-{rng.group(2)}"
    rng = re.search(r"\)\s*\[\s*(\d+)\s*\.\.\s*(\d+)\s*\]", cmd)
    if rng:
        return int(rng.group(2)) - int(rng.group(1)) + 1, f"Get-Content {rng.group(1)}..{rng.group(2)}"
    total, names = 0, []
    for segment in shell_segments.segments(cmd):
        for words in shell_segments.programs(segment):
            head = shell_segments.basename(words[0]).lower().removesuffix(".exe") if words else ""
            if head in VIEWERS or head in ("head", "tail"):
                for w in files_in(words):
                    p = resolve(w, cwd)
                    if os.path.isfile(p):
                        total += head_tail_lines(words, p) if head in ("head", "tail") else line_count(p)
                        names.append(short(p))
    return total, ("cat " + " ".join(names[:3])) if total else ""


TEMPLATE_STR = re.compile(r"(?<!\\)\$\{([^}]*)\}")


def norm_path(path, cwd):
    return os.path.normcase(os.path.normpath(resolve(path, cwd)))


def shell_quote(v):
    return "'" + v.replace("'", "'\\''") + "'"


def template_pattern(literal, value):
    """The commands a $ template literal runs, as a compiled regex (each ${...} value bare or single-quoted), or
    None when the script does not resolve one of its ${...}."""
    body, parts, pos = literal[1:-1], [], 0
    for m in TEMPLATE_STR.finditer(body):
        v = value(m.group(1))
        if v is None:
            return None
        parts.append(re.escape(body[pos:m.start()].replace("\\`", "`").replace("\\${", "${")))
        parts.append("(?:" + re.escape(v) + "|" + re.escape(shell_quote(v)) + ")")
        pos = m.end()
    parts.append(re.escape(body[pos:].replace("\\`", "`").replace("\\${", "${")))
    try:
        return re.compile(r"\s*" + "".join(parts) + r"\s*", re.S)
    except re.error:
        return None


def made_by_script(payload):
    """A call a REPL script made: its hook event carries a repl_ tool_use_id, a direct call the model's toolu_ id.
    A missing or other id is judged as a direct call."""
    return str((payload or {}).get("tool_use_id") or "").startswith("repl_")


def making_script(payload):
    """The one resolver of which REPL script made this call: the index in transcript_calls of the newest script
    that resolved exactly this Bash command (a $ template) or this Read file (file(X), tools.Read). None for a
    direct call, or a script call no script resolved (it gets the check of a direct call)."""
    if "maker" in _EARLIER:
        return _EARLIER["maker"]
    found, tool = None, (payload or {}).get("tool_name")
    if made_by_script(payload) and tool in ("Bash", "Read"):
        inp, cwd = payload.get("tool_input") or {}, payload.get("cwd") or DEFAULT_CWD
        cmd = str(inp.get("command") or "")
        path = norm_path(str(inp.get("file_path") or ""), cwd) if tool == "Read" and inp.get("file_path") else None
        calls = transcript_calls(payload)
        for k in range(len(calls) - 1, -1, -1):
            if calls[k][0] != "REPL":
                continue
            seen = []
            script_sites(calls[k][1], cwd, payload, seen)
            if any(kind == tool and (v.fullmatch(cmd) if tool == "Bash" else v == path) for kind, v in seen):
                found = k
                break
    _EARLIER["maker"] = found
    return found


def earlier_escapes(payload):
    """The agent's earlier REPL scripts and Bash commands that carried the escape, oldest first. The current call
    is left out by its tool_use_id; for a call a REPL script made, that script is left out too (making_script),
    since its calls reach this hook after it entered the transcript."""
    if "esc" in _EARLIER:
        return _EARLIER["esc"]
    own, maker = payload.get("tool_use_id"), making_script(payload)
    out = [(n, t) for k, (n, t, i) in enumerate(transcript_calls(payload)) if i != own and k != maker]
    _EARLIER["esc"] = [(n, t) for n, t in out if ESCAPE in t]
    return _EARLIER["esc"]


def escaped_read(kind, text, cwd, payload):
    """The long file the call would be denied for without the escape (the same analysis as an unescaped call),
    as a normalized full path, or None."""
    stripped = text.replace(ESCAPE, "")
    hit = check_repl(stripped, cwd, payload) if kind == "REPL" else check_bash(stripped, cwd)
    if not hit or hit[0] in ("BUDGET", "REPEAT") or not isinstance(hit[0], str):
        return None
    p = hit[0] if os.path.isabs(hit[0]) else os.path.join(cwd, hit[0])
    return os.path.normcase(os.path.normpath(p))


def repeat_escape(kind, text, cwd, payload):
    """A second whole read of the same long file under the escape in one agent: the first copy is still in the
    context."""
    if not payload:
        return None
    now = escaped_read(kind, text, cwd, payload)
    if not now:
        return None
    for k, t in earlier_escapes(payload):
        if escaped_read(k, t, cwd, payload) == now:
            return ("REPEAT", now)
    return None


def script_sites(code, cwd, payload, seen=None):
    """A script's mask, bracket pairs, template braces and read sites (see read_sites for seen)."""
    env, full = js_env(code), None

    def value(expr):
        nonlocal env, full
        v = js_value(expr, env)
        if v is None and full is None:
            full = earlier_env(payload)
            env = {**full, **js_env(code, full)}
            v = js_value(expr, env)
        return v

    masked, pair, subst = js_mask(code)
    hfuncs = helpers(code)
    if re.search(r"(?<![\w$.])[A-Za-z_$][\w$]*\s*\([^()]*,\s*\d+\s*,\s*\d+\s*\)", code):
        hfuncs = {**earlier_helpers(payload), **hfuncs}
    return masked, pair, subst, read_sites(code, masked, pair, value, cwd, hfuncs, seen)


def check_repl(code, cwd, payload):
    if ESCAPE in code:
        return repeat_escape("REPL", code, cwd, payload)
    masked, pair, subst, raw = script_sites(code, cwd, payload)
    sites = [(a, b, h + (a,) if h[0] == "part" else h) for a, b, h in raw]
    if not sites:
        return None
    spans = statements(masked, pair)
    last = max((s for s in spans if not DECL.match(masked[s[0]:s[1]])), default=None, key=lambda s: s[0])
    last = last if last and last == spans[-1] else None
    tainted, returned = {}, {}
    for lo, hi in spans:
        text = masked[lo:hi]
        srcs = [(a, b, ((h,), False)) for a, b, h in sites if lo <= a < hi]
        for name, (h, hbox) in tainted.items():
            for m in re.finditer(r"(?<![\w$.])" + re.escape(name) + r"(?![\w$])", text):
                rest, head = text[m.end():], text[:m.start()].rstrip()
                if re.match(r"\s*=(?!=)", rest) or (re.match(r"\s*:", rest) and head[-1:] in ("{", ",")):
                    continue  # an assignment target or an object key
                srcs.append((lo + m.start(), lo + m.end(), (h, hbox)))
        asg = re.match(r"\s*(?:(?:const|let|var)\s+)?([A-Za-z_$][\w$]*|\[[^\]]*\]|\{[^}]*\})\s*(\+)?=(?!=)\s*", text)
        rhs_lo = lo + asg.end() if asg else lo
        out = set()
        for a, b, (h, hbox) in srcs:
            if a < rhs_lo:
                continue
            where, box = carried(masked, pair, subst, a, b, rhs_lo, hi, hbox)
            if where == "sink" or (where == "top" and not asg and (lo, hi) == last):
                for info in h:
                    if info[0] == "long":
                        return info[1:]
                    returned[info] = info
            if where == "top" and asg:
                out.add((h, box))
        if asg:
            target = asg.group(1)
            names = re.findall(r"[A-Za-z_$][\w$]*", target) if target[0] in "[{" else [target]
            if target[0] == "[" and out:
                arr = re.search(r"\[", masked[rhs_lo:hi])
                ao = rhs_lo + arr.start() if arr else None
                if ao in pair:
                    elems, s, d = [], ao + 1, 0
                    for k in range(ao + 1, pair[ao]):
                        if masked[k] == "," and not any(o < k < c for o, c in pair.items() if ao < o < pair[ao] and masked[o] in "([{"):
                            elems.append((s, k)); s = k + 1
                    elems.append((s, pair[ao]))
                    for idx, nm in enumerate(names):
                        if idx < len(elems):
                            e0, e1 = elems[idx]
                            hit = next(((h, carried(masked, pair, subst, a, b, e0, e1, hb)[1]) for a, b, (h, hb) in srcs if e0 <= a < e1 and carried(masked, pair, subst, a, b, e0, e1, hb)[0] == "top"), None)
                            if hit:
                                tainted[nm] = hit
                            else:
                                tainted.pop(nm, None)
                    continue
            for nm in names:
                if out:
                    merged = tuple(sorted({i for hs, _ in out for i in hs}, key=str))
                    tainted[nm] = (merged, any(bx for _, bx in out))
                elif not asg.group(2):
                    tainted.pop(nm, None)
    infos = {i: i for i in returned.values() if not exempt(i)}
    total = sum(i[1] for i in infos.values())
    if total > SCRIPT_CAP:
        return ("BUDGET", total, sorted(infos.values(), key=lambda i: -i[1]))
    return None

def main():
    raw = sys.stdin.read()
    if '"agent_type"' not in raw:
        return
    payload = json.loads(raw)
    if not isinstance(payload, dict) or not payload.get("agent_id") or payload.get("agent_type") not in AGENTS:
        return
    inp = payload.get("tool_input") or {}
    cwd = payload.get("cwd") or DEFAULT_CWD
    tool = payload.get("tool_name")
    maker = making_script(payload)
    if tool == "Read":
        # a Read a REPL script made (file(X).text() arrives without limit) is judged by its script
        hit = check_repl(transcript_calls(payload)[maker][1], cwd, payload) if maker is not None else check_read(inp, cwd)
    elif tool in SHELLS:
        hit = check_bash(inp.get("command") or "", cwd, payload)
    elif tool == "REPL":
        hit = check_repl(inp.get("code") or "", cwd, payload)
    else:
        hit = None
    if hit and hit[0] == "REPEAT":
        print(json.dumps({"hookSpecificOutput": {
            "hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason":
                hit[1] + " was already read whole with '" + ESCAPE + "' by this agent and is still in your context. "
                "'" + ESCAPE + "' allows one whole read per file: look the text up in that earlier result, or read "
                "a range (Read offset/limit about 100 lines; LSP documentSymbol gives a Rust item's line range)."}}))
    elif hit and hit[0] == "BUDGET":
        deny_budget(hit[1], hit[2])
    elif hit:
        deny(*hit, repl=tool == "REPL" or maker is not None)


if __name__ == "__main__":
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
        main()
    except Exception:
        pass
    sys.exit(0)
