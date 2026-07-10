---
name: verify-session-model-before-claims
description: "Never infer the active Codex session model from public model documentation; distinguish session identity, model window, and usable remaining context"
metadata:
  node_type: memory
  type: feedback
  originSessionId: codex-2026-07-10
---

When asked why the current Codex session showed roughly 350k context, I searched the
public model catalog and asserted that the session was a 400k GPT-5.3-Codex-class
model. The user corrected me: the active model was `gpt-5.6-sol`. Public model docs
described available API models; they did not identify the model backing this session.

**How to apply:** Never identify the active session model from catalog similarity,
context size, or the system's generic model-family wording. Use explicit session/UI
metadata when available; otherwise state that the exact model is not visible and ask
the user what their selector reports. Keep three facts separate: active model identity,
nominal model context window, and the smaller usable/remaining context after system
instructions, tool schemas, conversation history, reasoning/output reserves, and any
product-level cap. Do not claim that an API model can be selected in the Codex product
unless official Codex-surface documentation confirms it.
