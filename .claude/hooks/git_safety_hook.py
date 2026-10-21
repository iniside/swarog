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
import subprocess
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


SOFT_UNDO_TARGETS = {"HEAD~1", "HEAD^", "@~1", "@^"}
RESET_UNSTAGE_FLAGS = {"-q", "--quiet", "--mixed", "-N", "--intent-to-add", "-p",
                       "--patch", "--refresh", "--no-refresh"}
PATHSPEC_GLOB = re.compile(r"[*?\[]")


def commands(command):
    for raw, _ in shell_data.split_commands(command):
        yield re.sub(r"\s+", " ", raw).strip(), shell_data.git_code(raw)


def git_args(code, verb):
    return shell_data.git_args(code, verb)


def short_flag_has(arg, letter):
    return re.fullmatch(r"-[a-zA-Z]+", arg) is not None and letter in arg[1:]


def is_path(arg, cwd):
    try:
        return bool(PATHSPEC_GLOB.search(arg)) or os.path.exists(os.path.join(cwd, arg))
    except Exception:
        return False


def is_commit(arg, cwd):
    try:
        out = subprocess.run(
            ["git", "rev-parse", "--verify", "--quiet", arg + "^{commit}"],
            cwd=cwd, capture_output=True, text=True, timeout=5)
    except Exception:
        return False
    return out.returncode == 0


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


def checkout_overwrites(args, cwd):
    if "--" in args:
        return True
    flags = [a for a in args if a.startswith("-")]
    if any(a.split("=", 1)[0] in ("--force", "--patch", "--pathspec-from-file")
           or short_flag_has(a, "f") or short_flag_has(a, "p") for a in flags):
        return True
    words = [a for a in args if not a.startswith("-")]
    if "." in words:
        return True
    if any(a in ("-b", "-B", "--orphan") for a in args):
        return False
    if len(words) >= 2:
        return True
    return bool(words) and is_path(words[0], cwd) and not is_commit(words[0], cwd)


def switch_overwrites(args):
    return any(a.split("=", 1)[0] in ("--force", "--discard-changes")
               or short_flag_has(a, "f") for a in args)


def reset_allowed(args, cwd):
    if "--" in args:
        before = args[:args.index("--")]
    else:
        before = args
    flags = [a for a in before if a.startswith("-")]
    words = [a for a in before if not a.startswith("-")]
    if set(flags) <= {"--soft", "-q", "--quiet"} and "--soft" in flags:
        return len(words) == 1 and words[0] in SOFT_UNDO_TARGETS and "--" not in args
    if any(a.split("=", 1)[0] not in RESET_UNSTAGE_FLAGS | {"--pathspec-from-file"}
           for a in flags):
        return False
    return not words or words[0] in ("HEAD", "@") or is_path(words[0], cwd)


def deny_reason(flat, code, raw_command, cwd):
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
        if checkout_overwrites(args, cwd):
            return (GIT_PREFIX + "never `git checkout -- <file>` / `git checkout <path>` / "
                    "`git checkout .` / `git checkout -f` / `git checkout <rev> <path>` — "
                    "it overwrites uncommitted changes. Inspect old contents with "
                    "`git show <sha>:<path>`.")

    for args in git_args(code, "switch"):
        if switch_overwrites(args):
            return (GIT_PREFIX + "never `git switch -f` / `--discard-changes` — it "
                    "discards uncommitted changes.")

    for args in git_args(code, "reset"):
        if "--hard" in args:
            return GIT_PREFIX + "never `git reset --hard` — it discards uncommitted changes."
        if not reset_allowed(args, cwd):
            return (GIT_PREFIX + "`git reset` may only unstage (`git reset [HEAD] [--] "
                    "<path>`) or be exactly `git reset --soft HEAD~1` to undo a commit YOU "
                    "made this turn, with nothing committed since — other agents commit "
                    "on master.")

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


def check(command, cwd):
    parsed = list(commands(command))
    for flat, code in parsed:
        reason = deny_reason(flat, code, command, cwd)
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
        cwd = data.get("cwd")
        check(command, cwd if isinstance(cwd, str) and cwd else os.getcwd())
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
