#!/usr/bin/env python
"""PostToolUse(Bash|PowerShell): after a `git commit` lands, reminds to dispatch
one `core-reviewer` pass (planning-dispatch.md Adversarial Diff Review -
MANDATORY), naming `proof-auditor` only when the commit touches a verify stage
or a golden file. Silent on prose-only commits, --amend, --dry-run and
output without a `[branch sha]` line (failed or no-op commit), and on step-part
commits (", part <k> (<plan> step <id>)"). The commit that completes a step
after its parts is classified by the union of the parts' paths and points the
reviewer at the step's cumulative diff. Uncertainty about which files a landed
commit touched resolves toward reminding. Any internal error exits 0 with no output.
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
try:
    from commit_format import STEP_SUFFIX
except Exception:
    STEP_SUFFIX = None

SHELL_TOOLS = {"Bash", "PowerShell"}

PROSE_SUFFIXES = frozenset({".md", ".markdown", ".txt", ".rst"})

PROSE_PREFIXES = ("docs/", ".agents/", ".claude/agents/", "memory/")

COMMIT_LINE =re.compile(r"^\[[^\]]*?\b([0-9a-f]{7,40})\]", re.MULTILINE)

PART_SUBJECT = re.compile(r", part \d+ " + STEP_SUFFIX.pattern) if STEP_SUFFIX else None

PROOF_PREFIXES = (
    "tools/verifyctl/", "tools/archcheck/", "tools/conformance/", "tools/topiccheck/",
    "docs/reference/public-api-baseline/", "docs/reference/contract-golden/",
)

BASE_TEMPLATE = (
    "ADVERSARIAL DIFF REVIEW (planning-dispatch.md - MANDATORY): {landed} just "
    "landed. Dispatch ONE `core-reviewer` subagent (subagent_type: \"core-reviewer\") "
    "on {target} in a separate context, explicit model: >= the author's "
    "tier. It routes by files touched through docs/reference/core-failure-taxonomy.md "
    "and returns a binary verdict or a punch list."
)
BASE = BASE_TEMPLATE.format(landed="a commit", target="it (`git show <sha>`)")
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


def is_part(subject):
    return bool(PART_SUBJECT and PART_SUBJECT.search(subject))


def step_commits(cwd, sha, subject):
    match = STEP_SUFFIX.search(subject) if STEP_SUFFIX else None
    if not match:
        return [sha]
    suffix = match.group(0).strip()
    ok, log = run_git(
        cwd, ["log", "-F", "--grep", suffix, "--format=%H%x00%s", "-n", "50", sha])
    if not ok:
        return [sha]
    found = []
    for line in log.splitlines():
        full, _, title = line.partition("\0")
        title = title.strip()
        if not title.endswith(suffix):
            continue
        if found and not is_part(title):
            break
        found.append(full.strip())
    if not found or not found[0].startswith(sha):
        return [sha]
    return found


def union_paths(cwd, commits):
    paths = []
    for commit in commits:
        listed = commit_paths(cwd, commit)
        if listed is None:
            return None
        paths.extend(listed)
    return paths


def is_proof_path(path):
    p = path.replace("\\", "/")
    return p.startswith(PROOF_PREFIXES) or "golden" in os.path.basename(p).lower()


def is_prose_path(path):
    p = path.replace("\\", "/")
    suffix = os.path.splitext(p)[1].lower()
    if suffix == ".rs":
        return False
    return p.startswith(PROSE_PREFIXES) or suffix in PROSE_SUFFIXES


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
    all_paths, targets, ranges, unknown = [], [], 0, False
    for sha in shas:
        ok, subject = run_git(cwd, ["log", "-1", "--format=%s", sha])
        subject = subject.strip() if ok else ""
        if is_part(subject):
            continue
        commits = step_commits(cwd, sha, subject)
        paths = union_paths(cwd, commits) if len(commits) > 1 else None
        if paths is None:
            commits = [sha]
            paths = commit_paths(cwd, sha)
        if paths is None:
            unknown = True
            targets.append("`git show %s`" % sha)
            continue
        if len(commits) > 1:
            ranges += 1
            targets.append("`git diff %s^..%s`" % (commits[-1][:12], sha))
        else:
            targets.append("`git show %s`" % sha)
        all_paths.extend(paths)

    base = BASE
    if len(targets) == 1 and ranges:
        base = BASE_TEMPLATE.format(
            landed="a commit",
            target="the step's cumulative diff (%s; not only the last commit)" % targets[0])
    elif len(targets) == 1 and len(shas) > 1:
        base = BASE_TEMPLATE.format(landed="a commit", target="it (%s)" % targets[0])
    elif len(targets) > 1:
        note = "; each `git diff` range is a step's cumulative diff, not only its last commit"
        base = BASE_TEMPLATE.format(
            landed="%d commits" % len(shas),
            target="these %d changes together (%s%s)"
            % (len(targets), ", ".join(targets), note if ranges else ""))
    if unknown:
        return base + PROOF_UNKNOWN
    if all(is_prose_path(p) for p in all_paths):
        return None
    if any(is_proof_path(p) for p in all_paths):
        if len(targets) > 1:
            return base + PROOF.replace(" This commit touches", " One of these commits touches", 1)
        return base + PROOF
    return base


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
