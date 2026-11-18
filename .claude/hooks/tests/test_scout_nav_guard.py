"""Subprocess tests for .claude/hooks/scout_nav_guard.py (record, gate and report modes).

Run from the repo root: python -B -m unittest discover -s .claude/hooks/tests -v
The hook is fed Claude Code style JSON on stdin with TMP/TEMP/TMPDIR pointed at a private
directory, so the real scout-nav state folder is never touched.
"""
import json
import os
import subprocess
import sys
import tempfile
import unittest
import uuid

HOOKS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HOOK = os.environ.get("SCOUT_NAV_HOOK") or os.path.join(HOOKS, "scout_nav_guard.py")
ROOT = os.path.dirname(os.path.dirname(HOOKS))
CWD = "G:/Projects/swarog"

_TEMP = tempfile.TemporaryDirectory()
STATE_DIR = os.path.join(_TEMP.name, "scout-nav")
ENV = dict(os.environ, PYTHONDONTWRITEBYTECODE="1", TEMP=_TEMP.name, TMP=_TEMP.name, TMPDIR=_TEMP.name)

FIND_REFS = (
    "Found 20 references across 7 files:\n\nmodules/characters/src/service.rs:\n"
    "  Line 5:32\n  Line 85:6\n\nmodules/characters/src/lib.rs:\n  Line 12:1\n"
)
GO_IMPL = (
    "Found 4 definitions:\n  modules/characters/src/service.rs:85:20\n"
    "  api/characters/rpc/src/lib.rs:24:1\n"
)
WS_SYMBOL = "Found 1 symbol in workspace:\n\ncore/app/src/lib.rs:\n  RateZeroPolicy (Enum) - Line 67\n"
INCOMING = (
    "Found 1 incoming call:\n\nmodules/wallet/src/ops.rs:\n"
    "  credit (Function) - Line 40 [calls at: 44:9, 51:13]\n"
)
DOC_SYMBOL = "Found 1 symbol:\n\ncore/app/src/lib.rs:\n  RateZeroPolicy (Enum) - Line 67\n"


def tearDownModule():
    _TEMP.cleanup()


def run_hook(mode, payload=None, raw=None, debug=True):
    data = raw if raw is not None else json.dumps(payload)
    args = [sys.executable, "-B", HOOK] + ([mode] if mode else [])
    env = dict(ENV, SCOUT_NAV_DEBUG="1") if debug else {k: v for k, v in ENV.items() if k != "SCOUT_NAV_DEBUG"}
    r = subprocess.run(args, input=data.encode("utf-8"), capture_output=True, cwd=ROOT, env=env, timeout=60)
    return r.returncode, r.stdout.decode("utf-8"), r.stderr.decode("utf-8")


def decision(stdout):
    if not stdout.strip():
        return None
    out = json.loads(stdout)["hookSpecificOutput"]
    assert out["hookEventName"] == "PreToolUse"
    return out


def new_agent():
    return "agent-" + uuid.uuid4().hex[:12]


def state_file(agent_id):
    return os.path.join(STATE_DIR, agent_id + ".jsonl")


def read_state(agent_id):
    with open(state_file(agent_id), encoding="utf-8") as f:
        return [json.loads(line) for line in f if line.strip()]


def scout(agent_id, tool, tool_input, event="PreToolUse", cwd=CWD, **extra):
    p = {"agent_type": "code-scout", "agent_id": agent_id, "tool_name": tool, "tool_input": tool_input,
         "hook_event_name": event, "cwd": cwd}
    p.update(extra)
    return p


def lsp(agent_id, op, file, line, text, count=None, event="PostToolUse"):
    extra = {}
    if event == "PostToolUse":
        extra["tool_response"] = {"result": text, "resultCount": count if count is not None else text.count("\n")}
    return scout(agent_id, "LSP", {"operation": op, "filePath": file, "line": line, "character": 1},
                 event=event, **extra)


def record(payload):
    rc, out, err = run_hook("record", payload)
    assert rc == 0 and not out and not err, (rc, out, err)


def gate(payload):
    rc, out, err = run_hook("gate", payload)
    assert rc == 0 and not err, (rc, err)
    return decision(out)


def report(agent_id, message):
    rc, out, err = run_hook("report", scout(agent_id, "SubagentHandback", {"message": message}))
    assert rc == 0 and not err, (rc, err)
    return decision(out)


def grep_tool(agent_id, **tool_input):
    return scout(agent_id, "Grep", dict({"pattern": "foo"}, **tool_input))


def bash(agent_id, command, cwd=CWD):
    return scout(agent_id, "Bash", {"command": command}, cwd=cwd)


def anchor(path_line, label, symbol="Sym"):
    return "- `%s` \u00b7 %s \u00b7 something \u00b7 `%s`" % (path_line, symbol, label)


def message(anchor_lines, header="2. **Anchors:**", answer="Answer text.", method="Method text.", open_="None."):
    return "\n".join(["1. **Answer:** " + answer, header] + anchor_lines
                     + ["3. **Method:** " + method, "4. **Open:** " + open_])


class NoOp(unittest.TestCase):
    WIDE = {"pattern": "foo"}

    def assert_silent(self, mode, payload=None, raw=None, debug=True):
        rc, out, err = run_hook(mode, payload, raw, debug)
        self.assertEqual((rc, out.strip(), err), (0, "", ""))

    def test_main_session_without_agent_type(self):
        aid = new_agent()
        p = grep_tool(aid, **self.WIDE)
        del p["agent_type"]
        self.assert_silent("gate", p)
        self.assertFalse(os.path.exists(state_file(aid)))

    def test_other_agent_with_code_scout_in_pattern(self):
        aid = new_agent()
        p = scout(aid, "Grep", {"pattern": "foo", "note": "code-scout"})
        p["agent_type"] = "core-reviewer"
        self.assertIn('"code-scout"', json.dumps(p))
        self.assert_silent("gate", p)
        self.assertFalse(os.path.exists(state_file(aid)))

    def test_other_agent_record_writes_nothing(self):
        aid = new_agent()
        p = lsp(aid, "findReferences", "core/app/src/lib.rs", 1, FIND_REFS)
        p["agent_type"] = "core-reviewer"
        p["tool_input"]["note"] = "code-scout"
        self.assert_silent("record", p)
        self.assertFalse(os.path.exists(state_file(aid)))

    def test_code_scout_without_agent_id(self):
        p = grep_tool("x", **self.WIDE)
        del p["agent_id"]
        self.assert_silent("gate", p)

    def test_garbage_and_empty_stdin(self):
        for mode in ("gate", "record", "report"):
            for raw in ("", "not json {", "[]", "null", "\"code-scout\"", "{\"agent_type\": \"code-scout\"", "\xff"):
                with self.subTest(mode=mode, raw=raw):
                    self.assert_silent(mode, raw=raw, debug=False)

    def test_no_mode_is_silent(self):
        self.assert_silent("", grep_tool(new_agent(), **self.WIDE))


class GateDeny(unittest.TestCase):
    TOOL_CASES = [
        ("no path", {}),
        ("path core", {"path": "core"}),
        ("path repo parent", {"path": "G:/Projects"}),
        ("path ..", {"path": ".."}),
        ("glob *.rs", {"glob": "*.rs"}),
        ("glob *.[rt]s", {"glob": "*.[rt]s"}),
        ("glob *.r*", {"glob": "*.r*"}),
        ("glob *.r?", {"glob": "*.r?"}),
        ("glob mod.rs", {"glob": "mod.rs"}),
        ("glob lib*.rs", {"glob": "lib*.rs"}),
        ("glob negative only", {"glob": "!*.md"}),
        ("type rust", {"type": "rust"}),
        ("path modules glob *.rs", {"path": "modules", "glob": "*.rs"}),
    ]
    BASH_CASES = [
        "rg -trs foo",
        "rg foo",
        "git grep foo -- ':!*.md'",
        "git grep foo -- ':(exclude)*.md'",
        "git grep foo -- '*.rs'",
        "git grep foo",
        "git grep foo -- ':(top)*.rs'",
        "git grep foo -- ':/*.rs'",
        "git grep foo -- ':(glob)*.rs'",
        "git grep foo -- ':(top)*.md'",
        "cd modules && rg foo",
    ]

    def assert_denied(self, payload):
        got = gate(payload)
        self.assertIsNotNone(got, payload)
        self.assertEqual(got["permissionDecision"], "deny")
        self.assertIn("position-based LSP call", got["permissionDecisionReason"])

    def test_grep_tool_wide(self):
        for name, tool_input in self.TOOL_CASES:
            with self.subTest(case=name):
                self.assert_denied(grep_tool(new_agent(), **tool_input))

    def test_bash_wide(self):
        for command in self.BASH_CASES:
            with self.subTest(command=command):
                self.assert_denied(bash(new_agent(), command))

    def test_top_pathspec_resolves_from_repo_root_not_cwd(self):
        wallet = CWD + "/modules/wallet"
        self.assert_denied(bash(new_agent(), "git grep foo -- ':(top)core'", wallet))
        self.assert_denied(bash(new_agent(), "git grep foo -- ':/core'", wallet))

    def test_cd_out_of_a_narrow_cwd_widens_the_search(self):
        wallet = CWD + "/modules/wallet"
        self.assert_denied(bash(new_agent(), "cd ../.. && rg foo", wallet))
        self.assert_denied(bash(new_agent(), "cd .. && rg foo", wallet))

    def test_workspace_symbol_and_document_symbol_do_not_open_the_gate(self):
        aid = new_agent()
        record(lsp(aid, "workspaceSymbol", "core/app/src/lib.rs", 1, WS_SYMBOL, 1))
        record(lsp(aid, "documentSymbol", "core/app/src/lib.rs", 1, DOC_SYMBOL, 1))
        self.assert_denied(grep_tool(aid))


class GateAllow(unittest.TestCase):
    TOOL_CASES = [
        ("path modules/wallet", {"path": "modules/wallet"}),
        ("path .idea", {"path": ".idea"}),
        ("path weles", {"path": "weles"}),
        ("path docs", {"path": "docs"}),
        ("glob *trust*.md", {"glob": "*trust*.md"}),
        ("glob *.{md,toml}", {"glob": "*.{md,toml}"}),
        ("type md", {"type": "md"}),
        ("path core/app/src/lib.rs", {"path": "core/app/src/lib.rs"}),
    ]
    BASH_CASES = [
        ("rg --files core", CWD),
        ("rg -g '*.sh' foo .", CWD),
        ("git grep foo -- ':!*.md' modules/wallet", CWD),
        ("git grep foo -- ':!*.md'", CWD + "/modules/wallet"),
        ("rg foo modules/wallet", CWD),
        ("git grep foo -- ':(top)modules/wallet/*.rs'", CWD),
        ("git grep foo -- ':(top)modules/wallet/*.rs'", CWD + "/modules/wallet"),
        ("cd modules/wallet && rg foo", CWD),
        ("git -C modules/wallet grep foo", CWD),
        ("git log | grep foo", CWD),
        ("echo hello", CWD),
    ]

    def test_grep_tool_narrow(self):
        for name, tool_input in self.TOOL_CASES:
            with self.subTest(case=name):
                self.assertIsNone(gate(grep_tool(new_agent(), **tool_input)))

    def test_bash_narrow(self):
        for command, cwd in self.BASH_CASES:
            with self.subTest(command=command, cwd=cwd):
                self.assertIsNone(gate(bash(new_agent(), command, cwd)))

    def test_allowed_search_records_no_denial(self):
        aid = new_agent()
        self.assertIsNone(gate(grep_tool(aid, path="modules/wallet")))
        self.assertFalse(os.path.exists(state_file(aid)))


class GateOpensAfterLsp(unittest.TestCase):
    def test_wide_grep_allowed_after_position_lsp_for_each_op(self):
        for op in ("findReferences", "incomingCalls", "outgoingCalls", "goToImplementation", "hover",
                   "goToDefinition", "prepareCallHierarchy"):
            with self.subTest(op=op):
                aid = new_agent()
                self.assertEqual(gate(grep_tool(aid))["permissionDecision"], "deny")
                record(lsp(aid, op, "core/app/src/lib.rs", 67, FIND_REFS, 3))
                self.assertIsNone(gate(grep_tool(aid)))

    def test_wide_bash_grep_allowed_after_position_lsp(self):
        aid = new_agent()
        record(lsp(aid, "findReferences", "core/app/src/lib.rs", 67, FIND_REFS, 3))
        self.assertIsNone(gate(bash(aid, "rg -trs foo")))

    def test_recorded_empty_result_still_opens_the_fallback(self):
        aid = new_agent()
        record(lsp(aid, "findReferences", "core/app/src/lib.rs", 1, "No references found", 0))
        self.assertFalse(read_state(aid)[0]["ok"])
        self.assertIsNone(gate(grep_tool(aid)))

    def test_recorded_failure_event_opens_the_fallback(self):
        aid = new_agent()
        record(lsp(aid, "findReferences", "core/app/src/lib.rs", 1, "", event="PostToolUseFailure"))
        self.assertFalse(read_state(aid)[0]["ok"])
        self.assertIsNone(gate(grep_tool(aid)))

    def test_lsp_of_another_agent_does_not_open_this_gate(self):
        record(lsp(new_agent(), "findReferences", "core/app/src/lib.rs", 1, FIND_REFS, 3))
        self.assertEqual(gate(grep_tool(new_agent()))["permissionDecision"], "deny")


class DenyCap(unittest.TestCase):
    def test_third_wide_grep_without_lsp_is_allowed(self):
        aid = new_agent()
        self.assertEqual(gate(grep_tool(aid))["permissionDecision"], "deny")
        self.assertEqual(gate(grep_tool(aid))["permissionDecision"], "deny")
        self.assertIsNone(gate(grep_tool(aid)))
        self.assertIsNone(gate(bash(aid, "rg foo")))
        self.assertEqual([r["kind"] for r in read_state(aid)], ["gate-deny", "gate-deny"])


class RecordParsing(unittest.TestCase):
    def recorded(self, op, file, line, text, count):
        aid = new_agent()
        record(lsp(aid, op, file, line, text, count))
        (rec,) = read_state(aid)
        return rec

    def test_find_references_header_and_line_entries(self):
        rec = self.recorded("findReferences", "G:/Projects/swarog/modules/characters/src/service.rs", 85, FIND_REFS, 20)
        self.assertEqual(rec["kind"], "lsp")
        self.assertEqual(rec["op"], "findReferences")
        self.assertEqual(rec["file"], "modules/characters/src/service.rs")
        self.assertEqual(rec["line"], 85)
        self.assertTrue(rec["ok"])
        self.assertEqual(rec["hits"], [["modules/characters/src/service.rs", 5],
                                       ["modules/characters/src/service.rs", 85],
                                       ["modules/characters/src/lib.rs", 12]])

    def test_go_to_implementation_path_line_col_targets(self):
        rec = self.recorded("goToImplementation", "api/characters/rpc/src/lib.rs", 10, GO_IMPL, 4)
        self.assertEqual(rec["hits"], [["modules/characters/src/service.rs", 85],
                                       ["api/characters/rpc/src/lib.rs", 24]])

    def test_workspace_symbol_hit(self):
        rec = self.recorded("workspaceSymbol", "core/app/src/lib.rs", 1, WS_SYMBOL, 1)
        self.assertEqual(rec["hits"], [["core/app/src/lib.rs", 67]])

    def test_incoming_calls_line_and_calls_at(self):
        rec = self.recorded("incomingCalls", "modules/wallet/src/ops.rs", 40, INCOMING, 1)
        self.assertEqual(rec["hits"], [["modules/wallet/src/ops.rs", 40],
                                       ["modules/wallet/src/ops.rs", 44],
                                       ["modules/wallet/src/ops.rs", 51]])

    def test_empty_result_count_zero_is_not_ok(self):
        rec = self.recorded("findReferences", "core/app/src/lib.rs", 1, "No references found", 0)
        self.assertFalse(rec["ok"])
        self.assertEqual(rec["hits"], [])

    def test_failure_event_is_not_ok(self):
        aid = new_agent()
        record(lsp(aid, "hover", "core/app/src/lib.rs", 1, "", event="PostToolUseFailure"))
        self.assertFalse(read_state(aid)[0]["ok"])

    def test_string_tool_response_without_count_uses_text(self):
        aid = new_agent()
        p = lsp(aid, "findReferences", "core/app/src/lib.rs", 1, "x")
        p["tool_response"] = FIND_REFS
        record(p)
        self.assertTrue(read_state(aid)[0]["ok"])
        p = lsp(aid, "findReferences", "core/app/src/lib.rs", 1, "x")
        p["tool_response"] = "No references found"
        record(p)
        self.assertFalse(read_state(aid)[1]["ok"])

    def test_non_lsp_tool_is_not_recorded(self):
        aid = new_agent()
        record(scout(aid, "Grep", {"pattern": "foo"}, event="PostToolUse", tool_response={"result": "x"}))
        self.assertFalse(os.path.exists(state_file(aid)))


class Report(unittest.TestCase):
    def assert_deny(self, got, *needles):
        self.assertIsNotNone(got)
        self.assertEqual(got["permissionDecision"], "deny")
        for needle in needles:
            self.assertIn(needle, got["permissionDecisionReason"])

    def assert_pass(self, aid, msg):
        self.assertIsNone(report(aid, msg))
        self.assertFalse(os.path.exists(state_file(aid)))

    def test_confirmed_covered_by_find_references_hit(self):
        aid = new_agent()
        record(lsp(aid, "findReferences", "modules/other/src/x.rs", 900, FIND_REFS, 20))
        msg = message([anchor("modules/characters/src/service.rs:86", "confirmed")])
        self.assertTrue(os.path.exists(state_file(aid)))
        self.assert_pass(aid, msg)

    def test_confirmed_beyond_slack_is_not_covered(self):
        aid = new_agent()
        record(lsp(aid, "findReferences", "modules/other/src/x.rs", 900, FIND_REFS, 20))
        got = report(aid, message([anchor("modules/characters/src/service.rs:88", "confirmed")]))
        self.assert_deny(got, "labelled confirmed but no LSP call covers it")

    def test_confirmed_covered_by_go_to_implementation_target(self):
        aid = new_agent()
        record(lsp(aid, "goToImplementation", "modules/other/src/x.rs", 900, GO_IMPL, 4))
        self.assert_pass(aid, message([anchor("api/characters/rpc/src/lib.rs:24", "confirmed")]))

    def test_confirmed_covered_by_workspace_symbol_hit(self):
        aid = new_agent()
        record(lsp(aid, "workspaceSymbol", "modules/other/src/x.rs", 900, WS_SYMBOL, 1))
        self.assert_pass(aid, message([anchor("core/app/src/lib.rs:67", "confirmed")]))

    def test_confirmed_covered_by_position_call_input_location(self):
        aid = new_agent()
        record(lsp(aid, "goToDefinition", "G:/Projects/swarog/core/app/src/lib.rs", 66, "Found 1 definition", 1))
        self.assert_pass(aid, message([anchor("core/app/src/lib.rs:67", "confirmed")]))

    def test_confirmed_covered_only_by_document_symbol_is_denied(self):
        aid = new_agent()
        record(lsp(aid, "documentSymbol", "core/app/src/lib.rs", 67, DOC_SYMBOL, 1))
        got = report(aid, message([anchor("core/app/src/lib.rs:67", "confirmed")]))
        self.assert_deny(got, "core/app/src/lib.rs:67 labelled confirmed but no LSP call covers it")

    def test_confirmed_covered_only_by_empty_lsp_is_denied(self):
        aid = new_agent()
        record(lsp(aid, "findReferences", "core/app/src/lib.rs", 67, FIND_REFS, 0))
        got = report(aid, message([anchor("core/app/src/lib.rs:67", "confirmed")]))
        self.assert_deny(got, "labelled confirmed")

    def test_unlabelled_rs_anchor_denied_with_full_path(self):
        aid = new_agent()
        got = report(aid, message(["- `modules/characters/src/service.rs:85` \u00b7 Sym \u00b7 does x"]))
        self.assert_deny(got, "modules/characters/src/service.rs:85 has no label")

    def test_unlabelled_non_rs_anchor_passes(self):
        self.assert_pass(new_agent(), message(["- `docs/reference/testing.md:12` \u00b7 doc \u00b7 text"]))

    def test_grep_only_labelled_passes_and_removes_state(self):
        aid = new_agent()
        record(lsp(aid, "findReferences", "core/app/src/lib.rs", 1, FIND_REFS, 3))
        self.assert_pass(aid, message([anchor("modules/characters/src/service.rs:85", "grep-only")]))

    def test_line_with_grep_only_and_confirmed_is_grep_only(self):
        for tail in ("`confirmed` · grep-only", "grep only · confirmed", "`grep-only` `confirmed`"):
            with self.subTest(tail=tail):
                aid = new_agent()
                self.assert_pass(aid, message(["- `modules/a/src/x.rs:10` · Sym · " + tail]))

    def test_not_confirmed_is_treated_as_unlabelled(self):
        for word in ("not confirmed", "unconfirmed", "Not Confirmed"):
            with self.subTest(word=word):
                aid = new_agent()
                got = report(aid, message(["- `core/app/src/lib.rs:67` \u00b7 Sym \u00b7 x \u00b7 " + word]))
                self.assert_deny(got, "core/app/src/lib.rs:67 has no label")

    def test_anchors_outside_the_anchors_section_are_ignored(self):
        aid = new_agent()
        msg = message(
            [anchor("modules/a/src/ok.rs:3", "grep-only")],
            answer="see modules/a/src/answer.rs:10 and core/app/src/lib.rs:1 confirmed",
            method="modules/b/src/method.rs:5 confirmed, modules/b/src/m2.rs:6",
            open_="modules/c/src/open.rs:7 unresolved",
        )
        self.assert_pass(aid, msg)

    def test_numbered_anchors_header_is_parsed(self):
        for header in ("2. **Anchors:**", "**Anchors:**", "Anchors:", "## Anchors", "2. Anchors"):
            with self.subTest(header=header):
                got = report(new_agent(), message([anchor("modules/a/src/x.rs:10", "confirmed")], header=header))
                self.assert_deny(got, "modules/a/src/x.rs:10 labelled confirmed")

    def test_without_anchors_header_bullets_before_method_are_parsed(self):
        msg = "\n".join([
            "1. **Answer:** done",
            "- `modules/a/src/x.rs:10` \u00b7 Sym \u00b7 text",
            "3. **Method:** none",
            "- `modules/z/src/after.rs:1` \u00b7 Sym \u00b7 ignored",
        ])
        got = report(new_agent(), msg)
        self.assert_deny(got, "modules/a/src/x.rs:10 has no label")
        self.assertNotIn("after.rs", got["permissionDecisionReason"])

    def test_without_anchors_header_only_bullets_before_method_or_open_count(self):
        for stop in ("3. **Method:** none", "4. **Open:** none"):
            with self.subTest(stop=stop):
                msg = "\n".join([
                    "1. **Answer:** see modules/a/src/answer.rs:10 confirmed",
                    "* `modules/a/src/star.rs:1` · Sym · text",
                    "• `modules/a/src/dot.rs:2` · Sym · text",
                    stop,
                    "- `modules/z/src/after.rs:1` · Sym · ignored",
                ])
                reason = report(new_agent(), msg)["permissionDecisionReason"]
                self.assertIn("modules/a/src/star.rs:1 has no label", reason)
                self.assertIn("modules/a/src/dot.rs:2 has no label", reason)
                self.assertNotIn("answer.rs", reason)
                self.assertNotIn("after.rs", reason)

    def test_anchor_windows_path_is_normalised_to_full_path_in_reason(self):
        got = report(new_agent(), message(["- `G:\\Projects\\swarog\\core\\app\\src\\lib.rs:67` \u00b7 Sym \u00b7 x"]))
        self.assert_deny(got, "G:/Projects/swarog/core/app/src/lib.rs:67 has no label")

    def test_two_denials_then_allow_with_hook_note(self):
        aid = new_agent()
        msg = message([anchor("modules/a/src/x.rs:10", "confirmed")], answer="caf\u00e9 \u00b7 result")
        self.assert_deny(report(aid, msg), "labelled confirmed")
        self.assert_deny(report(aid, msg), "labelled confirmed")
        got = report(aid, msg)
        self.assertEqual(got["permissionDecision"], "allow")
        updated = got["updatedInput"]["message"]
        self.assertTrue(updated.startswith(msg.rstrip()))
        self.assertIn("\u00b7", updated)
        self.assertIn("caf\u00e9", updated)
        self.assertIn("\n\n**Hook:** unverified anchors: modules/a/src/x.rs:10 labelled confirmed", updated)
        self.assertFalse(os.path.exists(state_file(aid)))

    def test_allow_keeps_other_tool_input_fields(self):
        aid = new_agent()
        msg = message([anchor("modules/a/src/x.rs:10", "confirmed")])
        for _ in range(2):
            report(aid, msg)
        rc, out, err = run_hook("report", scout(aid, "SubagentHandback", {"message": msg, "extra": 1}))
        self.assertEqual(json.loads(out)["hookSpecificOutput"]["updatedInput"]["extra"], 1)

    def test_clean_report_after_denial_passes_and_removes_state(self):
        aid = new_agent()
        self.assert_deny(report(aid, message([anchor("modules/a/src/x.rs:10", "confirmed")])), "confirmed")
        record(lsp(aid, "goToDefinition", "modules/a/src/x.rs", 10, "Found 1 definition", 1))
        self.assert_pass(aid, message([anchor("modules/a/src/x.rs:10", "confirmed")]))


NAV_AGENTS = ("core-implementer", "test-author", "docs-writer")
SRC = "modules/inventory/src/lib.rs"


def as_agent(payload, agent):
    payload["agent_type"] = agent
    return payload


def nav_grep(aid, agent, **tool_input):
    return as_agent(grep_tool(aid, **tool_input), agent)


def nav_bash(aid, agent, command, cwd=CWD):
    return as_agent(bash(aid, command, cwd), agent)


def nav_lsp(aid, agent, op, file, line, text, count=None):
    return as_agent(lsp(aid, op, file, line, text, count), agent)


def slice_records(aid):
    if not os.path.exists(state_file(aid)):
        return []
    return [r for r in read_state(aid) if r["kind"] == "slice"]


class NavSearchGate(unittest.TestCase):
    WIDE = [
        ("Grep no path", lambda aid, a: nav_grep(aid, a)),
        ("rg foo modules", lambda aid, a: nav_bash(aid, a, "rg foo modules")),
        ("git grep -n foo", lambda aid, a: nav_bash(aid, a, "git grep -n foo")),
    ]
    SCOPED = [
        ("rg foo modules/inventory", lambda aid, a: nav_bash(aid, a, "rg foo modules/inventory")),
        ("Grep path core/app", lambda aid, a: nav_grep(aid, a, path="core/app")),
        ("Grep glob *.md", lambda aid, a: nav_grep(aid, a, glob="*.md")),
    ]

    def assert_nav_deny(self, got, agent):
        self.assertIsNotNone(got)
        self.assertEqual(got["permissionDecision"], "deny")
        reason = got["permissionDecisionReason"]
        self.assertIn("Ask LSP first", reason)
        self.assertTrue(reason.startswith(agent + ":"), reason)

    def test_wide_search_denied_twice_then_allowed(self):
        for agent in NAV_AGENTS:
            for name, make in self.WIDE:
                with self.subTest(agent=agent, case=name):
                    aid = new_agent()
                    self.assert_nav_deny(gate(make(aid, agent)), agent)
                    self.assert_nav_deny(gate(make(aid, agent)), agent)
                    self.assertIsNone(gate(make(aid, agent)))
                    self.assertEqual([r["kind"] for r in read_state(aid)], ["gate-deny", "gate-deny"])

    def test_scoped_search_allowed_without_state(self):
        for agent in NAV_AGENTS:
            for name, make in self.SCOPED:
                with self.subTest(agent=agent, case=name):
                    aid = new_agent()
                    self.assertIsNone(gate(make(aid, agent)))
                    self.assertFalse(os.path.exists(state_file(aid)))

    def test_workspace_symbol_alone_keeps_the_gate_closed(self):
        for agent in NAV_AGENTS:
            with self.subTest(agent=agent):
                aid = new_agent()
                record(nav_lsp(aid, agent, "workspaceSymbol", "core/app/src/lib.rs", 1, WS_SYMBOL, 1))
                self.assert_nav_deny(gate(nav_grep(aid, agent)), agent)

    def test_position_lsp_record_opens_the_gate(self):
        for agent in NAV_AGENTS:
            with self.subTest(agent=agent):
                aid = new_agent()
                self.assert_nav_deny(gate(nav_grep(aid, agent)), agent)
                record(nav_lsp(aid, agent, "findReferences", "core/app/src/lib.rs", 67, FIND_REFS, 3))
                self.assertEqual(read_state(aid)[-1]["kind"], "lsp")
                self.assertIsNone(gate(nav_grep(aid, agent)))
                self.assertIsNone(gate(nav_bash(aid, agent, "rg foo modules")))

    def test_code_scout_dispatch_opens_the_gate(self):
        for agent in NAV_AGENTS:
            with self.subTest(agent=agent):
                aid = new_agent()
                self.assert_nav_deny(gate(nav_grep(aid, agent)), agent)
                record(as_agent(scout(aid, "Agent", {"subagent_type": "code-scout", "prompt": "q"},
                                      event="PostToolUse", tool_response={"result": "ok"}), agent))
                self.assertEqual(read_state(aid)[-1], {"kind": "scout"})
                self.assertIsNone(gate(nav_grep(aid, agent)))

    def test_general_purpose_dispatch_does_not_open_the_gate(self):
        for agent in NAV_AGENTS:
            with self.subTest(agent=agent):
                aid = new_agent()
                record(as_agent(scout(aid, "Agent", {"subagent_type": "general-purpose", "prompt": "q"},
                                      event="PostToolUse", tool_response={"result": "ok"}), agent))
                self.assertFalse(os.path.exists(state_file(aid)))
                self.assert_nav_deny(gate(nav_grep(aid, agent)), agent)

    def test_nav_ok_passes_the_search_gate(self):
        for agent in NAV_AGENTS:
            for command in ("rg foo # nav-ok", "rg foo modules #nav-ok"):
                with self.subTest(agent=agent, command=command):
                    aid = new_agent()
                    self.assertIsNone(gate(nav_bash(aid, agent, command)))
                    self.assertFalse(os.path.exists(state_file(aid)))

    def test_nav_ok_does_not_pass_the_slice_gate(self):
        aid = new_agent()
        command = "sed -n '1,5p' %s # nav-ok" % SRC
        self.assertIsNone(gate(nav_bash(aid, "core-implementer", command)))
        self.assertIsNone(gate(nav_bash(aid, "core-implementer", command)))
        got = gate(nav_bash(aid, "core-implementer", command))
        self.assertEqual(got["permissionDecision"], "deny")
        self.assertIn("already sliced", got["permissionDecisionReason"])


class NavExcluded(unittest.TestCase):
    MARK = {"note": "code-scout"}

    def assert_silent_all(self, make):
        aid = new_agent()
        for command in ("rg foo", "sed -n '1p' %s" % SRC):
            for _ in range(4):
                rc, out, err = run_hook("gate", make(aid, command))
                self.assertEqual((rc, out.strip(), err), (0, "", ""))
        rc, out, err = run_hook("gate", make(aid, None))
        self.assertEqual((rc, out.strip(), err), (0, "", ""))
        self.assertFalse(os.path.exists(state_file(aid)))

    @staticmethod
    def payload(aid, command, agent):
        if command is None:
            p = scout(aid, "Grep", dict({"pattern": "foo"}, **NavExcluded.MARK))
        else:
            p = scout(aid, "Bash", dict({"command": command}, **NavExcluded.MARK))
        if agent is None:
            del p["agent_type"]
            del p["agent_id"]
        else:
            p["agent_type"] = agent
        return p

    def test_other_agents_and_main_session_are_silent(self):
        for agent in ("core-reviewer", "proof-auditor", "mockup-implementer", None):
            with self.subTest(agent=agent):
                self.assert_silent_all(lambda aid, command, a=agent: self.payload(aid, command, a))

    def test_nav_agent_without_agent_id_is_silent(self):
        p = self.payload(new_agent(), "rg foo", "core-implementer")
        del p["agent_id"]
        rc, out, err = run_hook("gate", p)
        self.assertEqual((rc, out.strip(), err), (0, "", ""))

    def test_other_agent_record_and_report_write_nothing(self):
        aid = new_agent()
        p = as_agent(lsp(aid, "findReferences", "core/app/src/lib.rs", 1, FIND_REFS), "core-reviewer")
        p["tool_input"]["note"] = "core-implementer"
        record(p)
        rc, out, err = run_hook("report", as_agent(scout(aid, "SubagentHandback", {"message": "core-implementer"}), "core-reviewer"))
        self.assertEqual((rc, out.strip(), err), (0, "", ""))
        self.assertFalse(os.path.exists(state_file(aid)))


class CodeScoutKeepsItsOwnGate(unittest.TestCase):
    def test_code_scout_reason_is_its_own_not_the_nav_reason(self):
        got = gate(grep_tool(new_agent()))
        reason = got["permissionDecisionReason"]
        self.assertIn("position-based LSP call", reason)
        self.assertNotIn("Ask LSP first", reason)
        self.assertNotIn("dispatch code-scout", reason)

    def test_code_scout_is_not_opened_by_a_scout_dispatch(self):
        aid = new_agent()
        record(scout(aid, "Agent", {"subagent_type": "code-scout"}, event="PostToolUse", tool_response={"result": "ok"}))
        self.assertEqual(gate(grep_tool(aid))["permissionDecision"], "deny")

    def test_code_scout_has_no_slice_gate(self):
        aid = new_agent()
        for _ in range(5):
            self.assertIsNone(gate(bash(aid, "sed -n '1,5p' %s" % SRC)))
        self.assertFalse(os.path.exists(state_file(aid)))


class SliceGate(unittest.TestCase):
    AGENT = "core-implementer"

    def run_slice(self, aid, command, cwd=CWD):
        return gate(nav_bash(aid, self.AGENT, command, cwd))

    def assert_slice_deny(self, got, file=SRC):
        self.assertIsNotNone(got)
        self.assertEqual(got["permissionDecision"], "deny")
        reason = got["permissionDecisionReason"]
        self.assertIn("already sliced 2 times", reason)
        self.assertIn(file, reason)
        self.assertNotIn("Ask LSP first", reason)

    def test_third_slice_denied_then_cap_allows(self):
        for agent in NAV_AGENTS:
            with self.subTest(agent=agent):
                aid = new_agent()
                command = "sed -n '1,5p' %s" % SRC
                self.assertIsNone(gate(nav_bash(aid, agent, command)))
                self.assertIsNone(gate(nav_bash(aid, agent, command)))
                self.assert_slice_deny(gate(nav_bash(aid, agent, command)))
                self.assert_slice_deny(gate(nav_bash(aid, agent, command)))
                self.assertIsNone(gate(nav_bash(aid, agent, command)))
                self.assertEqual([r["kind"] for r in read_state(aid)],
                                 ["slice", "slice", "slice-deny", "slice-deny", "slice"])

    def test_different_files_do_not_share_a_count(self):
        aid = new_agent()
        for name in ("a", "b", "c"):
            for _ in range(2):
                self.assertIsNone(self.run_slice(aid, "sed -n '1,5p' modules/inventory/src/%s.rs" % name))

    def test_path_forms_share_one_key(self):
        forms = [
            "sed -n '1,5p' " + SRC,
            "sed -n '6,9p' G:/Projects/swarog/" + SRC,
            "sed -n '10p' /g/Projects/swarog/" + SRC,
            "cd modules/inventory && sed -n '2p' src/lib.rs",
            "cd " + CWD + "/modules/inventory && head -n 3 src/lib.rs",
        ]
        for third in range(len(forms)):
            with self.subTest(third=forms[third]):
                aid = new_agent()
                firsts = [form for index, form in enumerate(forms) if index != third][:2]
                for form in firsts:
                    self.assertIsNone(self.run_slice(aid, form))
                self.assert_slice_deny(self.run_slice(aid, forms[third]))
                self.assertEqual({r["file"] for r in slice_records(aid)}, {SRC})

    def test_cwd_relative_path_shares_the_key(self):
        aid = new_agent()
        wallet = CWD + "/modules/inventory"
        self.assertIsNone(self.run_slice(aid, "sed -n '1p' src/lib.rs", wallet))
        self.assertIsNone(self.run_slice(aid, "sed -n '2p' src/lib.rs", wallet))
        self.assert_slice_deny(self.run_slice(aid, "sed -n '3p' " + SRC))

    def test_head_and_tail_forms_are_counted(self):
        forms = [
            "head -n 5 " + SRC, "head -n5 " + SRC, "head -5 " + SRC, "head --lines=5 " + SRC,
            "tail -n 5 " + SRC, "tail -n5 " + SRC, "tail -5 " + SRC, "tail --lines=5 " + SRC,
            "tail -n +5 " + SRC, "sed -n '$p' " + SRC, "sed -n -e '3,4p' " + SRC,
        ]
        for form in forms:
            with self.subTest(form=form):
                aid = new_agent()
                self.assertIsNone(self.run_slice(aid, "sed -n '1p' " + SRC))
                self.assertIsNone(self.run_slice(aid, "sed -n '2p' " + SRC))
                self.assert_slice_deny(self.run_slice(aid, form))

    def test_head_and_tail_count_towards_the_same_file_total(self):
        aid = new_agent()
        self.assertIsNone(self.run_slice(aid, "head -n 5 " + SRC))
        self.assertIsNone(self.run_slice(aid, "tail -n 5 " + SRC))
        self.assert_slice_deny(self.run_slice(aid, "sed -n '9p' " + SRC))

    def assert_never_counted(self, command, cwd=CWD):
        aid = new_agent()
        for _ in range(5):
            self.assertIsNone(self.run_slice(aid, command, cwd), command)
        self.assertEqual(slice_records(aid), [], command)

    def test_not_counted_forms(self):
        outside = os.path.join(_TEMP.name, "x.rs").replace("\\", "/")
        cases = [
            "git log | head -n 5",
            "cat " + SRC + " | head -n 5",
            "sed '5p' " + SRC,
            "sed -n 's/a/b/p' " + SRC,
            "sed -n '1p'",
            "head " + SRC,
            "head -c 100 " + SRC,
            "tail -f " + SRC,
            "sed -n '1p' target/debug/x.rs",
            "head -n 5 run/server.log",
            "tail -n 5 deploy/weles/x.log",
            "tail -n 40 target/t.log",
            "sed -n '1p' " + outside,
            "head -n 5 " + outside,
            "sed -n '1p' /tmp/x.rs",
            "head -n 5 ../x.rs",
            "head -n 5 C:/Users/x/y.rs",
        ]
        for command in cases:
            with self.subTest(command=command):
                self.assert_never_counted(command)

    def test_redirected_file_in_same_command_is_not_counted(self):
        for command in (
            "cargo test > target/t.log 2>&1; tail -n 40 target/t.log",
            "cargo test > notes/t.log 2>&1; tail -n 40 notes/t.log",
            "cargo test &> notes/t.log; tail -n 40 notes/t.log",
            "cargo test >> notes/t.log; head -n 40 notes/t.log",
            "cd notes && cargo test > t.log; tail -n 40 t.log",
        ):
            with self.subTest(command=command):
                self.assert_never_counted(command)

    def test_same_file_without_redirect_is_counted(self):
        aid = new_agent()
        self.assertIsNone(self.run_slice(aid, "tail -n 40 notes/t.log"))
        self.assertIsNone(self.run_slice(aid, "tail -n 40 notes/t.log"))
        self.assert_slice_deny(self.run_slice(aid, "tail -n 40 notes/t.log"), "notes/t.log")

    def test_redirect_in_an_earlier_command_does_not_exempt_a_later_one(self):
        aid = new_agent()
        self.assertIsNone(self.run_slice(aid, "cargo test > notes/t.log"))
        for _ in range(2):
            self.assertIsNone(self.run_slice(aid, "tail -n 40 notes/t.log"))
        self.assert_slice_deny(self.run_slice(aid, "tail -n 40 notes/t.log"), "notes/t.log")

    def test_denied_slice_is_not_recorded_as_a_slice(self):
        aid = new_agent()
        for _ in range(4):
            self.run_slice(aid, "sed -n '1p' " + SRC)
        self.assertEqual(len(slice_records(aid)), 2)

    def test_slice_gate_does_not_deny_a_search(self):
        aid = new_agent()
        self.assertIsNone(self.run_slice(aid, "rg foo modules/inventory"))
        self.assertEqual(slice_records(aid), [])


class NavReport(unittest.TestCase):
    def test_nav_agent_state_deleted_at_handback(self):
        for agent in NAV_AGENTS:
            with self.subTest(agent=agent):
                aid = new_agent()
                self.assertIsNotNone(gate(nav_grep(aid, agent)))
                self.assertTrue(os.path.exists(state_file(aid)))
                rc, out, err = run_hook("report", as_agent(
                    scout(aid, "SubagentHandback", {"message": "done " + SRC + ":1"}), agent))
                self.assertEqual((rc, out.strip(), err), (0, "", ""))
                self.assertFalse(os.path.exists(state_file(aid)))
                self.assertIsNotNone(gate(nav_grep(aid, agent)))

    def test_nav_agent_report_without_state_is_silent(self):
        rc, out, err = run_hook("report", as_agent(
            scout(new_agent(), "SubagentHandback", {"message": "x"}), "core-implementer"))
        self.assertEqual((rc, out.strip(), err), (0, "", ""))


if __name__ == "__main__":
    unittest.main()
