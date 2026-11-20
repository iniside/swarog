"""Subprocess tests for .claude/hooks/view_guard.py.

Run directly: python .claude/hooks/tests/test_view_guard.py
Each test feeds the hook Claude Code style PreToolUse JSON on stdin. The files it views live in a private
temporary directory (cwd of the payload), and the agent's earlier tool calls come from a transcript the
test writes there.
"""
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
import uuid

HOOKS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HOOK = os.path.join(HOOKS, "view_guard.py")
sys.path.insert(0, HOOKS)
import view_guard  # noqa: E402

_TEMP = tempfile.TemporaryDirectory()
ROOT = _TEMP.name.replace("\\", "/")

RUST = """//! A long module.
use std::fmt;

mod inner;

pub struct Unit;

pub(crate) struct Tuple(u8, [u8; 4]);

/// Braces in strings, chars and comments do not count: "{" '{' /* { */
pub struct Config<'a> {
    name: &'a str,
    raw: &'static str,
}

impl<'a> Config<'a> {
    pub fn new(name: &'a str) -> Self {
        let raw = r#"{ "a": { "b": 1 } }"#;
        let brace = '{';
        let s = format!("{{{}}}", name);
        fn helper_inside() -> u8 { 1 }
        Config { name, raw: "}" }
    }

    pub async fn run(&self)
    where
        Self: Sized,
    {
        let _ = self.name;
    }
}

pub trait Shape {
    fn area(&self) -> f64;
    fn name(&self) -> &str {
        "shape"
    }
}

macro_rules! twice {
    ($e:expr) => {{ fn not_listed() {} $e; $e }};
}

#[cfg(test)]
mod tests {
    #[test]
    fn works() {
        assert_eq!(1, 1);
    }
}

pub enum Kind {
    A,
    B { x: u8 },
}
"""


def write(rel, text):
    path = os.path.join(ROOT, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    return path.replace("\\", "/")


FENCE = "~" * 3
LONG_RS = write("core/big/src/lib.rs", RUST + "\n".join(f"// filler {k}" for k in range(400)) + "\n")
OTHER_RS = write("core/big/src/other.rs", "\n".join(f"fn f{k}() {{}}" for k in range(400)) + "\n")
SHORT_RS = write("core/big/src/short.rs", "fn small() {}\n" * 50)
LONG_MD = write("docs/reference/guide.md", "# Guide\n\n## One\n" + "text\n" * 200 + f"## Two\n{FENCE}\n# not a heading\n{FENCE}\n"
                + "text\n" * 200)
LONG_PLAN = write("docs/plans/2026-11-20-1200-x-plan.md", "# Plan\n" + "step\n" * 600)
LONG_LOG = write("run/out.log", "line\n" * 900)
LONG_JS = write("core/admin/static/admin.js", "export function boot() {\n}\nconst go = async (x) => x;\n" + "// js\n" * 400)
LONG_CSS = write("core/admin/static/portal.css", ".a { color: red; }\n" * 400)
BT = "\x60"


def tearDownModule():
    _TEMP.cleanup()


def transcript(calls):
    """A subagent transcript with the given (tool, input) tool_use blocks, oldest first."""
    path = os.path.join(ROOT, "subagents", "agent-" + uuid.uuid4().hex + ".jsonl")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        for tool, inp in calls:
            block = {"type": "tool_use", "id": "toolu_" + uuid.uuid4().hex, "name": tool, "input": inp}
            f.write(json.dumps({"message": {"role": "assistant", "content": [block]}}) + "\n")
    return path


def run(tool, inp, agent="core-implementer", calls=(), main=False, timeout=30):
    payload = {"hook_event_name": "PreToolUse", "tool_name": tool, "tool_input": inp, "cwd": ROOT,
               "tool_use_id": "toolu_now", "session_id": "s"}
    if not main:
        payload.update(agent_id="a" + uuid.uuid4().hex[:8], agent_type=agent, transcript_path=transcript(calls))
    r = subprocess.run([sys.executable, "-I", "-B", HOOK], input=json.dumps(payload).encode("utf-8"),
                       capture_output=True, timeout=timeout)
    assert r.returncode == 0, r.stderr
    out = r.stdout.decode("utf-8").strip()
    if not out:
        return None
    o = json.loads(out)["hookSpecificOutput"]
    assert o["hookEventName"] == "PreToolUse" and o["permissionDecision"] == "deny"
    return o["permissionDecisionReason"]


def repl(code, **kw):
    return run("REPL", {"code": code}, **kw)


class Scope(unittest.TestCase):
    def test_main_session_and_other_agents_are_no_ops(self):
        self.assertIsNone(run("Read", {"file_path": LONG_RS}, main=True))
        self.assertIsNone(run("Read", {"file_path": LONG_RS}, agent="Explore"))

    def test_every_lane_is_covered(self):
        for agent in ("core-implementer", "test-author", "docs-writer", "mockup-implementer", "code-scout",
                      "core-reviewer", "proof-auditor", "general-purpose"):
            self.assertIsNotNone(run("Read", {"file_path": LONG_RS}, agent=agent), agent)

    def test_garbage_input_fails_open(self):
        r = subprocess.run([sys.executable, "-I", "-B", HOOK], input=b'{"agent_type": nope', capture_output=True,
                           timeout=30)
        self.assertEqual((r.returncode, r.stdout), (0, b""))


class Read(unittest.TestCase):
    def test_whole_long_rust_file_denied_with_outline(self):
        reason = run("Read", {"file_path": LONG_RS})
        self.assertIn("lines", reason)
        self.assertIn("documentSymbol", reason)
        self.assertIn("impl<'a> Config<'a>", reason)

    def test_limit_tail_short_file_and_plan_pass(self):
        self.assertIsNone(run("Read", {"file_path": LONG_RS, "offset": 1, "limit": 100}))
        self.assertIsNone(run("Read", {"file_path": LONG_RS, "offset": 300}))
        self.assertIsNone(run("Read", {"file_path": SHORT_RS}))
        self.assertIsNone(run("Read", {"file_path": LONG_PLAN}))

    def test_offset_far_from_the_end_is_a_whole_view(self):
        self.assertIsNotNone(run("Read", {"file_path": LONG_RS, "offset": 10}))

    def test_every_file_type_is_guarded(self):
        for path in (LONG_MD, LONG_LOG, LONG_JS, LONG_CSS):
            self.assertIsNotNone(run("Read", {"file_path": path}), path)

    def test_markdown_outline_has_section_ranges_and_skips_fences(self):
        reason = run("Read", {"file_path": LONG_MD})
        self.assertRegex(reason, r"3-\d+\s+## One")
        self.assertNotIn("# not a heading", reason)

    def test_other_outlines(self):
        self.assertIn("export function boot", run("Read", {"file_path": LONG_JS}))
        self.assertIn("const go = async", run("Read", {"file_path": LONG_JS}))
        self.assertIn("rg -n", run("Read", {"file_path": LONG_CSS}))


class RustOutline(unittest.TestCase):
    def items(self):
        return {label: (a, b, d) for a, b, d, label in view_guard.rs_items(LONG_RS)}

    def line(self, text, after=0):
        return RUST.split("\n").index(text, after) + 1

    def test_items_have_their_ranges(self):
        items = self.items()
        start = self.line("pub struct Config<'a> {")
        self.assertEqual(items["pub struct Config<'a>"], (start, self.line("}", start), 0))
        impl = items["impl<'a> Config<'a>"]
        self.assertEqual(impl[0], self.line("impl<'a> Config<'a> {"))
        self.assertEqual(items["pub fn new(name: &'a str) -> Self"], (impl[0] + 1, impl[0] + 7, 1))
        self.assertEqual(items["pub async fn run(&self)"][1], impl[1] - 1)
        self.assertEqual(items["pub enum Kind"][1], len(RUST.split("\n")) - 1)

    def test_declarations_end_at_their_semicolon(self):
        items = self.items()
        for label in ("mod inner;", "pub struct Unit;", "pub(crate) struct Tuple(u8, [u8; 4]);", "fn area(&self) -> f64;"):
            a, b, _ = items[label]
            self.assertEqual(a, b, label)

    def test_bodies_of_fns_and_macros_are_not_listed(self):
        items = self.items()
        self.assertNotIn("fn helper_inside() -> u8", items)
        self.assertFalse(any("not_listed" in x for x in items))
        self.assertIn("macro_rules! twice", items)
        self.assertEqual(items["fn works()"][2], 1)
        self.assertEqual(items["fn name(&self) -> &str"][2], 1)

    def test_outline_is_capped(self):
        reason = run("Read", {"file_path": OTHER_RS})
        self.assertIn(f"{400 - view_guard.OUTLINE_CAP} more items", reason)


class Shell(unittest.TestCase):
    def test_viewers_of_a_long_file_denied(self):
        self.assertIsNotNone(run("Bash", {"command": f"cat {LONG_RS}"}))
        self.assertIsNotNone(run("Bash", {"command": "cat core/big/src/lib.rs"}))
        self.assertIsNotNone(run("PowerShell", {"command": f"Get-Content '{LONG_RS}'"}))
        self.assertIsNotNone(run("Bash", {"command": f"sed -n '1,400p' {LONG_RS}"}))

    def test_cut_views_pass(self):
        for cmd in (f"cat {LONG_RS} | head -50", f"sed -n '1,100p' {LONG_RS}", f"cat {LONG_RS} > copy.rs",
                    f"Get-Content {LONG_RS} -TotalCount 40", f"cat {SHORT_RS}", f"cat {LONG_PLAN}",
                    f"cat {LONG_RS} # view-ok"):
            self.assertIsNone(run("Bash", {"command": cmd}), cmd)


class Repl(unittest.TestCase):
    def test_whole_text_returned_or_logged_denied(self):
        for code in (f"await file('{LONG_RS}').text()",
                     f"const t = await file('{LONG_RS}').text();\nlog(t)",
                     f"const p = '{LONG_RS}';\nconst t = await file(p).lines();\nt.join('\\n')",
                     f"({{a: file('{LONG_RS}').text()}})",
                     f"const t = await file('{LONG_RS}').text();\npreview(t, 9000)",
                     f"await ${BT}cat {LONG_RS}{BT}.text()"):
            reason = repl(code)
            self.assertIsNotNone(reason, code)
            self.assertIn("file(X).slice(offset, limit)", reason)

    def test_cut_or_unreturned_text_passes(self):
        for code in (f"const t = await file('{LONG_RS}').text();\nt.split('\\n').length",
                     f"const t = await file('{LONG_RS}').text();\npreview(t, 2000)",
                     f"const t = await file('{LONG_RS}').text();\nawait write('x', t);\n'ok'",
                     f"await file('{LONG_RS}').slice(1, 100)",
                     f"await file('{LONG_PLAN}').text()",
                     f"await file('{LONG_RS}').text() // view-ok"):
            self.assertIsNone(repl(code), code)

    def test_name_from_an_earlier_script_resolves(self):
        calls = [("REPL", {"code": f"const P = '{LONG_RS}';\n'set'"})]
        self.assertIsNotNone(repl("await file(P).text()", calls=calls))
        self.assertIsNone(repl("await file(P).text()"))

    def test_script_budget(self):
        two = f"({{a: file('{LONG_RS}').slice(1, 200), b: file('{OTHER_RS}').slice(1, 200)}})"
        self.assertIn("about 400 lines", repl(two))
        self.assertIsNone(repl(f"({{a: file('{LONG_RS}').slice(1, 100), b: file('{OTHER_RS}').slice(1, 100)}})"))
        self.assertIsNone(repl(two + " // view-ok"))

    def test_budget_counts_tools_read_and_own_range_helpers(self):
        code = (f"const show = (p, a, b) => ${BT}sed -n ${{a}},${{b}}p ${{p}}{BT}.text();\n"
                f"({{x: show('{LONG_RS}', 1, 250), y: tools.Read({{file_path: '{OTHER_RS}', limit: 100}})}})")
        self.assertIn("about 350 lines", repl(code))

    def test_long_identifiers_do_not_hang_the_name_scan(self):
        code = ("const x = " + "a" * 60 + ".foo()\n") * 30 + f"await file('{LONG_RS}').text()"
        start = time.monotonic()
        self.assertIsNotNone(repl(code, timeout=20))
        self.assertLess(time.monotonic() - start, 10)


class Escape(unittest.TestCase):
    def test_second_escaped_whole_read_of_one_file_denied(self):
        first = f"await file('{LONG_RS}').text() // view-ok"
        self.assertIn("already read whole", repl(first, calls=[("REPL", {"code": first})]))
        self.assertIsNone(repl(f"await file('{OTHER_RS}').text() // view-ok", calls=[("REPL", {"code": first})]))
        # a Bash call is taken as run by the newest REPL script, which is left out of the earlier reads
        later = ("REPL", {"code": "'unrelated'"})
        self.assertIn("already read whole", run("Bash", {"command": f"cat {LONG_RS} # view-ok"},
                                                calls=[("REPL", {"code": first}), later]))

    def test_bash_run_by_a_repl_script_is_not_its_own_repeat(self):
        script = f"const f = '{LONG_RS}';\nawait ${BT}cat ${{f}} # view-ok{BT}.text()"
        cmd = f"cat '{LONG_RS}' # view-ok"
        self.assertIsNone(run("Bash", {"command": cmd}, calls=[("REPL", {"code": script})]))
        other = f"await file('{OTHER_RS}').text() // view-ok"
        self.assertIsNone(run("Bash", {"command": cmd}, calls=[("REPL", {"code": script}), ("REPL", {"code": other})]))
        self.assertIsNotNone(run("Bash", {"command": cmd},
                                 calls=[("REPL", {"code": script}), ("REPL", {"code": script})]))


if __name__ == "__main__":
    unittest.main()
