"""context_budget_guard: resume limit on SendMessage and the in-agent context limit."""
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

    def test_send_by_agent_name_resolves(self):
        path = self.agent([300_000])
        with open(path[:-6] + ".meta.json", "w", encoding="utf-8") as f:
            json.dump({"finished": True, "name": "impl-13c"}, f)
        self.assertEqual(self.send(to="impl-13c"), "deny")
        self.assertEqual(self.send(to="impl-13c [77f0b1]"), "deny")
        self.assertEqual(self.send(to="someone-else"), "silent")

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
