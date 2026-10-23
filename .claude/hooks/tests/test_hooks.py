"""Subprocess tests for the .claude/hooks guard hooks.

Run from the repo root: python -B -m unittest discover .claude/hooks/tests
Each hook is fed crafted hook JSON on stdin; the decision JSON is classified as
deny / ask / silent / context. Shell cases run under both Bash and PowerShell.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
import uuid

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
HOOKS = os.path.join(ROOT, ".claude", "hooks")
R = ROOT.replace("\\", "/")
TR = "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
SHELL_TOOLS = ("Bash", "PowerShell")

_TEMP = tempfile.TemporaryDirectory()
ENV = dict(os.environ, PYTHONDONTWRITEBYTECODE="1", TEMP=_TEMP.name, TMP=_TEMP.name,
           TMPDIR=_TEMP.name)
ENV.pop("CLAUDE_PROJECT_DIR", None)


def _git(repo, *args):
    r = subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@example.com",
                        "-c", "commit.gpgsign=false", *args], cwd=repo, capture_output=True,
                       text=True, timeout=60)
    assert r.returncode == 0, (args, r.stderr)
    return r.stdout.strip()


def _commit(repo, rel, text, msg):
    path = os.path.join(repo, *rel.split("/"))
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        f.write(text)
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", msg)
    return _git(repo, "rev-parse", "--short=10", "HEAD")


def _build_fixture():
    repo = os.path.join(_TEMP.name, "fixture-repo")
    os.makedirs(repo)
    _git(repo, "init", "-q", "-b", "master")
    _commit(repo, "README.md", "seed\n", "seed")
    shas = {
        "code": _commit(repo, "core/x/src/lib.rs", "fn a() {}\n", "code"),
        "prose": _commit(repo, "docs/note.md", "hello\n", "prose"),
        "proof": _commit(repo, "tools/verifyctl/x.rs", "fn b() {}\n", "proof"),
    }
    _git(repo, "checkout", "-q", "-b", "side")
    _commit(repo, "side.rs", "fn s() {}\n", "side")
    _git(repo, "checkout", "-q", "master")
    _commit(repo, "main.rs", "fn m() {}\n", "main")
    _git(repo, "merge", "-q", "--no-ff", "-m", "merge", "side")
    shas["merge"] = _git(repo, "rev-parse", "--short=10", "HEAD")
    return repo, shas


FIX, FIX_SHAS = _build_fixture()
FX = FIX.replace("\\", "/")
CODE_SHA = FIX_SHAS["code"]
PROSE_SHA = FIX_SHAS["prose"]


def tearDownModule():
    _TEMP.cleanup()


def run(hook, payload, raw=None):
    inp = raw if raw is not None else json.dumps(payload)
    r = subprocess.run([sys.executable, "-B", os.path.join(HOOKS, hook + ".py")], input=inp,
                       capture_output=True, text=True, cwd=ROOT, env=ENV, timeout=60)
    if r.returncode != 0 or r.stderr:
        return "ERR rc=%d %s" % (r.returncode, r.stderr[:200])
    if not r.stdout.strip():
        return "silent"
    o = json.loads(r.stdout)["hookSpecificOutput"]
    want_event = "PostToolUse" if hook == "post_commit_review_reminder" else "PreToolUse"
    assert o.get("hookEventName") == want_event, (hook, o.get("hookEventName"))
    return o.get("permissionDecision") or ("context:" + o.get("additionalContext", ""))


def sh(cmd, tool="Bash"):
    return {"tool_name": tool, "tool_input": {"command": cmd}}


def shc(cmd, tool, cwd=R):
    p = sh(cmd, tool)
    if cwd:
        p["cwd"] = cwd
    return p


def pc(cmd, tool, out):
    return {"tool_name": tool, "cwd": FX, "tool_input": {"command": cmd}, "tool_response": {"stdout": out}}


ALLOW_BRANCH = ["git branch", "git branch -a", "git branch -v", "git branch --show-current",
                "git branch -d x", "git branch -D x", "git branch --contains x",
                "git branch --list 'pat*'", "git switch master", "git checkout master",
                "git status"]

BASE_CASES = [
    ("git_safety_hook", sh("git stash"), "deny"),
    ("git_safety_hook", sh("git stash list"), "silent"),
    ("git_safety_hook", sh("git restore x"), "deny"),
    ("git_safety_hook", sh("git -C G:/x restore x"), "deny"),
    ("git_safety_hook", sh("git reset --hard", "PowerShell"), "deny"),
    ("git_safety_hook", sh("git reset --soft HEAD~1"), "silent"),
    ("git_safety_hook", sh("git reset HEAD~3"), "deny"),
    ("git_safety_hook", sh("git checkout -- a.rs"), "deny"),
    ("git_safety_hook", sh("git worktree add ../x"), "deny"),
    ("git_safety_hook", sh("Remove-Item .git\\index", "PowerShell"), "deny"),
    ("git_safety_hook", sh("git commit -m 'feat: x'"), "deny"),
    ("git_safety_hook", sh("git commit -m 'feat: x\n\nCo-Authored-By: Claude <Family> <Version> <noreply@anthropic.com>'"), "deny"),
    ("git_safety_hook", sh("git commit -m 'docs: say git stash is banned\n\n" + TR + "'"), "silent"),
    ("git_safety_hook", sh("git commit -F - <<'EOF'\nfix: x\n\n" + TR + "\nEOF"), "silent"),
    ("git_safety_hook", sh("$m = @'\nfix: mention git restore\n\n" + TR + "\n'@\ngit commit -m $m", "PowerShell"), "silent"),
    ("git_safety_hook", sh("git commit -m @'\nfix: x\n\n" + TR + "\n'@", "PowerShell"), "silent"),
    ("git_safety_hook", sh("git commit -m @'\nfix: x\n'@", "PowerShell"), "deny"),
    ("git_safety_hook", sh("git commit --amend --no-edit"), "silent"),
    ("git_safety_hook", sh("git push --force"), "ask"),
    ("git_safety_hook", sh("git push -f origin master"), "ask"),
    ("git_safety_hook", sh("git push --force-with-lease"), "ask"),
    ("git_safety_hook", sh("git push origin master"), "silent"),
    ("git_safety_hook", sh("git checkout -b x"), "ask"),
    ("git_safety_hook", sh("git switch -c x", "PowerShell"), "ask"),
    ("git_safety_hook", sh("git branch newbr"), "ask"),
    ("git_safety_hook", sh("git stash; git push --force"), "deny"),
    *[("git_safety_hook", sh(c), "silent") for c in ALLOW_BRANCH],
    ("git_safety_hook", sh("git checkout master 2>&1"), "silent"),
    ("git_safety_hook", sh("git checkout master > out.txt", "PowerShell"), "silent"),
    ("git_safety_hook", sh("git checkout master *>$null", "PowerShell"), "silent"),
    ("git_safety_hook", sh("git checkout HEAD a.rs 2>&1"), "deny"),
    ("git_safety_hook", sh("git commit-graph write"), "silent"),
    ("git_safety_hook", sh("echo \"git stash\""), "silent"),
    ("git_safety_hook", sh("git push origin +master"), "ask"),
    ("git_safety_hook", sh("git push origin +master:master"), "ask"),
    ("git_safety_hook", sh("git stash list && git stash pop"), "deny"),
    ("git_safety_hook", sh("git stash show && git log"), "silent"),
    ("git_safety_hook", sh("git stash list\ngit stash"), "deny"),
    ("git_safety_hook", sh("git checkout master\ngit log -- path"), "silent"),
    ("git_safety_hook", sh("git status\ngit branch\ngit log -3"), "silent"),
    ("git_safety_hook", sh("git status\r\ngit branch\r\ngit log -3", "PowerShell"), "silent"),
    ("git_safety_hook", sh("git log HEAD~5..HEAD && git reset --soft HEAD~1"), "silent"),
    ("git_safety_hook", sh("git checkout HEAD~1"), "silent"),
    ("git_safety_hook", sh("git checkout HEAD -- a.rs"), "deny"),
    ("git_safety_hook", sh("git checkout HEAD a.rs"), "deny"),
    ("git_safety_hook", sh("git checkout ."), "deny"),
    ("git_safety_hook", sh("git checkout -b x origin/x"), "ask"),
    ("git_safety_hook", sh("git reset --soft HEAD~2"), "deny"),
    ("git_safety_hook", sh("git reset HEAD^^"), "deny"),
    ("git_safety_hook", sh("git reset --soft HEAD~~"), "deny"),
    ("git_safety_hook", sh("git reset --soft abc1234"), "deny"),
    ("git_safety_hook", sh("git reset --soft HEAD^"), "silent"),
    ("git_safety_hook", sh("git reset HEAD a.rs"), "silent"),
    ("git_safety_hook", sh("git status\ngit reset --hard"), "deny"),
    ("git_safety_hook", sh("git log \\\n  --oneline -3"), "silent"),
    ("git_safety_hook", sh("git commit -F - <<'EOF'\nfix: x; git stash\n\n" + TR + "\nEOF\ngit log -1"), "silent"),
    ("git_safety_hook", sh("cat > f.txt <<'EOF'\ngit stash\nEOF\ngit stash"), "deny"),
    ("git_safety_hook", sh("Remove-Item '.git\\index'", "PowerShell"), "deny"),
    ("git_safety_hook", sh("git -C G:/x stash list; git -C G:/x stash drop"), "deny"),
    ("git_safety_hook", sh("git commit -m \"fix: x; git stash\n\n" + TR + "\""), "silent"),
    ("git_safety_hook", {"tool_name": "Read", "tool_input": {"command": "git stash"}}, "silent"),
    ("git_safety_hook", None, "silent"),
    ("no_worktree_hook", {"tool_name": "EnterWorktree", "tool_input": {}}, "deny"),
    ("no_worktree_hook", {"tool_name": "Agent", "tool_input": {"isolation": "worktree", "model": "opus"}}, "deny"),
    ("no_worktree_hook", {"tool_name": "Agent", "tool_input": {"model": "opus", "prompt": "x"}}, "silent"),
    ("no_worktree_hook", None, "silent"),
    ("explicit_model_guard", {"tool_name": "Agent", "tool_input": {"prompt": "x"}}, "deny"),
    ("explicit_model_guard", {"tool_name": "Agent", "tool_input": {"prompt": "x", "model": "  "}}, "deny"),
    ("explicit_model_guard", {"tool_name": "Agent", "tool_input": {"prompt": "x", "model": "sonnet"}}, "silent"),
    ("explicit_model_guard", None, "silent"),
    ("haiku_listing_reminder", {"tool_name": "Agent", "tool_input": {"model": "opus", "prompt": "List all callers of foo"}}, "deny"),
    ("haiku_listing_reminder", {"tool_name": "Agent", "tool_input": {"model": "opus", "prompt": "List all callers of foo force-model"}}, "silent"),
    ("haiku_listing_reminder", {"tool_name": "Agent", "tool_input": {"model": "opus", "prompt": "List all callers and analyze the flow"}}, "silent"),
    ("haiku_listing_reminder", {"tool_name": "Agent", "tool_input": {"model": "haiku", "prompt": "List all callers"}}, "silent"),
    ("haiku_listing_reminder", None, "silent"),
    ("grep_sweep_guard", sh("grep -r foo target/"), "deny"),
    ("grep_sweep_guard", sh("grep -rn foo core/"), "deny"),
    ("grep_sweep_guard", sh("git log | grep foo"), "silent"),
    ("grep_sweep_guard", sh("grep -r foo target/ force-grep"), "silent"),
    ("grep_sweep_guard", sh("rg --no-ignore foo"), "deny"),
    ("grep_sweep_guard", sh("rg foo core"), "silent"),
    ("grep_sweep_guard", sh("Get-ChildItem -Recurse -Filter *.rs", "PowerShell"), "silent"),
    ("grep_sweep_guard", sh("Get-ChildItem -Recurse target | Select-String foo", "PowerShell"), "deny"),
    ("grep_sweep_guard", sh("Get-ChildItem -Recurse .\\run\\devctl", "PowerShell"), "deny"),
    ("grep_sweep_guard", sh("gci -Recurse -Path deploy -Filter *.toml", "PowerShell"), "deny"),
    ("grep_sweep_guard", sh("Select-String -Path target\\debug\\*.d -Pattern foo", "PowerShell"), "deny"),
    ("grep_sweep_guard", sh("Select-String -Path core\\x.rs -Pattern 'run'", "PowerShell"), "silent"),
    ("grep_sweep_guard", sh("Get-ChildItem -Recurse | Select-String deploy", "PowerShell"), "silent"),
    ("grep_sweep_guard", sh("cargo run -p devctl -- up monolith"), "silent"),
    ("grep_sweep_guard", sh("gci -r target", "PowerShell"), "deny"),
    ("grep_sweep_guard", sh("dir -r run", "PowerShell"), "deny"),
    ("grep_sweep_guard", sh("gci -re -Path deploy", "PowerShell"), "deny"),
    ("grep_sweep_guard", sh("gci -Recurse:$true target", "PowerShell"), "deny"),
    ("grep_sweep_guard", sh("gci -ReadOnly target", "PowerShell"), "silent"),
    ("grep_sweep_guard", sh("git commit -F - <<'EOF'\nfix: x; find . -name y\nEOF"), "silent"),
    ("grep_sweep_guard", sh("cat > f.py <<'EOF'\nimport os\ngrep -r a b\nEOF"), "silent"),
    ("grep_sweep_guard", sh("cat > f.py <<'EOF'\ngrep -r a b\nEOF\ngrep -r a core/"), "deny"),
    ("grep_sweep_guard", sh("git commit -m 'fix: x; grep -r foo core'"), "silent"),
    ("grep_sweep_guard", sh("echo \"a | find . -name x\""), "silent"),
    ("grep_sweep_guard", sh("git status\ngrep -rn foo core/"), "deny"),
    ("grep_sweep_guard", sh("Select-String -Path \"target\\debug\\x.d\" -Pattern foo", "PowerShell"), "deny"),
    ("grep_sweep_guard", sh("find . -maxdepth 1 -exec cat {} \\;"), "silent"),
    ("grep_sweep_guard", sh("$s = @'\ngrep -r foo .\n'@\nWrite-Output $s", "PowerShell"), "silent"),
    ("grep_sweep_guard", None, "silent"),
    ("exit_plan_checklist", {"tool_name": "ExitPlanMode", "tool_input": {}}, "context"),
    ("exit_plan_checklist", None, "context"),
    ("post_commit_review_reminder", {"tool_name": "PowerShell", "cwd": FX, "tool_input": {"command": "git commit -m x"}, "tool_response": {"stdout": "[master " + PROSE_SHA + "] docs\n"}}, "silent"),
    ("post_commit_review_reminder", {"tool_name": "Bash", "cwd": FX, "tool_input": {"command": "git commit -m x"}, "tool_response": {"stdout": "[master " + CODE_SHA + "] refactor\n"}}, "context"),
    ("post_commit_review_reminder", {"tool_name": "Bash", "cwd": FX, "tool_input": {"command": "git commit --amend"}, "tool_response": {"stdout": "[master " + CODE_SHA + "] x\n"}}, "silent"),
    ("post_commit_review_reminder", {"tool_name": "Bash", "cwd": FX, "tool_input": {"command": "git commit --dry-run"}, "tool_response": {"stdout": ""}}, "silent"),
    ("post_commit_review_reminder", {"tool_name": "Bash", "cwd": FX, "tool_input": {"command": "git commit-graph write"}, "tool_response": {"stdout": ""}}, "silent"),
    ("post_commit_review_reminder", None, "silent"),
]

NEW_GIT = [
    ("git checkout -f", "deny"), ("git checkout --force master", "deny"),
    ("git checkout -fq master", "deny"), ("git checkout -p", "deny"),
    ("git switch --discard-changes master", "deny"), ("git switch -f master", "deny"),
    ("git switch --force master", "deny"), ("git switch master", "silent"),
    ("git checkout CLAUDE.md", "deny"), ("git checkout core", "deny"),
    ("git checkout master CLAUDE.md", "deny"), ("git checkout '*.rs'", "deny"),
    ("git checkout master", "silent"), ("git checkout HEAD~1", "silent"),
    ("git checkout -b x origin/x", "ask"), ("git checkout nosuchthing", "silent"),
    ("git reset", "silent"), ("git reset HEAD a.rs", "silent"), ("git reset -- a.rs", "silent"),
    ("git reset HEAD", "silent"), ("git reset CLAUDE.md", "silent"), ("git reset -q", "silent"),
    ("git reset --soft HEAD~1", "silent"), ("git reset --soft HEAD^", "silent"),
    ("git reset --soft @~1", "silent"), ("git reset --soft @^", "silent"),
    ("git reset --hard", "deny"), ("git reset --mixed HEAD~1", "deny"),
    ("git reset --soft origin/master", "deny"), ("git reset --soft HEAD@{1}", "deny"),
    ("git reset HEAD@{2}", "deny"), ("git reset --soft ABC1234", "deny"),
    ("git reset --soft abc1234def", "deny"), ("git reset deadbeef", "deny"),
    ("git reset HEAD~1", "deny"), ("git reset --keep HEAD~1", "deny"),
    ("git reset --merge", "deny"), ("git reset --soft", "deny"),
    ("git clean -fd", "deny"), ("git push --force-if-includes", "ask"),
    ("rm .git/index", "deny"), ("Remove-Item .git\\index", "deny"),
    ("rm .git/index.lock", "silent"), ("Remove-Item .git\\index.lock", "silent"),
    ("git reset --soft HEAD~2", "deny"), ("git reset origin/master", "deny"),
    ("git reset \"--hard\"", "deny"), ("git \"restore\" .", "deny"),
    ("git reset --soft 'HEAD~3'", "deny"), ("git 'stash'", "deny"),
    ("git commit -m \"docs: never git stash\n\n" + TR + "\"", "silent"),
    ("git commit -m 'git stash'\n", "deny"),
    ("git commit -m 'docs: git stash\n\n" + TR + "'", "silent"),
    ("git commit -F - <<\\EOF\nfix: x; git stash\n\n" + TR + "\nEOF\ngit log -1", "silent"),
    ("cat > f <<\\EOF\ngit stash\nEOF\ngit stash", "deny"),
    ("cat > f <<-EOF\n\tgit stash\n\tEOF\ngit log", "silent"),
    ("cat > f <<\"EOF\"\ngit stash\nEOF\ngit log", "silent"),
    ("git commit --no-verify -m 'x\n\n" + TR + "'", "deny"),
    ("git commit --no-veri -m 'x\n\n" + TR + "'", "deny"),
    ("git commit -n -m 'x\n\n" + TR + "'", "deny"),
    ("git commit -nm 'x\n\n" + TR + "'", "deny"),
    ("git commit -an -m 'x\n\n" + TR + "'", "deny"),
    ("git commit -am 'x\n\n" + TR + "'", "silent"),
    ("git commit -uno -m 'x\n\n" + TR + "'", "silent"),
    ("git commit -un -m 'x\n\n" + TR + "'", "silent"),
    ("git commit -Skey -m 'x\n\n" + TR + "'", "silent"),
    ("git commit -u -n -m 'x\n\n" + TR + "'", "deny"),
    ("git commit -S -n -m 'x\n\n" + TR + "'", "deny"),
    ("git commit -aS -n -m 'x\n\n" + TR + "'", "deny"),
    ("git commit --no-verbose -m 'x\n\n" + TR + "'", "silent"),
    ("git commit -m 'x\n\n" + TR + "' --no-verify", "deny"),
    ("git commit -m 'x\n\n" + TR + "' -n", "deny"),
    ("git commit --amend -n", "deny"),
    ("git commit --allow-empty -n", "deny"),
    ("git commit -mWIP -n", "deny"),
    ("git commit --amend --no-edit -n", "deny"),
    ("git commit -m -n --amend", "silent"),
    ("git commit --author -n -m 'x\n\n" + TR + "'", "silent"),
    ("git commit -Sjohn -m 'x\n\n" + TR + "'", "silent"),
    ("git commit -m -nothing --amend", "silent"),
    ("git commit --amend --no-edit", "silent"),
    ("git commit -m 'docs(agents): deny --no-verify and -n\n\n" + TR + "'", "silent"),
]

POST = [
    ("git commit -m x", "[master " + CODE_SHA + "] refactor\n", "context"),
    ("git commit -m x", "On branch master\nnothing to commit, working tree clean\n", "silent"),
    ("git commit -m x", "error: pathspec 'y' did not match\n", "silent"),
    ("git commit -m x", "", "silent"),
    ("git status; git commit -m x", "[master " + CODE_SHA + "] refactor\n", "context"),
    ("git -C " + R + " commit -m x", "[master " + CODE_SHA + "] refactor\n", "context"),
    ("echo 'git commit -m x'", "[master " + CODE_SHA + "] refactor\n", "silent"),
    ("echo \"git commit\"", "[master " + CODE_SHA + "] refactor\n", "silent"),
    ("git log --grep=commit", "[master " + CODE_SHA + "] refactor\n", "silent"),
    ("git commit --amend --no-edit", "[master " + CODE_SHA + "] refactor\n", "silent"),
    ("git commit --dry-run", "[master " + CODE_SHA + "] refactor\n", "silent"),
    ("git commit -m \"mention --amend in text\"", "[master " + CODE_SHA + "] refactor\n", "context"),
    ("git commit -m x", "[master " + PROSE_SHA + "] docs\n", "silent"),
]


def classify_all(payload_hook_filter):
    return [(h, p, w) for h, p, w in BASE_CASES if h == payload_hook_filter]


def _variants(payload):
    if isinstance(payload, dict) and payload.get("tool_name") in SHELL_TOOLS:
        for tool in SHELL_TOOLS:
            yield dict(payload, tool_name=tool)
    else:
        yield payload


class HookCases(unittest.TestCase):
    def check(self, hook, extra=()):
        cases = classify_all(hook) + [c for c in extra if c[0] == hook]
        self.assertTrue(cases)
        for _, payload, want in cases:
            if payload is None:
                continue
            for p in _variants(payload):
                label = (p.get("tool_input") or {}).get("command") or p.get("tool_name")
                with self.subTest(hook=hook, tool=p.get("tool_name"), case=str(label)[:70], want=want):
                    got = run(hook, p)
                    self.assertTrue(got.startswith(want), "%s != %s" % (got, want))

    def test_git_safety(self):
        extra = []
        for tool in SHELL_TOOLS:
            for cmd, want in NEW_GIT:
                extra.append(("git_safety_hook", shc(cmd, tool), want))
            extra.append(("git_safety_hook", shc("git checkout CLAUDE.md", tool, cwd=None), "deny"))
            extra.append(("git_safety_hook", shc("git checkout CLAUDE.md", tool, cwd="C:/Windows"), "silent"))
        self.check("git_safety_hook", extra)

    def test_post_commit(self):
        extra = [("post_commit_review_reminder", pc(c, t, o), w)
                 for t in SHELL_TOOLS for c, o, w in POST]
        self.check("post_commit_review_reminder", extra)

    def test_known_gap_reset_soft_head_tilde_denied(self):
        for tool in SHELL_TOOLS:
            self.assertEqual(run("git_safety_hook", shc("git reset --soft HEAD~", tool)), "deny")

    def test_known_gap_checkout_track_silent(self):
        for tool in SHELL_TOOLS:
            self.assertEqual(
                run("git_safety_hook", shc("git checkout --track origin/x", tool)), "silent")

    def test_grep_sweep(self):
        self.check("grep_sweep_guard")

    def test_haiku_listing(self):
        self.check("haiku_listing_reminder")

    def test_no_worktree(self):
        self.check("no_worktree_hook")

    def test_explicit_model_guard(self):
        self.check("explicit_model_guard")
        ok = run("explicit_model_guard", {"tool_name": "Agent", "tool_input": {"model": "opus", "prompt": "x"}})
        self.assertEqual(ok, "silent")

    def test_exit_plan_context_never_deny(self):
        got = run("exit_plan_checklist", {"tool_name": "ExitPlanMode", "tool_input": {}})
        self.assertTrue(got.startswith("context:"))
        for needle in ("core-reviewer", "test-author", "model"):
            self.assertIn(needle, got)


sys.path.insert(0, HOOKS)
import commit_format  # noqa: E402

CF_TRAILER = "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
CF_SCISSORS = "# ------------------------ >8 ------------------------"


class CommitFormat(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.plans = os.path.join(_TEMP.name, "cf-plans")
        os.makedirs(cls.plans)
        with open(os.path.join(cls.plans, "2026-01-01-0000-demo-plan.md"), "w") as f:
            f.write("x\n")
        cls.saved = commit_format.PLANS_DIR
        commit_format.PLANS_DIR = commit_format.Path(cls.plans)

    @classmethod
    def tearDownClass(cls):
        commit_format.PLANS_DIR = cls.saved

    def ok(self, text):
        self.assertIsNone(commit_format.check_message(text), text)

    def bad(self, text, needle=None):
        err = commit_format.check_message(text)
        self.assertIsNotNone(err, text)
        if needle:
            self.assertIn(needle, err)
        return err

    def test_subject_shape(self):
        self.ok("feat(core): add a thing")
        self.ok("fix(a,b): two scopes")
        self.bad("add a thing", "Bad commit subject")
        self.bad("feat: add a thing", "Bad commit subject")
        self.bad("feat([Core]): add a thing", "Bad commit subject")
        self.bad("feat(Core): add a thing", "Bad commit subject")
        self.bad("wip(core): add a thing", "Bad commit subject")
        self.bad("revert(x): y", "Bad commit subject")

    def test_exempt_prefixes(self):
        for s in ("Merge branch 'x'", 'Revert "feat(a): b"', "fixup! feat(a): b",
                  "squash! feat(a): b", "amend! feat(a): b"):
            with self.subTest(s=s):
                self.ok(s)

    def test_step_suffix(self):
        self.ok("feat(core): add a thing (demo step 3)")
        self.ok("feat(core): add a thing (demo steps 3-4)")
        self.bad("feat(core): finish step 3", "does not end with")
        self.bad("feat(core): add a thing (demo step 3) now", "does not end with")
        self.ok("feat(core): the next step is harder")
        self.ok("feat(core): add a step function")

    def test_unknown_slug_hint(self):
        err = self.bad("feat(core): add a thing (demo-x step 3)", "matches no file")
        self.assertIn("Closest plan slugs: demo", err)
        err = self.bad("feat(core): add a thing (zzzzqqqq step 3)", "matches no file")
        self.assertNotIn("Closest", err)

    def test_kind_trailer_ok(self):
        self.ok("fix(core): fix findings\n\nbody text\n\nKind: review-fix\n" + CF_TRAILER + "\n")
        self.ok("fix(core): fix findings (demo step 2)\n\nKind: review-fix\n" + CF_TRAILER + "\n")

    def test_kind_trailer_rejected(self):
        self.bad("fix(core): x\n\nKind: review-fix\n\nmore body\n\n" + CF_TRAILER + "\n",
                 "outside the final trailer paragraph")
        self.bad("fix(core): x\n\nbody\n\nKind: review-fix\nKind: review-fix\n" + CF_TRAILER + "\n",
                 "More than one")
        self.bad("fix(core): x\n\nbody\n\nKind: other\n" + CF_TRAILER + "\n", "Unknown `Kind: other`")

    def test_comments_and_scissors_ignored(self):
        self.ok("fix(core): x\n# Kind: other\n# a comment\n\nbody\n\n" + CF_TRAILER + "\n")
        self.ok("fix(core): x\n# trailing comment\n\nbody\n\n" + CF_TRAILER + "\n")
        self.ok("feat(x): ok\n# comment\n")
        self.ok("fix(core): x\n\nbody\n\n" + CF_TRAILER + "\n" + CF_SCISSORS +
                "\nKind: other\nbad subject\ndiff --git a b\n")
        self.bad("fix(core): x\n\nbody\n\nKind: other\n" + CF_TRAILER + "\n" + CF_SCISSORS + "\nignored\n")

    def test_hash_led_message_rejected_without_editor(self):
        for text in ("# leading comment\nfix(core): x\n\nbody\n\n" + CF_TRAILER + "\n",
                     "# x\n\nfeat(x): ok\n", "#123 fix the thing\n"):
            with self.subTest(text=text):
                err = commit_format.check_message(text, editor_used=False)
                self.assertIsNotNone(err)
                self.assertIn("starts with '#'", err)

    def test_hash_led_template_accepted_with_editor(self):
        self.ok("# Subject below\nfeat(x): ok\n")

    def test_kind_followed_by_prose_rejected(self):
        self.bad("fix(core): x\n\nKind: review-fix\nsome prose\n" + CF_TRAILER + "\n",
                 "outside the final trailer paragraph")

    def test_lowercase_unknown_kind_rejected(self):
        self.bad("fix(core): x\n\nbody\n\nkind: other\n" + CF_TRAILER + "\n", "Unknown `Kind: other`")

    def test_tab_led_subject_rejected(self):
        self.bad("\tfeat(x): ok\n", "leading whitespace")

    def test_editor_message_with_trailing_comment_block_ok(self):
        self.ok("fix(core): x\n\nbody\n\nKind: review-fix\n" + CF_TRAILER + "\n\n\n"
                "# Please enter the commit message for your changes. Lines starting\n"
                "# with '#' will be ignored.\n#\n# On branch master\n")

    def test_column0_kind_comment_after_trailer_ignored(self):
        self.ok("fix(core): x\n\nbody\n\n" + CF_TRAILER + "\n# Kind: other\n")

    def test_leading_whitespace_subject_rejected(self):
        self.bad("  feat(x): ok\n", "leading whitespace")
        self.bad(" # x\nfeat(x): ok\n", "leading whitespace")


class CommitMsgShim(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not shutil.which("sh"):
            raise unittest.SkipTest("sh not on PATH; the shim would not run")
        if not (shutil.which("python") or shutil.which("python3")):
            raise unittest.SkipTest("neither python nor python3 on PATH; the shim fails open, assertions would be vacuous")
        cls.repo = os.path.join(_TEMP.name, "shim-repo")
        os.makedirs(cls.repo)
        _git(cls.repo, "init", "-q", "-b", "master")
        hooks = os.path.join(cls.repo, ".claude", "hooks")
        os.makedirs(hooks)
        shutil.copy(os.path.join(HOOKS, "commit_format.py"), hooks)
        dest = os.path.join(cls.repo, ".git", "hooks", "commit-msg")
        shutil.copy(os.path.join(HOOKS, "git-commit-msg.sh"), dest)
        os.chmod(dest, 0o755)
        plans = os.path.join(cls.repo, "docs", "plans")
        os.makedirs(plans)
        with open(os.path.join(plans, "2026-01-01-0000-demo-plan.md"), "w") as f:
            f.write("x\n")

    def commit(self, msg):
        return subprocess.run(
            ["git", "-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false",
             "commit", "-q", "--allow-empty", "-m", msg],
            cwd=self.repo, capture_output=True, text=True, env=ENV, timeout=60)

    def test_bad_subject_fails_with_checker_text(self):
        r = self.commit("add stuff")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("Bad commit subject", r.stderr)

    def test_unknown_slug_fails_with_checker_text(self):
        r = self.commit("feat(core): add (nosuch step 1)")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("matches no file under docs/plans", r.stderr)

    def test_hash_led_m_message_fails_with_hash_text(self):
        r = subprocess.run(
            ["git", "-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false",
             "commit", "-q", "--allow-empty", "-m", "# x", "-m", "feat(x): ok"],
            cwd=self.repo, capture_output=True, text=True, env=ENV, timeout=60)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("starts with '#'", r.stderr)

    def git_commit(self, *args, env=None):
        return subprocess.run(
            ["git", "-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false",
             *args], cwd=self.repo, capture_output=True, text=True, env=env or ENV, timeout=60)

    def test_hash_led_single_line_m_fails_with_hash_text(self):
        r = self.git_commit("commit", "-q", "--allow-empty", "-m", "#123 fix")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("starts with '#'", r.stderr)

    def test_editor_mode_template_comment_then_valid_subject_succeeds(self):
        tpl = os.path.join(_TEMP.name, "shim-template.txt")
        with open(tpl, "w", encoding="utf-8") as f:
            f.write("# hint\n")
        ed = os.path.join(_TEMP.name, "shim-editor.py")
        with open(ed, "w", encoding="utf-8") as f:
            f.write("import sys\n"
                    "with open(sys.argv[1], 'a', encoding='utf-8') as f:\n"
                    "    f.write('feat(core): add a thing (demo step 1)\\n\\n" + TR + "\\n')\n")
        editor = '"%s" "%s"' % (sys.executable.replace("\\", "/"), ed.replace("\\", "/"))
        r = self.git_commit("-c", "commit.template=" + tpl.replace("\\", "/"), "commit", "-q",
                            "--allow-empty", env=dict(ENV, GIT_EDITOR=editor))
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_good_subject_succeeds(self):
        r = self.commit("feat(core): add a thing (demo step 1)")
        self.assertEqual(r.returncode, 0, r.stderr)


class PostCommitFixture(unittest.TestCase):
    """Real git repo in a temp dir; the hook classifies the real `git show` output."""

    BASE = (
        "ADVERSARIAL DIFF REVIEW (planning-dispatch.md - MANDATORY): a commit just "
        "landed. Dispatch ONE `core-reviewer` subagent (subagent_type: \"core-reviewer\") "
        "on it (`git show <sha>`) in a separate context, explicit model: >= the author's "
        "tier. It routes by files touched through docs/reference/core-failure-taxonomy.md "
        "and returns a binary verdict or a punch list."
    )
    PROOF = (
        " This commit touches a verify stage or golden file \u2014 also dispatch "
        "`proof-auditor`."
    )
    UNKNOWN = (
        " If it touches tools/{verifyctl,archcheck,conformance,topiccheck} or a golden "
        "file, also dispatch `proof-auditor`."
    )

    def reminder(self, which, tool="Bash"):
        out = "[master %s] %s\n 1 file changed, 1 insertion(+)\n" % (FIX_SHAS[which], which)
        r = subprocess.run(
            [sys.executable, "-B", os.path.join(HOOKS, "post_commit_review_reminder.py")],
            input=json.dumps(pc("git commit -m x", tool, out)), capture_output=True,
            text=True, cwd=ROOT, env=ENV, timeout=60, encoding="utf-8")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stderr, "")
        if not r.stdout.strip():
            return None
        o = json.loads(r.stdout)["hookSpecificOutput"]
        self.assertEqual(o["hookEventName"], "PostToolUse")
        return o["additionalContext"]

    def test_code_commit_base_reminder(self):
        for tool in SHELL_TOOLS:
            self.assertEqual(self.reminder("code", tool), self.BASE)

    def test_prose_commit_silent(self):
        for tool in SHELL_TOOLS:
            self.assertIsNone(self.reminder("prose", tool))

    def test_verifyctl_commit_adds_proof_auditor(self):
        for tool in SHELL_TOOLS:
            self.assertEqual(self.reminder("proof", tool), self.BASE + self.PROOF)

    def test_merge_commit_is_unknown(self):
        for tool in SHELL_TOOLS:
            self.assertEqual(self.reminder("merge", tool), self.BASE + self.UNKNOWN)

    def test_unresolvable_sha_is_unknown(self):
        out = "[master deadbee] x\n"
        r = subprocess.run(
            [sys.executable, "-B", os.path.join(HOOKS, "post_commit_review_reminder.py")],
            input=json.dumps(pc("git commit -m x", "Bash", out)), capture_output=True,
            text=True, cwd=ROOT, env=ENV, timeout=60, encoding="utf-8")
        ctx = json.loads(r.stdout)["hookSpecificOutput"]["additionalContext"]
        self.assertEqual(ctx, self.BASE + self.UNKNOWN)


class Wiring(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with open(os.path.join(ROOT, ".claude", "settings.json"), encoding="utf-8") as f:
            cls.hooks = json.load(f)["hooks"]

    @staticmethod
    def script(h):
        return h["command"].split("/.claude/hooks/")[1].rstrip('"')

    def scripts(self, event, matcher):
        found = [g for g in self.hooks[event] if g["matcher"] == matcher]
        self.assertEqual(len(found), 1, (event, matcher))
        return [self.script(h) for h in found[0]["hooks"] if h["type"] == "command"]

    def test_exact_scripts_per_matcher(self):
        want = {
            ("PreToolUse", "Edit|Write|MultiEdit|NotebookEdit"): ["ref_reminder.py"],
            ("PreToolUse", "Bash|PowerShell"): ["ref_reminder.py", "git_safety_hook.py",
                                                "grep_sweep_guard.py"],
            ("PreToolUse", "Agent|EnterWorktree"): ["no_worktree_hook.py"],
            ("PreToolUse", "Agent"): ["explicit_model_guard.py", "haiku_listing_reminder.py"],
            ("PreToolUse", "ExitPlanMode"): ["exit_plan_checklist.py"],
            ("PostToolUse", "Bash|PowerShell"): ["post_commit_review_reminder.py"],
        }
        for (event, matcher), scripts in want.items():
            with self.subTest(event=event, matcher=matcher):
                self.assertEqual(self.scripts(event, matcher), scripts)
        self.assertEqual(set(self.hooks), {"PreToolUse", "PostToolUse"})
        self.assertEqual(sum(len(v) for v in self.hooks.values()), len(want))

    def test_agent_prompt_guardrail_present(self):
        grp = [g for g in self.hooks["PreToolUse"] if g["matcher"] == "Agent"][0]
        prompts = [h for h in grp["hooks"] if h["type"] == "prompt"]
        self.assertEqual(len(prompts), 1)
        self.assertIn("$ARGUMENTS", prompts[0]["prompt"])

    def test_scripts_exist_and_all_wired(self):
        wired = {self.script(h) for groups in self.hooks.values() for g in groups
                 for h in g["hooks"] if h["type"] == "command"}
        for name in wired:
            self.assertTrue(os.path.isfile(os.path.join(HOOKS, name)), name)
        on_disk = {f for f in os.listdir(HOOKS) if f.endswith(".py")} - {"shell_data.py", "commit_format.py"}
        self.assertEqual(wired, on_disk)

    def test_command_form(self):
        for groups in self.hooks.values():
            for g in groups:
                for h in g["hooks"]:
                    if h["type"] != "command":
                        continue
                    self.assertTrue(h["command"].startswith("python "), h["command"])
                    self.assertNotIn("python3", h["command"])
                    self.assertIn("$CLAUDE_PROJECT_DIR", h["command"])


class RefReminder(unittest.TestCase):
    def payload(self, sid, tool, **ti):
        return {"tool_name": tool, "session_id": sid, "cwd": R, "tool_input": ti}

    def test_once_per_session_edit(self):
        sid = str(uuid.uuid4())
        p = self.payload(sid, "Edit", file_path=os.path.join(ROOT, "core", "asyncevents", "src", "lib.rs"))
        self.assertTrue(run("ref_reminder", p).startswith("context:"))
        self.assertEqual(run("ref_reminder", p), "silent")
        p2 = self.payload(str(uuid.uuid4()), "Edit", file_path=p["tool_input"]["file_path"])
        self.assertTrue(run("ref_reminder", p2).startswith("context:"))

    def test_once_per_session_command_both_tools(self):
        for tool in SHELL_TOOLS:
            sid = str(uuid.uuid4())
            p = self.payload(sid, tool, command="cargo run -p verifyctl -- --fast")
            with self.subTest(tool=tool):
                self.assertTrue(run("ref_reminder", p).startswith("context:"))
                self.assertEqual(run("ref_reminder", p), "silent")

    def test_paths(self):
        for rel, want in (("modules/admin/templates/x.html", "context"),
                          ("modules/gateway/src/lib.rs", "context"),
                          ("modules/wallet/src/lib.rs", "silent")):
            with self.subTest(rel=rel):
                p = self.payload(str(uuid.uuid4()), "Write", file_path=R + "/" + rel)
                self.assertTrue(run("ref_reminder", p).startswith(want))


class FailOpen(unittest.TestCase):
    HOOKS = ("git_safety_hook", "no_worktree_hook", "explicit_model_guard",
             "haiku_listing_reminder", "grep_sweep_guard", "ref_reminder",
             "post_commit_review_reminder", "exit_plan_checklist")

    def test_malformed_json(self):
        for hook in self.HOOKS:
            for raw in ("not json{", "", "[]", "null"):
                with self.subTest(hook=hook, raw=raw):
                    got = run(hook, None, raw=raw)
                    self.assertFalse(got.startswith("ERR"), got)
                    self.assertNotIn(got, ("deny", "ask"))
                    if hook != "exit_plan_checklist":
                        self.assertEqual(got, "silent")


if __name__ == "__main__":
    unittest.main()
