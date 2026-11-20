"""fix_brief_guard: plan-step or Diagnosis briefs for implementation lanes, and anchored continuation briefs."""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

HOOKS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HOOK = os.path.join(HOOKS, "fix_brief_guard.py")
REAL_PLAN = "docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md"
PLAN = "docs/plans/2026-01-01-0000-sample-plan.md"
PLAN_TEXT = """# Sample plan

Execution order: 1 -> 13c.2a2a -> 24.a1 -> 9z.

## Steps

### Step 3 - Heading step

- **Step 13d2b - bullet step.** Files.
"""
DIAG = """Fix the empty tab.

Diagnosis:
Answer: the tab count reads the wrong field.
Anchors: core/admin/src/render.rs:120 - count_tabs
Change: core/admin/src/render.rs:120 - read manifest.tabs, not pages
"""
HANDOFF = """
Done: 1a2b3c4 part 1, cargo check -p admin green
Remaining:
- core/admin/src/render.rs:200
  Change: move draw_tabs into tabs.rs
"""


class FixBriefGuard(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        hooks = os.path.join(cls.tmp.name, ".claude", "hooks")
        os.makedirs(hooks)
        shutil.copy(HOOK, hooks)
        cls.hook = os.path.join(hooks, "fix_brief_guard.py")
        plan = os.path.join(cls.tmp.name, *PLAN.split("/"))
        os.makedirs(os.path.dirname(plan))
        with open(plan, "w", encoding="utf-8") as f:
            f.write(PLAN_TEXT)

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def run_hook(self, kind, prompt, hook=None, tool="Agent"):
        payload = {"tool_name": tool, "tool_input": {"subagent_type": kind, "model": "opus", "prompt": prompt}}
        r = subprocess.run([sys.executable, "-I", hook or self.hook], input=json.dumps(payload),
                           capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        if not r.stdout.strip():
            return "silent"
        out = json.loads(r.stdout)["hookSpecificOutput"]
        self.last_reason = out["permissionDecisionReason"]
        return out["permissionDecision"]

    def test_unplanned_fix_is_denied_with_the_diagnosis_route(self):
        for kind in ("core-implementer", "mockup-implementer", "test-author"):
            self.assertEqual(self.run_hook(kind, "Fix the empty tab in core/admin."), "deny", kind)
            self.assertIn("code-scout", self.last_reason)
            self.assertIn('model:\"sonnet\"', self.last_reason)
            self.assertIn("review punch list", self.last_reason)

    def test_plan_step_by_heading_bullet_or_execution_order_passes(self):
        for step in ("3", "13d2b", "13c.2a2a", "24.a1", "9z"):
            prompt = "Do Step {0} of {1}.".format(step, PLAN)
            self.assertEqual(self.run_hook("core-implementer", prompt), "silent", step)
        self.assertEqual(self.run_hook("test-author", "Implement Step 3 of " + PLAN), "silent")

    def test_step_missing_from_plan_or_plan_missing_is_denied(self):
        for prompt in ("Do Step 4 of " + PLAN, "Do Step 13d2 of " + PLAN, "Do Step 13c.2a of " + PLAN,
                       "Do Step 3 of docs/plans/2026-01-01-0000-missing-plan.md", "Do Step 3.",
                       "Fix step 3 of " + PLAN):
            self.assertEqual(self.run_hook("core-implementer", prompt), "deny", prompt)

    def test_real_plan_steps_resolve(self):
        if not os.path.isfile(os.path.join(os.path.dirname(HOOKS), "..", *REAL_PLAN.split("/"))):
            self.skipTest("plan not present")
        for step in ("13c", "13c.2a2a", "13d2b", "13c.1r", "28", "6"):
            prompt = "Do Step {0} of {1}.".format(step, REAL_PLAN)
            self.assertEqual(self.run_hook("core-implementer", prompt, hook=HOOK), "silent", step)
        self.assertEqual(self.run_hook("core-implementer", "Do Step 13q of " + REAL_PLAN, hook=HOOK), "deny")

    def test_diagnosis_with_anchor_and_change_passes(self):
        self.assertEqual(self.run_hook("core-implementer", DIAG), "silent")
        self.assertEqual(self.run_hook("mockup-implementer", DIAG.replace("render.rs", "page.tmpl")), "silent")

    def test_diagnosis_without_anchor_or_change_is_denied(self):
        no_anchor = DIAG.replace("render.rs:120", "render.rs")
        no_change = DIAG.replace("Change:", "Fix it")
        anchor_before = "core/admin/src/render.rs:120\nChange: x\n\nDiagnosis: see above"
        for prompt in (no_anchor, no_change, anchor_before):
            self.assertEqual(self.run_hook("core-implementer", prompt), "deny", prompt)

    def test_continuation_needs_pasted_hand_off(self):
        bare = "Do Step 13c.2a2a of {0}, part 2. Continue from the previous agent's report.".format(PLAN)
        for kind in ("core-implementer", "test-author", "mockup-implementer"):
            self.assertEqual(self.run_hook(kind, bare), "deny", kind)
            self.assertIn("Done:", self.last_reason)
            self.assertEqual(self.run_hook(kind, bare + HANDOFF), "silent", kind)
        docs = "Docs sweep, this is part 2 of 3."
        self.assertEqual(self.run_hook("docs-writer", docs), "deny")
        self.assertEqual(self.run_hook("docs-writer", docs + HANDOFF), "silent")
        no_change = HANDOFF.replace("Change:", "then")
        self.assertEqual(self.run_hook("docs-writer", docs + no_change), "deny")
        self.assertEqual(self.run_hook("docs-writer", "Write the step 5 docs; part 1 of the sweep."), "silent")

    def test_other_lanes_and_tools_pass(self):
        for kind in ("code-scout", "core-reviewer", "Explore", "general-purpose", "docs-writer"):
            self.assertEqual(self.run_hook(kind, "Fix the empty tab."), "silent", kind)
        self.assertEqual(self.run_hook("core-implementer", "Fix it.", tool="Bash"), "silent")

    def test_bad_input_fails_open(self):
        r = subprocess.run([sys.executable, "-I", self.hook], input="not json",
                           capture_output=True, text=True, timeout=30)
        self.assertEqual((r.returncode, r.stdout), (0, ""))


if __name__ == "__main__":
    unittest.main()
