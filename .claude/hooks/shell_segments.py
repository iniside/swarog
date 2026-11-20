"""Shell parsing for view_guard.py (its Bash, PowerShell and REPL $`...` checks).

`segments(command)` removes here-doc / here-string bodies, drops `#` and
`<# #>` comments outside quotes (kept as text for `cmd /c`), splits the command
on `&&`, `||`, `;`, `|`, `&`, newlines, parentheses and braces outside quotes,
lifts `$(...)` and backtick substitutions out as their own segments, and
returns each segment as a token list with quotes removed. A quote left open at
the end is also re-parsed as unquoted text, so it cannot hide a later command.
`programs(tokens)`
unwraps launchers (`&`, `.`, `call`, `cmd /c`, `powershell -File/-Command`,
`pwsh`, `bash -c`, `Start-Process`, `Invoke-Expression`, env assignments) and
returns the token list starting at the program actually run.
"""
import base64
import re

_HEREDOC = re.compile(r"(?<!<)<<(?!<)-?[ \t]*(['\"]?)([A-Za-z_][A-Za-z0-9_]*)\1")
_HERESTRING = re.compile(r"@(['\"])[ \t]*\n.*?\n\1@", re.S)
_COMMENT_LEAD = " \t;|&(\n"


def skip_comment(text, i):
    """Index just past a `#` line comment or `<# #>` block starting at `i`, else `i`."""
    if text.startswith("<#", i):
        end = text.find("#>", i + 2)
        return len(text) if end < 0 else end + 2
    if text[i] == "#" and (i == 0 or text[i - 1] in _COMMENT_LEAD):
        end = text.find("\n", i)
        return len(text) if end < 0 else end
    return i


def _skip_heredoc_bodies(command, pos, names):
    for name in names:
        close = re.compile(r"^[ \t]*" + re.escape(name) + r"(?=[ \t)]|$)", re.M)
        c = close.search(command, pos)
        if not c:
            return pos
        pos = c.end()
    return pos


def strip_bodies(command, comments=True):
    command = command.replace("\r\n", "\n")
    command = _HERESTRING.sub("''", command)
    out = []
    stack = []
    pending = []
    i = 0
    n = len(command)
    while i < n:
        ch = command[i]
        top = stack[-1] if stack else None
        if top == "'":
            if ch == "'":
                stack.pop()
            out.append(ch)
            i += 1
            continue
        if ch == "`":
            end = command.find("`", i + 1)
            if end > i:
                out.append(command[i:end + 1])
                i = end + 1
                continue
        if command.startswith("$(", i):
            stack.append("(")
            out.append("$(")
            i += 2
            continue
        if top == '"':
            if ch == '"':
                stack.pop()
            out.append(ch)
            i += 1
            continue
        if comments:
            end = skip_comment(command, i)
            if end > i:
                out.append(command[i:end])
                i = end
                continue
        if ch in "'\"":
            stack.append(ch)
        elif ch == "(":
            stack.append("(")
        elif ch == ")" and top == "(":
            stack.pop()
        elif ch == "<":
            m = _HEREDOC.match(command, i)
            if m:
                pending.append(m.group(2))
                out.append(m.group(0))
                i = m.end()
                continue
        elif ch == "\n" and pending:
            out.append(ch)
            i = _skip_heredoc_bodies(command, i + 1, pending)
            pending = []
            continue
        out.append(ch)
        i += 1
    return "".join(out)


def _matching_paren(text, start, comments=True):
    depth = 0
    quote = None
    i = start
    while i < len(text):
        ch = text[i]
        if quote:
            if ch == quote:
                quote = None
        elif comments and skip_comment(text, i) > i:
            i = skip_comment(text, i)
            continue
        elif ch in "'\"":
            quote = ch
        elif ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return len(text)


def _split(text, out, comments=True):
    current = []
    quote = None
    quote_start = 0
    i = 0
    n = len(text)

    def flush():
        seg = "".join(current).strip()
        if seg:
            out.append(seg)
        current.clear()

    while i < n:
        ch = text[i]
        if quote == "'":
            current.append(ch)
            if ch == "'":
                quote = None
            i += 1
            continue
        if quote is None and comments:
            end = skip_comment(text, i)
            if end > i:
                i = end
                continue
        if ch == "$" and i + 1 < n and text[i + 1] == "(":
            end = _matching_paren(text, i + 1, comments)
            _split(text[i + 2:end], out, comments)
            current.append("_")
            i = end + 1
            continue
        if ch == "`":
            end = text.find("`", i + 1)
            if end > i:
                _split(text[i + 1:end], out, comments)
                current.append("_")
                i = end + 1
                continue
        if quote == '"':
            current.append(ch)
            if ch == '"':
                quote = None
            i += 1
            continue
        if ch in "'\"":
            quote = ch
            quote_start = i
            current.append(ch)
            i += 1
            continue
        if ch == "&":
            prev = text[i - 1] if i > 0 else ""
            nxt = text[i + 1] if i + 1 < n else ""
            if prev in "<>" or nxt == ">":
                current.append(ch)
                i += 1
                continue
            flush()
            i += 2 if nxt == "&" else 1
            continue
        if ch == "|":
            flush()
            i += 2 if i + 1 < n and text[i + 1] == "|" else 1
            continue
        if ch in ";\n(){}":
            flush()
            i += 1
            continue
        current.append(ch)
        i += 1
    flush()
    if quote:
        _split(text[quote_start + 1:], out, comments)


def tokenize(segment):
    tokens = []
    current = []
    quote = None
    quote_start = 0
    has_token = False
    for i, ch in enumerate(segment):
        if quote:
            if ch == quote:
                quote = None
            else:
                current.append(ch)
            continue
        if ch in "'\"":
            quote = ch
            quote_start = i
            has_token = True
            continue
        if ch in " \t":
            if current or has_token:
                tokens.append("".join(current))
            current = []
            has_token = False
            continue
        current.append(ch)
    if current or has_token:
        tokens.append("".join(current))
    if quote:
        tokens.extend(tokenize(segment[quote_start + 1:]))
    return tokens


def segments(command, comments=True):
    raw = []
    _split(strip_bodies(command, comments), raw, comments)
    return [t for t in (tokenize(s) for s in raw) if t]


def basename(token):
    return token.replace("\\", "/").rstrip("/").split("/")[-1].lower()


_PS_VALUE_OPTIONS = ("executionpolicy", "windowstyle", "outputformat", "inputformat",
                     "configurationname", "workingdirectory", "version", "psconsolefile",
                     "settingsfile", "custompipename")
_PS_VALUE_ALIASES = {"ep", "wd"}
_SHELLS = {"bash", "bash.exe", "sh", "sh.exe", "zsh", "dash"}
_PS = {"powershell", "powershell.exe", "pwsh", "pwsh.exe"}
_CMD = {"cmd", "cmd.exe"}
_PREFIXES = {"&", ".", "call", "exec", "time", "nohup", "env", "command", "builtin",
             "sudo", "xargs", "start", "nice"}


def _powershell(args):
    """Returns ("file", tokens) or ("command", text) or (None, None)."""
    i = 0
    while i < len(args):
        tok = args[i]
        low = tok.lower()
        if low.startswith("-") or (low.startswith("/") and len(low) > 1 and "/" not in low[1:]):
            opt = low.lstrip("-/")
            if opt == "f" or ("file".startswith(opt) and opt):
                return "file", args[i + 1:]
            if opt in ("c", "com", "comm", "comma", "comman", "command"):
                return "command", " ".join(args[i + 1:])
            if opt in ("e", "ec") or (len(opt) >= 2 and "encodedcommand".startswith(opt)):
                try:
                    return "command", base64.b64decode(args[i + 1]).decode("utf-16-le")
                except Exception:
                    return "command", " ".join(args[i + 1:])
            if opt in _PS_VALUE_ALIASES or (len(opt) >= 2 and any(o.startswith(opt) for o in _PS_VALUE_OPTIONS)):
                i += 2
                continue
            i += 1
            continue
        return "command", " ".join(args[i:])
    return None, None


def programs(tokens, depth=0):
    """Yields every token list (program first) that `tokens` ends up running."""
    if depth > 6 or not tokens:
        return
    i = 0
    while i < len(tokens):
        tok = tokens[i]
        low = tok.lower()
        if low in _PREFIXES or re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", tok):
            i += 1
            continue
        if low in ("-n", "-i") and i > 0 and tokens[i - 1].lower() in ("nice", "sudo", "env"):
            i += 1
            continue
        break
    tokens = tokens[i:]
    if not tokens:
        return
    name = basename(tokens[0])
    args = tokens[1:]
    if name in _PS:
        kind, rest = _powershell(args)
        if kind == "file":
            yield from programs(rest, depth + 1)
        elif kind == "command":
            for seg in segments(rest):
                yield from programs(seg, depth + 1)
        return
    if name in _CMD:
        for j, a in enumerate(args):
            if a.lower() in ("/c", "/k", "/r"):
                for seg in segments(" ".join(args[j + 1:]), comments=False):
                    yield from programs(seg, depth + 1)
                return
        return
    if name in _SHELLS:
        for j, a in enumerate(args):
            if a.startswith("-") and "c" in a[1:] and not a.startswith("--"):
                if j + 1 < len(args):
                    for seg in segments(args[j + 1]):
                        yield from programs(seg, depth + 1)
                return
            if not a.startswith("-"):
                yield from programs(args[j:], depth + 1)
                return
        return
    if name in ("invoke-expression", "iex"):
        text = " ".join(a for a in args if not a.lower().startswith("-command"))
        for seg in segments(text):
            yield from programs(seg, depth + 1)
        return
    if name in ("start-process", "saps"):
        target = None
        rest = []
        j = 0
        while j < len(args):
            low = args[j].lower()
            if low in ("-filepath", "-file", "-path"):
                target = args[j + 1] if j + 1 < len(args) else None
                j += 2
                continue
            if low in ("-argumentlist", "-args"):
                rest = args[j + 1:j + 2]
                j += 2
                continue
            if low.startswith("-"):
                j += 2 if low in ("-workingdirectory", "-verb", "-windowstyle",
                                  "-redirectstandardoutput", "-redirectstandarderror",
                                  "-redirectstandardinput", "-credential") else 1
                continue
            if target is None:
                target = args[j]
            else:
                rest.append(args[j])
            j += 1
        if target:
            yield from programs([target] + " ".join(rest).split(), depth + 1)
        return
    yield tokens