#!/usr/bin/env python
"""PostToolUse(Bash|PowerShell): after a `git commit` lands, reminds to dispatch
one `core-reviewer` pass (planning-dispatch.md Adversarial Diff Review -
MANDATORY), naming `proof-auditor` only when the commit touches a verify stage
or a golden file. Silent on prose-only commits, --amend, --dry-run and
output without a `[branch sha]` line (failed or no-op commit). Uncertainty
about which files a landed commit touched resolves toward reminding. Any internal error exits 0 with no output.
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

PROSE_SUFFIXES = frozenset({".md", ".markdown", ".txt", ".rst"})

COMMIT_LINE = re.compile(r"^\[[^\]]*?\b([0-9a-f]{7,40})\]", re.MULTILINE)

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


def commit_invocations(command):
    for raw, _ in shell_data.split_commands(command):
        for args in shell_data.git_args(shell_data.git_code(raw), "commit"):
            yield args


def reminder(data):
    command = (data.get("tool_input") or {}).get("command") or ""
    if not isinstance(command, str) or shell_data is None:
        return None
    invocations = list(commit_invocations(command))
    if not invocations or any(
            a.split("=", 1)[0] in ("--dry-run", "--amend")
            for args in invocations for a in args):
        return None
    stdout, _ = response_text(data.get("tool_response"))

    shas = COMMIT_LINE.findall(stdout)
    if not shas:
        return None

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
