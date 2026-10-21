#!/usr/bin/env python
"""PreToolUse(Agent) nudge: denies a listing-only research dispatch whose
`model` is set and is not Haiku, unless the prompt/description carries
`force-model`. A missing model is left to explicit_model_guard.py. Any internal
error exits 0 with no output.
"""
import json
import sys

LISTING_SIGNALS = (
    "list all", "list every", "list the", "enumerate", "enumeration",
    "catalog", "find all", "locate all",
    "zlistuj", "wylistuj", "wymień", "wymien", "enumeracja", "spis", "wypisz",
)

REASONING_SIGNALS = (
    "write", "implement", "refactor", "fix", "edit", "modify", "design",
    "analyze", "analyse", "review", "reason", "decide", "evaluate",
    "compare", "synthesize", "synthesise", "compile", "debug", "trace",
    "explain why", "assess", "critique", "napisz", "zaimplementuj",
    "popraw", "zrefaktor", "przeanalizuj", "zaprojektuj", "oceń", "ocen",
)

OVERRIDE_TOKEN = "force-model"

REASON = (
    "Haiku read-and-list lane (docs/reference/research-mode.md; "
    ".agents/adapters/claude.md → Dispatch Mapping: listing-only research → "
    "model:\"haiku\"): this subagent task reads as pure enumeration/listing (no "
    "reasoning, no code writing), but model is not \"haiku\". Re-issue with "
    "model:\"haiku\". If the listing genuinely needs a bigger model, keep it and "
    "add the token 'force-model' to the description."
)


def main():
    try:
        data = json.load(sys.stdin)
        if (data.get("tool_name") or "") != "Agent":
            return
        tool_input = data.get("tool_input") or {}
        model = str(tool_input.get("model") or "").strip().lower()
        if not model or "haiku" in model:
            return
        text = " ".join((
            str(tool_input.get("prompt") or ""),
            str(tool_input.get("description") or ""),
        )).lower()
        if OVERRIDE_TOKEN in text:
            return
        has_listing = any(sig in text for sig in LISTING_SIGNALS)
        has_reasoning = any(sig in text for sig in REASONING_SIGNALS)
        if has_listing and not has_reasoning:
            print(json.dumps({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": REASON,
                }
            }))
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
