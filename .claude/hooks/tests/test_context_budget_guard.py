"""context_budget_guard: resume limit on SendMessage, the in-agent context limit and its REPL script analysis."""
import json
import os
import subprocess
import sys
import tempfile
import unittest

HOOK = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "context_budget_guard.py")
AID = "a0123456789abcdef"


def turn(ctx, model="claude-opus-5-5"):
    return json.dumps({"type": "assistant", "message": {"model": model, "usage": {
        "input_tokens": 10, "cache_creation_input_tokens": 0, "cache_read_input_tokens": ctx - 10}}})


class Guard(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.main = os.path.join(self.tmp.name, "sess.jsonl")
        open(self.main, "w").close()
        self.sub = os.path.join(self.tmp.name, "sess", "subagents")
        os.makedirs(self.sub)

    def tearDown(self):
        self.tmp.cleanup()

    def agent(self, ctxs, finished=True, synthetic_last=False, aid=AID):
        path = os.path.join(self.sub, "agent-" + aid + ".jsonl")
        lines = [turn(c) for c in ctxs]
        if synthetic_last:
            lines.append(turn(999_999, "<synthetic>"))
        with open(path, "w", encoding="utf-8") as f:
            f.write("\n".join(lines) + "\n")
        with open(path[:-6] + ".meta.json", "w", encoding="utf-8") as f:
            json.dump({"finished": finished}, f)
        return path

    def run_hook(self, mode, payload):
        r = subprocess.run([sys.executable, "-I", HOOK, mode], input=json.dumps(payload),
                           capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        if not r.stdout.strip():
            return "silent"
        return json.loads(r.stdout)["hookSpecificOutput"]["permissionDecision"]

    def send(self, to=AID, agent_id=None):
        p = {"tool_name": "SendMessage", "transcript_path": self.main, "tool_input": {"to": to, "message": "x"}}
        if agent_id:
            p["agent_id"] = agent_id
        return self.run_hook("send", p)

    def work(self, tool, command=None, transcript=None):
        p = {"tool_name": tool, "agent_id": AID, "transcript_path": transcript or self.main,
             "tool_input": {"command": command} if command else {"file_path": "x"}}
        return self.run_hook("work", p)

    def repl(self, code, agent_type=None):
        p = {"tool_name": "REPL", "agent_id": AID, "transcript_path": self.main, "tool_input": {"code": code}}
        if agent_type:
            p["agent_type"] = agent_type
        return self.run_hook("work", p)

    def test_repl_over_limit_denies_reading_scripts(self):
        self.agent([200_000], finished=False)
        for code in ("const t = await file('core/a.rs').text()",
                     "await tools.Read({file_path: 'a.rs'})",
                     "await tools.Grep({pattern: 'x'})",
                     "await tools.LSP({operation: 'hover'})",
                     "await tools.WebFetch({url: 'u'})",
                     "await tools.Agent({subagent_type: 'code-scout', model: 'haiku', prompt: 'q'})",
                     "const out = await $`rg -n foo ${dir}`.text()",
                     "await $`cd G:/p && cat a.rs`",
                     "const cmd = 'sed -n 1,40p a.rs'; await $(cmd)",
                     "await $('rg foo src')",
                     "await $(`grep -n x ${dir}`, 5000)",
                     "await $`echo ${'`'} && cat a.rs`",
                     "const out = await $`rg ${`a${b}`} src`",
                     "log(`${(await file(p).text()).length}`)",
                     "const s = `${await tools.Read({file_path: 'a.rs'})}`",
                     "const s = `x ${`y ${file('a.rs')}`}`",
                     "await write('x.rs', 'y'); await $('git status'); await $('cat a.rs')"):
            self.assertEqual(self.repl(code), "deny", code)

    def test_repl_over_limit_unparseable_script_checks_only_its_commands(self):
        self.agent([200_000], finished=False)
        self.assertEqual(self.repl("const a = 'open; await $`rg foo src`"), "deny")
        self.assertEqual(self.repl("const a = 'open; await $('cat a.rs')"), "deny")
        self.assertEqual(self.repl("const a = 'open; await write('x.rs', `fn file(a) {}`)"), "silent")

    def test_repl_over_limit_lets_edits_builds_and_git_run(self):
        self.agent([200_000], finished=False)
        for code in ("await edit('a.rs', 'file(x)', 'rg -n y')",
                     "await write('a.md', 'cat a.rs and tools.Read here')",
                     "// file('a.rs') is read later\nawait $`cargo check -p admin`",
                     "/* tools.Grep */ await $`git commit -m x`",
                     "const re = /file\\(/; await $`git status`",
                     "await $`cargo test -p admin 2>&1 | tail -n 40`",
                     "await $(`git log -1`)",
                     "await $`echo ${'`'}`; await write('x.rs', `fn file(a) {}`)",
                     "const s = `${`fn file(a){}`}`; await write('x.rs', `fn file(a) {}`)",
                     "await write('a.rs', 'type Foo = u8;'); await $('git status')",
                     "await write('a.md', `Intro\nFind the cause\nmore`); await $('cargo check')",
                     "await $`git commit -m \"$(cat msg)\"`; await write('a.rs', 'type Foo = u8;')",
                     "const cmd = 'git status'; const t = 'cat x'; await $(cmd)",
                     "const profile = 1; myfile(2)",
                     ""):
            self.assertEqual(self.repl(code), "silent", code)

    def test_repl_below_limit_is_silent(self):
        self.agent([170_000], finished=False)
        self.assertEqual(self.repl("await file('a.rs').text()"), "silent")

    def test_read_only_lane_is_told_to_return_its_report(self):
        self.agent([200_000], finished=False)
        for lane in ("researcher", "general-purpose", "Explore", "Plan", "code-scout", "core-reviewer",
                     "proof-auditor"):
            p = {"tool_name": "Read", "agent_id": AID, "agent_type": lane, "transcript_path": self.main,
                 "tool_input": {"file_path": "x"}}
            out = self.run_hook_raw(p)
            self.assertEqual(out["permissionDecision"], "deny", lane)
            self.assertIn("Not checked:", out["permissionDecisionReason"], lane)
        p = {"tool_name": "Read", "agent_id": AID, "agent_type": "core-implementer", "transcript_path": self.main,
             "tool_input": {"file_path": "x"}}
        out = self.run_hook_raw(p)
        self.assertIn("part hand-off", out["permissionDecisionReason"])
        self.assertNotIn("Not checked:", out["permissionDecisionReason"])

    def run_hook_raw(self, payload):
        r = subprocess.run([sys.executable, "-I", HOOK, "work"], input=json.dumps(payload),
                           capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        return json.loads(r.stdout)["hookSpecificOutput"]

    def test_send_denies_large_finished_agent(self):
        self.agent([20_000, 150_000])
        self.assertEqual(self.send(), "deny")

    def test_send_allows_small_agent(self):
        self.agent([20_000, 90_000])
        self.assertEqual(self.send(), "silent")

    def test_send_allows_running_agent(self):
        self.agent([20_000, 300_000], finished=False)
        self.assertEqual(self.send(), "silent")

    def test_send_ignores_synthetic_usage(self):
        self.agent([20_000, 50_000], synthetic_last=True)
        self.assertEqual(self.send(), "silent")

    def test_send_by_name_or_from_subagent_is_silent(self):
        self.agent([300_000])
        self.assertEqual(self.send(to="main"), "silent")
        self.assertEqual(self.send(agent_id="aother"), "silent")

    def test_send_unknown_agent_is_silent(self):
        self.assertEqual(self.send(to="a99999999999"), "silent")

    def test_send_strips_a_ref_suffix(self):
        self.agent([300_000])
        self.assertEqual(self.send(to=AID + " [77f0b1]"), "deny")

    def test_work_below_limit_is_silent(self):
        self.agent([170_000], finished=False)
        self.assertEqual(self.work("Read"), "silent")
        self.assertEqual(self.work("Bash", "rg foo core"), "silent")

    def test_work_over_limit_denies_exploration(self):
        self.agent([200_000], finished=False)
        for tool in ("Read", "Grep", "Glob", "LSP", "Agent"):
            self.assertEqual(self.work(tool), "deny", tool)
        for command in ("rg -n foo core", "cat a.rs", "sed -n 1,40p a.rs", "git status; rg foo",
                        "cd G:/p && grep -n x a.rs", "FOO=1 head a.rs", "C:/bin/rg.exe x"):
            self.assertEqual(self.work("Bash", command), "deny", command)
        for command in ("Get-Content a.rs", "Set-Location G:/p; Select-String -Path a.rs -Pattern x"):
            self.assertEqual(self.work("PowerShell", command), "deny", command)

    def test_work_over_limit_lets_the_part_finish(self):
        self.agent([300_000], finished=False)
        for tool in ("Edit", "Write", "REPL", "Poll", "SubagentHandback", "SendMessage"):
            self.assertEqual(self.work(tool), "silent", tool)
        for command in ("cargo check -p admin", "cargo test -p admin", "git add -- core/a.rs",
                        "git -c core.x=y commit -m x", "git --no-pager log -1", "git diff --stat",
                        "cd G:/p && git commit -m x"):
            self.assertEqual(self.work("Bash", command), "silent", command)
        self.assertEqual(self.work("PowerShell", "Set-Location G:/p; git commit -m x"), "silent")
        for command in ("cargo test -p admin 2>&1 | tail -n 40", "git log | head -5",
                        "git commit -m \"fix(x): y\n\nMore text\nType here | head of it\"",
                        "git commit -m \"$(cat <<'EOF'\nfix(x): y\n\nMore lines\nFind this\nEOF\n)\"",
                        "echo \"a | head b\""):
            self.assertEqual(self.work("Bash", command), "silent", command)
        self.assertEqual(self.work("PowerShell", "git commit -m @'\nfix(x): y\n\nMore\n'@"), "silent")

    def test_work_resolves_a_subagent_transcript_path(self):
        path = self.agent([250_000], finished=False)
        self.assertEqual(self.work("Read", transcript=path), "deny")

    def test_main_session_work_is_silent(self):
        r = self.run_hook("work", {"tool_name": "Read", "transcript_path": self.main, "tool_input": {}})
        self.assertEqual(r, "silent")

    def test_bad_input_fails_open(self):
        r = subprocess.run([sys.executable, "-I", HOOK, "work"], input="not json",
                           capture_output=True, text=True, timeout=30)
        self.assertEqual((r.returncode, r.stdout), (0, ""))


if __name__ == "__main__":
    unittest.main()
