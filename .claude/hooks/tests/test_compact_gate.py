"""compact_gate: auto-compaction waits for the step boundary."""
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest

HOOK = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "compact_gate.py")


class Gate(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.main = os.path.join(self.tmp.name, "sess.jsonl")
        open(self.main, "w").close()
        self.sub = os.path.join(self.tmp.name, "sess", "subagents")
        os.makedirs(self.sub)

    def tearDown(self):
        self.tmp.cleanup()

    def agent(self, aid, finished, age=0):
        path = os.path.join(self.sub, "agent-" + aid + ".jsonl")
        open(path, "w").close()
        if age:
            t = time.time() - age
            os.utime(path, (t, t))
        with open(path[:-6] + ".meta.json", "w", encoding="utf-8") as f:
            json.dump({"description": "step " + aid, **({"finished": True} if finished else {})}, f)

    def run_hook(self, trigger, transcript=None, agent_id=None):
        payload = {"hook_event_name": "PreCompact", "trigger": trigger,
                   "transcript_path": transcript or self.main, "custom_instructions": None}
        if agent_id:
            payload["agent_id"] = agent_id
        r = subprocess.run([sys.executable, "-I", HOOK], input=json.dumps(payload),
                           capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        out = r.stdout.strip()
        if out.startswith("{"):
            return json.loads(out).get("decision")
        return "instructions" if out else "silent"

    def test_auto_blocks_while_a_subagent_works(self):
        self.agent("a1", finished=False)
        self.assertEqual(self.run_hook("auto"), "block")

    def test_auto_runs_when_all_finished(self):
        self.agent("a1", finished=True)
        self.assertEqual(self.run_hook("auto"), "instructions")

    def test_stale_unfinished_agent_does_not_block(self):
        self.agent("a1", finished=False, age=3600)
        self.assertEqual(self.run_hook("auto"), "instructions")

    def test_manual_never_blocks(self):
        self.agent("a1", finished=False)
        self.assertEqual(self.run_hook("manual"), "instructions")

    def test_subagent_auto_blocks_and_manual_is_silent(self):
        path = os.path.join(self.sub, "agent-a1.jsonl")
        self.assertEqual(self.run_hook("auto", transcript=path), "block")
        self.assertEqual(self.run_hook("auto", agent_id="a1"), "block")
        self.assertEqual(self.run_hook("manual", agent_id="a1"), "silent")

    def test_no_subagent_folder(self):
        lone = os.path.join(self.tmp.name, "lone.jsonl")
        open(lone, "w").close()
        self.assertEqual(self.run_hook("auto", transcript=lone), "instructions")

    def test_bad_input_fails_open(self):
        r = subprocess.run([sys.executable, "-I", HOOK], input="nope", capture_output=True, text=True, timeout=30)
        self.assertEqual((r.returncode, r.stdout), (0, ""))


if __name__ == "__main__":
    unittest.main()
