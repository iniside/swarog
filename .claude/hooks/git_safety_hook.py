#!/usr/bin/env python
"""PreToolUse(Bash|PowerShell) guard for `.agents/shared/core-rules.md`:
Git Safety - MANDATORY and Commit Message Format - RULE.

Denies destructive git verbs and a visible commit message without a
`Co-Authored-By: Claude <Family> <Version> <noreply@anthropic.com>` trailer.
Returns "ask" for force-push and branch creation: both are allowed on explicit
owner instruction, and the permission prompt is that consent. Never approves;
any internal error exits 0 with no output.
"""
import json
import re
import sys

SHELL_TOOLS = {"Bash", "PowerShell"}

GIT_PREFIX = "core-rules.md Git Safety (MANDATORY): "

WORKTREE_REASON = (
    "core-rules.md Git Safety (MANDATORY): never use a git worktree — a second "
    "tree means a second `target/` and a second build/test rollout against the "
    "one shared local Postgres (migrate advisory lock, concurrent DDL). Work "
    "directly in the primary tree on master."
)

TRAILER = re.compile(
    r"Co-Authored-By:[ \t]*Claude[ \t]+[A-Z][A-Za-z]*[ \t]+\d+(?:\.\d+)*[ \t]+"
    r"<noreply@anthropic\.com>",
    re.IGNORECASE,
)

BRANCH_NON_CREATING_FLAGS = {
    "-a", "--all", "-r", "--remotes", "-v", "-vv", "--verbose", "-l", "--list",
    "--contains", "--no-contains", "--merged", "--no-merged", "--points-at",
    "--show-current", "-d", "-D", "--delete", "--sort", "--format", "--column",
    "--no-column", "--color", "--no-color", "-u", "--set-upstream-to",
    "--unset-upstream", "--edit-description", "-q", "--quiet", "-i",
    "--ignore-case", "--abbrev", "--no-abbrev",
}


def emit(decision, reason):
    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": decision,
            "permissionDecisionReason": reason,
        }
    }))
    sys.exit(0)


def deny(reason):
    emit("deny", reason)


def strip_data(command):
    code = re.sub(r"<<-?\s*['\"]?(\w+)['\"]?.*?\n\1\b", "", command, flags=re.S)
    code = re.sub(r"@(['\"])[ \t]*\r?\n.*?\r?\n\1@", "''", code, flags=re.S)
    code = re.sub(r"'[^']*'", "''", code)
    code = re.sub(r'"(?:\\.|[^"\\])*"', '""', code, flags=re.S)
    code = re.sub(r"\s+", " ", code).strip()
    return re.sub(
        r"\bgit((?:\s+(?:-C\s+\S+|-c\s+\S+|--git-dir=\S+|--work-tree=\S+|"
        r"--no-pager|--no-optional-locks|-P))+)(?=\s)",
        "git", code)


def git_args(norm, verb):
    for match in re.finditer(r"\bgit\s+" + verb + r"\b([^|;&]*)", norm):
        yield match.group(1).split()


def branch_creation(norm):
    for args in git_args(norm, "checkout"):
        if any(a in ("-b", "-B", "--orphan") for a in args):
            return True
    for args in git_args(norm, "switch"):
        if any(a in ("-c", "-C", "--create", "--force-create", "--orphan")
               or a.startswith(("--create=", "--force-create="))
               for a in args):
            return True
    for args in git_args(norm, "branch"):
        if not args:
            continue
        if any(a.split("=", 1)[0] in BRANCH_NON_CREATING_FLAGS for a in args):
            continue
        if any(not a.startswith("-") for a in args):
            return True
    return False


def force_push(norm):
    for args in git_args(norm, "push"):
        for a in args:
            if a.split("=", 1)[0] in ("--force", "--force-with-lease",
                                      "--force-if-includes"):
                return True
            if re.fullmatch(r"-[a-zA-Z]*f[a-zA-Z]*", a):
                return True
    return False


def check(command):
    norm = strip_data(command)

    if re.search(r"\b(rm|del|remove-item|ri)\b[^|;&]*\.git[/\\]index(?!\.lock)(\b|$|['\"\s])",
                 norm, re.I):
        deny(GIT_PREFIX + "never hand-edit git internals (deleting `.git/index`). A stale "
             "`.git/index.lock` is waited out and retried, never deleted.")

    if re.search(r"\bgit\s+stash\b", norm) and not re.search(
            r"\bgit\s+stash\s+(list|show)\b", norm):
        deny(GIT_PREFIX + "never `git stash` — it removes uncommitted work other agents "
             "may own. Inspect old contents with `git show <sha>:<path>`.")

    if re.search(r"\bgit\s+restore\b", norm):
        deny(GIT_PREFIX + "never `git restore` — it discards working-tree changes. "
             "Inspect old contents with `git show <sha>:<path>`.")

    if re.search(r"\bgit\s+clean\b", norm):
        deny(GIT_PREFIX + "never `git clean` — it deletes untracked files, which may be "
             "another agent's in-progress work.")

    if re.search(r"\bgit\s+worktree\s+add\b", norm):
        deny(WORKTREE_REASON)

    if re.search(r"\bgit\s+checkout\b", norm) and (
            re.search(r"\bgit\s+checkout\b[^|;&]*\s--(\s|$)", norm) or
            re.search(r"\bgit\s+checkout\s+\.(\s|$)", norm) or
            re.search(r"\bgit\s+checkout\s+HEAD\b", norm)):
        deny(GIT_PREFIX + "never `git checkout -- <file>` / `git checkout .` — it "
             "overwrites uncommitted changes. Inspect old contents with "
             "`git show <sha>:<path>`.")

    if re.search(r"\bgit\s+reset\b", norm):
        if re.search(r"\bgit\s+reset\b[^|;&]*--hard\b", norm):
            deny(GIT_PREFIX + "never `git reset --hard` — it discards uncommitted changes.")
        if (re.search(r"HEAD~([2-9]|\d{2,})\b", norm) or
                re.search(r"HEAD\^\^", norm) or
                re.search(r"\bgit\s+reset\b(\s+--(soft|mixed))?\s+[0-9a-f]{7,40}\b", norm)):
            deny(GIT_PREFIX + "only `git reset --soft HEAD~1` to undo a commit YOU made "
                 "this turn, with nothing committed since — other agents commit on master.")

    if re.search(r"\bgit\s+commit\b", norm) and not re.search(
            r"\bgit\s+commit\b[^|;&]*\s(--amend|--no-edit)\b", norm):
        visible_message = (
            re.search(r"\bgit\s+commit\b[^|;&]*\s-[a-zA-Z]*m", norm) or
            re.search(r"\bgit\s+commit\b[^|;&]*--message", norm) or
            "<<" in command or "@'" in command or '@"' in command or
            re.search(r"co-authored-by", command, re.I))
        if visible_message and not TRAILER.search(command):
            deny("core-rules.md Commit Message Format: every commit message ends with "
                 "`Co-Authored-By: Claude <Family> <Version> <noreply@anthropic.com>`, "
                 "filled with the identity that actually executed (see "
                 ".agents/adapters/claude.md → Commit Trailers). Add the trailer and retry.")

    if force_push(norm):
        emit("ask", GIT_PREFIX + "force-push rewrites published history; it needs the "
             "owner's explicit instruction. Approve only if the owner asked for it.")

    if branch_creation(norm):
        emit("ask", GIT_PREFIX + "work happens directly on master; a new branch needs the "
             "owner's explicit request. Approve only if the owner asked for it.")


def main():
    try:
        data = json.load(sys.stdin)
        tool = data.get("tool_name") or ""
        if tool and tool not in SHELL_TOOLS:
            return
        command = (data.get("tool_input") or {}).get("command") or ""
        if not isinstance(command, str) or not command.strip():
            return
        check(command)
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
