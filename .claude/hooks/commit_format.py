#!/usr/bin/env python
"""Single source of the commit message rule, run as the git commit-msg hook (subject + trailers).

CLI: python commit_format.py <commit-msg-file>  -> exit 1 with the error on stderr.
"""
import difflib
import os
import re
import sys
from pathlib import Path

sys.dont_write_bytecode = True

PLANS_DIR = Path(__file__).resolve().parents[2] / "docs" / "plans"
PLAN_FILE = re.compile(r"^\d{4}-\d{2}-\d{2}(?:-\d{4})?-(.+?)(?:-plan)?\.md$", re.I)
STEP_ID = r"(?:\d+[a-z]?\d*(?:\.\d+)?|[A-Z])"
STEP_SUFFIX = re.compile(
    r"\((?P<slug>[a-z0-9][a-z0-9-]*) (?:step " + STEP_ID + r"|steps " + STEP_ID + r"-" + STEP_ID + r")\)\s*$")
STEP_MENTION = re.compile(r"\b(?i:steps?)\s+" + STEP_ID + r"(?![A-Za-z0-9])")
EXEMPT = ("Merge ", 'Revert "', "fixup! ", "squash! ", "amend! ")
SHAPE = re.compile(
    r"^(feat|fix|perf|refactor|test|docs|chore)\([a-z0-9][a-z0-9._-]*(,[a-z0-9][a-z0-9._-]*)*\): \S")

HELP = ("Commit subject must be `<type>(<scope>[,<scope>]): <imperative>` with type in feat|fix|perf|"
        "refactor|test|docs|chore and lowercase scopes (no spaces, no brackets). A subject naming a plan "
        "step must END with `(<plan-slug> step <id>)` or `(<plan-slug> steps <a>-<b>)`, the slug being a "
        "docs/plans file name without date and `-plan.md`. Example: "
        "`docs(agents): port the commit format (trackable-commit-format step 3)`.")


def plan_slugs() -> set[str]:
    return {m.group(1).lower() for f in PLANS_DIR.rglob("*.md") if (m := PLAN_FILE.match(f.name))}


def check_subject(subject: str) -> str | None:
    subject = subject.strip()
    if subject.startswith(EXEMPT):
        return None
    if not SHAPE.match(subject):
        return f"Bad commit subject: {subject!r}. {HELP}"
    if not STEP_MENTION.search(subject):
        return None
    match = STEP_SUFFIX.search(subject)
    if not match:
        return (f"Commit subject {subject!r} mentions a step but does not end with "
                f"`(<plan-slug> step <id>)` or `(<plan-slug> steps <a>-<b>)` (one id or one range, "
                f"suffix last). {HELP}")
    slugs = plan_slugs()
    slug = match.group("slug")
    if slug not in slugs:
        close = difflib.get_close_matches(slug, sorted(slugs), n=3, cutoff=0.5)
        hint = f" Closest plan slugs: {', '.join(close)}." if close else ""
        return f"Plan slug {slug!r} matches no file under docs/plans.{hint} {HELP}"
    return None


SCISSORS = re.compile(r"^# -+ >8 -+$")
TRAILER_LINE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9-]*\s*:\s")
KIND_LINE = re.compile(r"^kind\s*:", re.I)
KINDS = ("review-fix",)
KIND_HELP = ("Put `Kind: review-fix` in the final trailer paragraph together with `Co-Authored-By` (no blank line "
             "between, one -m for all trailers); it marks a commit fixing review findings, of a plan step or of a named fix.")


def message_lines(text: str) -> list[str]:
    lines = []
    for line in text.splitlines():
        if SCISSORS.match(line):
            break
        if not line.startswith("#"):
            lines.append(line.rstrip())
    return lines


def trailer_block(lines: list[str]) -> tuple[int, int]:
    end = len(lines)
    while end > 0 and not lines[end - 1].strip():
        end -= 1
    start = end
    while start > 0 and lines[start - 1].strip():
        start -= 1
    block = lines[start:end]
    if start == 0 or not block or not TRAILER_LINE.match(block[0]):
        return end, end
    if all(TRAILER_LINE.match(l) or l[:1] in " \t" for l in block):
        return start, end
    return end, end


def check_message(text: str, editor_used: bool = True) -> str | None:
    if not editor_used:
        raw = []
        for line in text.splitlines():
            if SCISSORS.match(line):
                break
            raw.append(line)
        if next((l for l in raw if l.strip()), "").startswith("#"):
            return ("Commit subject starts with '#': git keeps it as the subject when the message is given "
                    f"with -m or -F. {HELP}")
    lines = message_lines(text)
    subject_at = next((i for i, l in enumerate(lines) if l.strip()), None)
    if subject_at is None:
        return None
    if lines[subject_at][:1].isspace():
        return f"Commit subject has leading whitespace. {HELP}"
    subject = lines[subject_at].strip()
    if subject.startswith(EXEMPT):
        return None
    error = check_subject(subject)
    if error:
        return error
    start, end = trailer_block(lines[subject_at:])
    start, end = start + subject_at, end + subject_at
    kinds = []
    for i, line in enumerate(lines[subject_at + 1:], subject_at + 1):
        if not KIND_LINE.match(line):
            continue
        if not start <= i < end:
            return f"`Kind:` line outside the final trailer paragraph: {line.strip()!r}. {KIND_HELP}"
        kinds.append(line.split(":", 1)[1].strip())
    if len(kinds) > 1:
        return f"More than one `Kind:` trailer ({', '.join(kinds)}). {KIND_HELP}"
    if kinds and kinds[0] not in KINDS:
        return f"Unknown `Kind: {kinds[0]}`; allowed: {', '.join(KINDS)}. {KIND_HELP}"
    return None


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit(0)
    try:
        text = Path(sys.argv[1]).read_text(encoding="utf-8", errors="replace")
    except OSError:
        sys.exit(0)
    error = check_message(text, editor_used=os.environ.get("GIT_EDITOR") != ":")
    if error:
        print(error, file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
