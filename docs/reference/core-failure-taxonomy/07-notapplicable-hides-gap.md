# 7. `notapplicable-hides-gap` — *a "this doesn't apply" rationale that's actually false*
An empty/opaque/unrestricted classification, backed by plausible prose, that on inspection
covers a real, reachable gap.
- **Recurred:** conformance `characters.name/class`, `accounts.loginEpic id_token`,
  `accounts.verifySession token`, `match.report` fields all marked NA/Opaque/Unrestricted
  while genuinely wire-reachable and uncapped — **3 modules independently** (`577d4f5`,
  `2915efd`, `0d5ff32` → swept by `a9211c6`); contrib empty-read returning `Vec::new` without
  reserving the canonical type (`d0bcd93`); topiccheck dedup keyed on topic not `(topic,
  version)`, rejecting the mandated additive-version evolution (`89353e6`).
- **Attack:** for every `NotApplicable`/`Opaque`/`Unrestricted`/"currently accepts no
  free-text" stance, verify the field's actual wire-reachability and byte-cap against the
  handler code — never accept the prose rationale. Test the READ-before-any-WRITE and the
  FORGED/DUPLICATE-first orderings, not just the sequence the feature was designed around.
- **Authority:** replace the stance with an executable probe (`CapCase`,
  `conformance_*_rejected`); reserve the invariant on first *read*, not only first write.
