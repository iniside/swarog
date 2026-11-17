# Dependencies

Last updated: 2026-11-17

- [x] **RustSec advisories in the dependency tree** (2026-11-17, security-quick-wins step 1)
  Lockfile-only patch bumps inside the existing requirements: quinn-proto, quinn-udp,
  rustls, rustls-webpki, webpki-roots, webpki-root-certs, event-listener, rustix and
  tokio (0fedd251, version list in its message); the yanked chacha20 and spin releases
  moved in 41d6f369. RUSTSEC-2026-0459 stays ignored with a premise guard
  (9c6ca7a3, 0fad5a23; proofs 552fce89, 54a309b4, 7eeb6fb9, 077a8773); its removal is
  open in `dependencies.md`. RUSTSEC-2023-0071 (rsa) stays ignored with the rationale
  commented in `tools/verifyctl/src/stages/audit.rs`.
