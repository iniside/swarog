"""Shared shell-command scanner for the PreToolUse guards: splits a Bash or
PowerShell command into commands at `;`, `|`, `&&`, `||` and newlines, never
inside a quoted string, a heredoc body or a PowerShell here-string.
"""
import re

HEREDOC = re.compile(r"(?<!<)<<-?[ \t]*(['\"]?)([A-Za-z_]\w*)\1")
HERESTRING = re.compile(r"@(['\"])[ \t]*\r?\n")


def quote_end(text, i):
    quote = text[i]
    j = i + 1
    while j < len(text):
        c = text[j]
        if quote == '"' and c in "\\`":
            j += 2
            continue
        if c == quote:
            return j + 1
        j += 1
    return len(text)


def blank_quotes(text):
    out = []
    i = 0
    while i < len(text):
        c = text[i]
        if c in "'\"":
            out.append(c + c)
            i = quote_end(text, i)
            continue
        out.append(c)
        i += 1
    return "".join(out)


def _skip_heredoc_bodies(text, i, terminators):
    for term in terminators:
        while i < len(text):
            end = text.find("\n", i)
            line_end = len(text) if end < 0 else end + 1
            line = text[i:line_end].strip()
            i = line_end
            if line == term:
                break
    return i


def split_commands(command):
    """Yields (command_text, preceded_by_pipe); quoted text stays verbatim."""
    buf = []
    piped = False
    pending = []
    i = 0
    n = len(command)

    def flush():
        text = "".join(buf).strip()
        buf.clear()
        return (text, piped) if text else None

    while i < n:
        c = command[i]
        if c in "'\"":
            j = quote_end(command, i)
            buf.append(command[i:j])
            i = j
            continue
        if c == "@":
            match = HERESTRING.match(command, i)
            if match:
                close = re.compile(r"\r?\n" + re.escape(match.group(1)) + "@")
                end = close.search(command, match.end() - 1)
                buf.append("''")
                i = end.end() if end else n
                continue
        if c == "<":
            match = HEREDOC.match(command, i)
            if match:
                pending.append(match.group(2))
                buf.append(match.group(0))
                i = match.end()
                continue
        if c in "\\`" and i + 1 < n:
            rest = command[i + 1:i + 3]
            if rest.startswith("\n") or rest == "\r\n":
                buf.append(" ")
                i += 1 + (2 if rest == "\r\n" else 1)
                continue
            if c == "\\":
                buf.append(command[i:i + 2])
                i += 2
                continue
        if c in "\r\n":
            seg = flush()
            if seg:
                yield seg
            piped = False
            i += 1
            if c == "\r" and command.startswith("\n", i):
                i += 1
            if pending:
                i = _skip_heredoc_bodies(command, i, pending)
                pending = []
            continue
        two = command[i:i + 2]
        if two in ("&&", "||") or c in ";|":
            seg = flush()
            if seg:
                yield seg
            piped = two != "||" and c == "|"
            i += 2 if two in ("&&", "||") else 1
            continue
        buf.append(c)
        i += 1
    seg = flush()
    if seg:
        yield seg
