---
name: csharp-client-after-contract-is-final
description: "Regenerate/fix the C# test client only once a plan's contract is final, not per intermediate step"
metadata:
  node_type: memory
  type: feedback
  originSessionId: 8214c2ca-dbe5-496f-9b45-71068b5bf051
  modified: 2026-11-26T21:22:04.907Z
---

Do not regenerate or fix the C# test client (clients/csharp, csharp-client-gen goldens, the csharp-client verifyctl stage) in intermediate steps of a plan that is still changing a contract. Do it once, at the end, when the whole contract is ready.

**Why:** owner, 2026-11-26, during one-token-mechanism Step 3: "klienta c# mozemy po prostu poprawic pozniej, kiedy caly kontrakt bedzie juz gotowy a nie.. kiedy nawet podpisywania nie mamy". Peripheral work done mid-plan cost time and pulled focus from the main scope (token unification).

**How to apply:** when planning or dispatching, put C# client regeneration in the plan's final step; skip the csharp-client / codegen-freshness stages for the C# side in earlier steps. More generally, flag work outside a plan's core scope before doing it (see [[no-inline-adhoc-fixes]]).
