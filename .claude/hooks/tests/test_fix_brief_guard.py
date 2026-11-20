"""fix_brief_guard: plan-step or Diagnosis briefs for implementation lanes, and anchored continuation briefs."""
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

HOOKS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HOOK = os.path.join(HOOKS, "fix_brief_guard.py")
REAL_PLAN = "docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md"
PLAN = "docs/plans/2026-01-01-0000-sample-plan.md"
OTHER_PLAN = "docs/plans/2026-01-02-0000-other-plan.md"
PLAN_TEXT = """# Sample plan

Execution order: 1 -> 13c.2a2a -> 24.a1 -> 9z.

## Steps

### Step 3 - Heading step

- **Step 13d2b - bullet step.** Files.
  - **14a1** (Na1): a bold sub-step.
- **25b — a bold step.** Files.
  - **25c1:** a colon sub-step.
"""
OTHER_TEXT = """# Other plan

### Step 6c - only here
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
        with open(os.path.join(cls.tmp.name, *OTHER_PLAN.split("/")), "w", encoding="utf-8") as f:
            f.write(OTHER_TEXT)

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
        for step in ("3", "13d2b", "13c.2a2a", "24.a1", "9z", "14a1", "25b", "25c1"):
            prompt = "Do Step {0} of {1}.".format(step, PLAN)
            self.assertEqual(self.run_hook("core-implementer", prompt), "silent", step)
        self.assertEqual(self.run_hook("test-author", "Implement Step 3 of " + PLAN), "silent")

    def test_step_missing_from_plan_or_plan_missing_is_denied(self):
        for prompt in ("Do Step 4 of " + PLAN, "Do Step 13d2 of " + PLAN, "Do Step 13c.2a of " + PLAN,
                       "Do Step 3 of docs/plans/2026-01-01-0000-missing-plan.md", "Do Step 3.",
                       "Fix step 3 of " + PLAN, "Do Step 14a of " + PLAN, "Do Step 25 of " + PLAN):
            self.assertEqual(self.run_hook("core-implementer", prompt), "deny", prompt)

    def test_each_step_binds_to_the_plan_in_its_sentence(self):
        cross = "Do Step 6c of {0}; see also {1}.".format(PLAN, OTHER_PLAN)
        self.assertEqual(self.run_hook("core-implementer", cross), "deny")
        own = "Do Step 6c of {1}; see also {0}.".format(PLAN, OTHER_PLAN)
        self.assertEqual(self.run_hook("core-implementer", own), "silent")
        both = "Do Step 3 of {0}.\nDo Step 6c of {1}.".format(PLAN, OTHER_PLAN)
        self.assertEqual(self.run_hook("core-implementer", both), "silent")
        one_bad = "Do Step 3 of {0}.\nDo Step 4 of {0}.".format(PLAN)
        self.assertEqual(self.run_hook("core-implementer", one_bad), "deny")
        unbound = "Do Step 3.\nPlans: {0}, {1}.".format(PLAN, OTHER_PLAN)
        self.assertEqual(self.run_hook("core-implementer", unbound), "deny")
        only_plan = "Do Step 3.\nPlan: {0}.".format(PLAN)
        self.assertEqual(self.run_hook("core-implementer", only_plan), "silent")

    def test_real_plan_steps_resolve(self):
        real = os.path.join(os.path.dirname(HOOKS), "..", *REAL_PLAN.split("/"))
        if not os.path.isfile(real):
            self.skipTest("plan not present")
        with open(real, encoding="utf-8") as f:
            text = f.read()
        ids = set(re.findall(r"^#{2,4}\s*Steps?\s+(\d+[0-9a-z]*(?:\.[0-9a-z]+)*)", text, re.M))
        ids |= set(re.findall(r"^\s*(?:[-*]\s+)?\*\*(?:Steps?\s+)?(\d+[0-9a-z]*(?:\.[0-9a-z]+)*)(?![\w-]|\.\w)",
                              text, re.M))
        self.assertGreater(len(ids), 60)
        spec = importlib.util.spec_from_file_location("fix_brief_guard", HOOK)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        missed = sorted(i for i in ids if not guard.plan_step_ok("Do Step {0} of {1}.".format(i, REAL_PLAN)))
        self.assertEqual(missed, [])
        for step in ("13c", "13c.2a2a", "13d2b", "13c.1r", "28", "6", "14a1", "15a1", "25b", "25c", "25h3"):
            prompt = "Do Step {0} of {1}.".format(step, REAL_PLAN)
            self.assertEqual(self.run_hook("core-implementer", prompt, hook=HOOK), "silent", step)
        self.assertEqual(self.run_hook("core-implementer", "Do Step 13q of " + REAL_PLAN, hook=HOOK), "deny")

    def test_diagnosis_with_anchor_and_change_passes(self):
        self.assertEqual(self.run_hook("core-implementer", DIAG), "silent")
        self.assertEqual(self.run_hook("mockup-implementer", DIAG.replace("render.rs", "page.tmpl")), "silent")
        for path in ("android/app/src/Main.kt", "deploy/compose.yml", "web/src/app.ts"):
            self.assertEqual(self.run_hook("core-implementer", DIAG.replace("core/admin/src/render.rs", path)),
                             "silent", path)

    def test_diagnosis_without_anchor_or_change_is_denied(self):
        no_anchor = DIAG.replace("render.rs:120", "render.rs")
        no_change = DIAG.replace("Change:", "Fix it")
        anchor_before = "core/admin/src/render.rs:120\nChange: x\n\nDiagnosis: see above"
        clock = DIAG.replace("core/admin/src/render.rs:120", "at 12:30")
        for prompt in (no_anchor, no_change, anchor_before, clock):
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
        clock = HANDOFF.replace("core/admin/src/render.rs:200", "at 12:30")
        self.assertEqual(self.run_hook("docs-writer", docs + clock), "deny")
        kt = HANDOFF.replace("core/admin/src/render.rs:200", "android/app/Main.kt:40")
        self.assertEqual(self.run_hook("docs-writer", docs + kt), "silent")
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
