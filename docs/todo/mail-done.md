# Mail

Last updated: 2026-11-17

- [x] **`admin.action` row for a remote admin submit** (2026-11-17, admin-control-plane-redesign step 11)
  MAIL-05. Mail's operator actions are `cp.invoke` calls from the portal, which appends an
  `admin.action` for every invoke attempt (`core/admin/src/routes.rs` `audit`) in both
  topologies, so no `mail.operator_sent` topic is needed.
- [x] **Automated proof of SMTP TLS negotiation** (2026-11-17, security-quick-wins step 5)
  MAIL-02. `SmtpSender::tls_trusting_for_tests` (`#[cfg(test)]`, 40f7090e) lets
  `modules/mail/src/smtp_tests.rs` complete STARTTLS and implicit-TLS handshakes and `AUTH`
  against a minted relay (7c21ceee), and prove both production arms of `SmtpSender::new`
  refuse an untrusted certificate (7c21ceee, 2ac03427). Not reached: production hostname
  checking, since production never trusts the minted CA.
