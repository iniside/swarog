#!/usr/bin/env python
"""PostToolUse(Bash|PowerShell): after a `git commit` lands, reminds to dispatch
one `core-reviewer` pass (planning-dispatch.md Adversarial Diff Review -
MANDATORY), naming `proof-auditor` only when the commit touches a verify stage
or a golden file. Silent on prose-only commits, --amend, --dry-run and
"nothing to commit". Uncertainty about which files a commit touched resolves
toward reminding. Any internal error exits 0 with no output.
"""
import json
import os
import re
import subprocess
import sys

SHELL_TOOLS = {"Bash", "PowerShell"}

PROSE_SUFFIXES = frozenset({".md", ".markdown", ".txt", ".rst"})

COMMIT_LINE = re.compile(r"^\[[^\]]*?\b([0-9a-f]{7,40})\]", re.MULTILINE)

GIT_COMMIT = re.compile(
    r"\bgit(?:\s+(?:-C\s+\S+|-c\s+\S+|--no-pager|-P))*\s+commit(?![-\w])")

PROOF_PREFIXES = (
    "tools/verifyctl/", "tools/archcheck/", "tools/conformance/", "tools/topiccheck/",
    "docs/reference/public-api-baseline/", "docs/reference/contract-golden/",
)

BASE = (
    "ADVERSARIAL DIFF REVIEW (planning-dispatch.md - MANDATORY): a commit just "
    "landed. Dispatch ONE `core-reviewer` subagent (subagent_type: \"core-reviewer\") "
    "on it (`git show <sha>`) in a separate context, explicit model: >= the author's "
    "tier. It routes by files touched through docs/reference/core-failure-taxonomy.md "
    "and returns a binary verdict or a punch list."
)
PROOF = (
    " This commit touches a verify stage or golden file — also dispatch "
    "`proof-auditor`."
)
PROOF_UNKNOWN = (
    " If it touches tools/{verifyctl,archcheck,conformance,topiccheck} or a golden "
    "file, also dispatch `proof-auditor`."
)


def run_git(cwd, args):
    try:
        out = subprocess.run(
            ["git"] + args, cwd=cwd, capture_output=True, text=True, timeout=10,
        )
    except Exception:
        return False, ""
    if out.returncode != 0:
        return False, ""
    return True, out.stdout


def commit_paths(cwd, sha):
    """Paths the commit touched, or None when they cannot be trusted."""
    ok, parents = run_git(cwd, ["rev-list", "--parents", "-n", "1", sha])
    if not ok or len(parents.split()) > 2:
        return None
    ok, listing = run_git(
        cwd, ["show", "--name-only", "--no-renames", "--pretty=format:", sha])
    if not ok:
        return None
    paths = [line.strip() for line in listing.splitlines() if line.strip()]
    return paths or None


def is_proof_path(path):
    p = path.replace("\\", "/")
    return p.startswith(PROOF_PREFIXES) or "golden" in os.path.basename(p).lower()


def response_text(response):
    if isinstance(response, dict):
        return str(response.get("stdout", "")), str(response.get("stderr", ""))
    if isinstance(response, str):
        return response, ""
    return "", ""


def reminder(data):
    command = (data.get("tool_input") or {}).get("command") or ""
    if not isinstance(command, str) or not GIT_COMMIT.search(command):
        return None
    low = command.lower()
    if "--dry-run" in low or "--amend" in low:
        return None
    stdout, stderr = response_text(data.get("tool_response"))
    if "nothing to commit" in (stdout + stderr).lower():
        return None

    shas = COMMIT_LINE.findall(stdout)
    if not shas:
        return BASE + PROOF_UNKNOWN

    cwd = data.get("cwd") or None
    all_paths = []
    for sha in shas:
        paths = commit_paths(cwd, sha)
        if paths is None:
            return BASE + PROOF_UNKNOWN
        all_paths.extend(paths)

    if all(os.path.splitext(p)[1].lower() in PROSE_SUFFIXES for p in all_paths):
        return None
    if any(is_proof_path(p) for p in all_paths):
        return BASE + PROOF
    return BASE


def main():
    try:
        data = json.load(sys.stdin)
        tool = data.get("tool_name") or ""
        if tool and tool not in SHELL_TOOLS:
            return
        text = reminder(data)
        if not text:
            return
        print(json.dumps({
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "additionalContext": text,
            }
        }))
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
