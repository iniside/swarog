# Mail

Last updated: 2026-10-23

## In progress

_None._

## Open

- [ ] **Automated proof of SMTP TLS negotiation**
  MAIL-02. No test completes a handshake, so the STARTTLS upgrade, the certificate
  check and `AUTH` are unproven; only the `relay` versus `starttls_relay` choice is
  pinned (modules/mail/src/smtp_tests.rs:13-19). Source:
  docs/roadmap/feature-tracker.md:246 @ fd797b3a (2026-09-05).
- [ ] **In-tree producer for `mail.send_requested`**
  MAIL-03. Only mail consumes `SEND_REQUESTED` (Grep of modules/, lower bound). The
  intended producer is accounts. Source: docs/roadmap/feature-tracker.md:246 @ fd797b3a
  (2026-09-05).
- [ ] **`admin.action` row for a remote admin submit**
  MAIL-05. Needs a `mail.operator_sent` topic; no `operator_sent` in
  modules/mail/src (Grep, lower bound). Source: docs/roadmap/feature-tracker.md:246 @ fd797b3a
  (2026-09-05).
