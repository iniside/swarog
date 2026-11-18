# 6. `coverage-gap` / `false-pass` — *the verify-net signature: a gate that can't see the thing it gates*
A check is architecturally present and green, but its authority (a match guard, a constant, a
stance, an argv, a lock branch) has silently detached from what it's supposed to gate — so it
matches zero targets, tests a hand-built fixture that bypasses the real classifier, or asserts
"a response came back" instead of "the seam works".
- **Recurred:** archcheck rule 9 `Kind::Other` vs real `Kind::Core("bus")` — dead, always
  green (`23ec41a → 5e8ae52`); `CONFORMANCE_POLICY_CRATE="conformance"` vs real
  `conformancecheck` — permanently vacuous (`3cbc2f0`); bare `Slot::new` via `use` bypassing
  the qualified-string tripwire (`971cf2c → 428182d`); audit argv missing the `audit`
  subcommand, fixture never asserted argv (`8e297f5`); splitproof asserting 201 but not that
  the created row is reachable through the remote list binding (`d0512a6`); golden fingerprint
  blind to `Option<T>→T` demotion and testing the Rust struct instead of the production SQL
  trigger (`be3aae3 → 606d321`); the admin golden enum guard (Step 13 of the admin redesign)
  restated the templates' drawing rules in test code, and four audit rounds each found a new
  drift (`9ce5eb84`, `97df9b4c`); the cause-fix is coverage observed from the render (plan Step 28).
- **Attack:** for any string/enum tripwire, write a test that calls the **actual production
  function** against a fixture proven to trigger the real violation — never assert on a
  hand-built array that skips the classifier. Ask "**if I renamed/aliased/re-imported the
  target, would this rule still fire?**" A rule that can silently match zero targets must
  itself be a violation. For a scenario assertion ask "does this prove the seam it claims, or
  just that *a* response returned". If the proof restates production logic instead of
  observing it, the mirror is the defect: one structural finding, not a list of missing rules.
- **Authority:** the match guard / constant / argv / stance itself, compile- or test-coupled
  to the real source of truth (a rule that matches zero packages fails loudly; a golden gen
  driven from the same artifact production uses).
