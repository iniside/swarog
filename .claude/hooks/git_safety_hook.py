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
import os
import re
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
try:
    import shell_data
except Exception:
    shell_data = None

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


GIT_GLOBAL_OPTS = re.compile(
    r"\bgit((?:\s+(?:-C\s+\S+|-c\s+\S+|--git-dir=\S+|--work-tree=\S+|"
    r"--no-pager|--no-optional-locks|-P))+)(?=\s|$)")
REVISION_STEP = re.compile(r"~(\d*)|\^(\d*)")
SHA = re.compile(r"[0-9a-f]{7,40}")
REDIRECTION = re.compile(r"[\d*]*(?:>>?|<)(?:&\d+|\s*[^\s<>]+)")


def commands(command):
    for raw, _ in shell_data.split_commands(command):
        flat = re.sub(r"\s+", " ", raw).strip()
        code = REDIRECTION.sub(" ", re.sub(r"\s+", " ", shell_data.blank_quotes(raw)))
        code = GIT_GLOBAL_OPTS.sub("git", code)
        yield flat, code.strip()


def git_args(code, verb):
    for match in re.finditer(r"\bgit\s+" + verb + r"\b(.*)", code):
        yield match.group(1).split()


def commits_back(rev):
    match = re.fullmatch(r"(?:HEAD|@)((?:~\d*|\^\d*)*)", rev)
    if not match:
        return None
    depth = 0
    for step in REVISION_STEP.finditer(match.group(1)):
        if step.group(0).startswith("~"):
            depth += int(step.group(1) or 1)
        elif step.group(2) in ("", "1"):
            depth += 1
        else:
            return 2
    return depth


def creates_branch(code):
    for args in git_args(code, "checkout"):
        if any(a in ("-b", "-B", "--orphan") for a in args):
            return True
    for args in git_args(code, "switch"):
        if any(a in ("-c", "-C", "--create", "--force-create", "--orphan")
               or a.startswith(("--create=", "--force-create="))
               for a in args):
            return True
    for args in git_args(code, "branch"):
        if not args:
            continue
        if any(a.split("=", 1)[0] in BRANCH_NON_CREATING_FLAGS for a in args):
            continue
        if any(not a.startswith("-") for a in args):
            return True
    return False


def force_pushes(code):
    for args in git_args(code, "push"):
        for a in args:
            if a.split("=", 1)[0] in ("--force", "--force-with-lease",
                                      "--force-if-includes"):
                return True
            if re.fullmatch(r"-[a-zA-Z]*f[a-zA-Z]*", a) or a.startswith("+"):
                return True
    return False


def overwrites_worktree(args):
    if "--" in args:
        return True
    revs = [a for a in args if not a.startswith("-")]
    if "." in revs:
        return True
    creating = any(a in ("-b", "-B", "--orphan") for a in args)
    return not creating and len(revs) >= 2


def unsafe_reset_target(args):
    for a in args:
        if a.startswith("-"):
            continue
        depth = commits_back(a)
        if depth is not None and depth >= 2 or SHA.fullmatch(a):
            return True
    return False


def deny_reason(flat, code, raw_command):
    if re.search(r"\b(rm|del|remove-item|ri)\b.*\.git[/\\]index(?!\.lock)(\b|$|['\"\s])",
                 flat, re.I):
        return (GIT_PREFIX + "never hand-edit git internals (deleting `.git/index`). A "
                "stale `.git/index.lock` is waited out and retried, never deleted.")

    for args in git_args(code, "stash"):
        if not args or args[0] not in ("list", "show"):
            return (GIT_PREFIX + "never `git stash` — it removes uncommitted work other "
                    "agents may own. Inspect old contents with `git show <sha>:<path>`.")

    if re.search(r"\bgit\s+restore\b", code):
        return (GIT_PREFIX + "never `git restore` — it discards working-tree changes. "
                "Inspect old contents with `git show <sha>:<path>`.")

    if re.search(r"\bgit\s+clean\b", code):
        return (GIT_PREFIX + "never `git clean` — it deletes untracked files, which may be "
                "another agent's in-progress work.")

    if re.search(r"\bgit\s+worktree\s+add\b", code):
        return WORKTREE_REASON

    for args in git_args(code, "checkout"):
        if overwrites_worktree(args):
            return (GIT_PREFIX + "never `git checkout -- <file>` / `git checkout .` / "
                    "`git checkout <rev> <path>` — it overwrites uncommitted changes. "
                    "Inspect old contents with `git show <sha>:<path>`.")

    for args in git_args(code, "reset"):
        if "--hard" in args:
            return GIT_PREFIX + "never `git reset --hard` — it discards uncommitted changes."
        if unsafe_reset_target(args):
            return (GIT_PREFIX + "only `git reset --soft HEAD~1` to undo a commit YOU made "
                    "this turn, with nothing committed since — other agents commit on master.")

    for args in git_args(code, "commit"):
        if "--amend" in args or "--no-edit" in args:
            continue
        visible_message = (
            any(re.fullmatch(r"-[a-zA-Z]*m.*", a) or a.startswith("--message")
                for a in args) or
            "<<" in raw_command or "@'" in raw_command or '@"' in raw_command or
            re.search(r"co-authored-by", raw_command, re.I))
        if visible_message and not TRAILER.search(raw_command):
            return ("core-rules.md Commit Message Format: every commit message ends with "
                    "`Co-Authored-By: Claude <Family> <Version> <noreply@anthropic.com>`, "
                    "filled with the identity that actually executed (see "
                    ".agents/adapters/claude.md → Commit Trailers). Add the trailer and "
                    "retry.")
    return None


def check(command):
    parsed = list(commands(command))
    for flat, code in parsed:
        reason = deny_reason(flat, code, command)
        if reason:
            deny(reason)

    if any(force_pushes(code) for _, code in parsed):
        emit("ask", GIT_PREFIX + "force-push rewrites published history; it needs the "
             "owner's explicit instruction. Approve only if the owner asked for it.")

    if any(creates_branch(code) for _, code in parsed):
        emit("ask", GIT_PREFIX + "work happens directly on master; a new branch needs the "
             "owner's explicit request. Approve only if the owner asked for it.")


def main():
    try:
        data = json.load(sys.stdin)
        tool = data.get("tool_name") or ""
        if tool and tool not in SHELL_TOOLS:
            return
        command = (data.get("tool_input") or {}).get("command") or ""
        if shell_data is None or not isinstance(command, str) or not command.strip():
            return
        check(command)
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
