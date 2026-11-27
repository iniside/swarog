# Dependencies

Last updated: 2026-11-27

- [x] **New RustSec advisories fail the audit stage** (2026-11-27, verify-reds-fix step 4)
  `cargo audit` reports 8 vulnerabilities and 1 warning:
  - cookie 0.18.1, RUSTSEC-2026-0594 (fix: >=0.18.3);
  - lettre 0.11.23, RUSTSEC-2026-0560 to -0565 (fix: >=0.11.24);
  - url 2.5.8, RUSTSEC-2026-0592 (fix: >=2.5.9);
  - chrono 0.4.45, RUSTSEC-2026-0543, unmaintained, no fixed upgrade (warning only).
  Source: verifyctl run verify-1795776086-65428-62521403, 2026-11-27.
  Closed: lettre 0.11.24, url 2.5.9, cookie 0.18.3 (5e953f2f); `cargo audit` leaves only the chrono warning.
- [x] **RustSec advisories in the dependency tree** (2026-11-17, security-quick-wins step 1)
  Lockfile-only bumps within the existing requirements: quinn-proto, quinn-udp,
  rustls, rustls-webpki, webpki-roots, webpki-root-certs, event-listener, rustix and
  tokio (0fedd251, version list in its message); the yanked chacha20 and spin releases
  moved in 41d6f369. RUSTSEC-2026-0459 stays ignored with a premise guard
  (9c6ca7a3, 0fad5a23; proofs 552fce89, 54a309b4, 7eeb6fb9, 077a8773); its removal is
  open in `dependencies.md`. RUSTSEC-2023-0071 (rsa) stays ignored with the rationale
  commented in `tools/verifyctl/src/stages/audit.rs`.
