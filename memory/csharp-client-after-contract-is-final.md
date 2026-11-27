---
name: csharp-client-after-contract-is-final
description: "Leave the C# client alone entirely; don't plan, fix or bring it up until the owner asks"
metadata:
  node_type: memory
  type: feedback
  originSessionId: 8214c2ca-dbe5-496f-9b45-71068b5bf051
  modified: 2026-11-27T14:31:03.068Z
---

Do not regenerate, fix or schedule the C# client (clients/csharp, tools/csharp-client-gen, the csharp-client verifyctl stage) in any plan or plan-end step, and do not raise it in reports or leftover lists. It is outside every current plan; the owner will start it separately.

**Why:** owner, 2026-11-27, during oidc-authorization-code: "tego clienta c# to sobie odpusc, zrobimy go innym razem" and "client c# jest w ogole poza tym ogolnym planem, miales tylko zapisac zeby do niego nie wracac". Earlier (2026-11-26) it was "fix it once the contract is final", which kept pulling it back into plan-end steps.

**How to apply:** drop C# client work from plan steps and verification lists; skip its stages when choosing checks; don't file it as a leftover or roadmap item. See [[no-inline-adhoc-fixes]].
