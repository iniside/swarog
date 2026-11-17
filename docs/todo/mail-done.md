# Mail

Last updated: 2026-11-17

- [x] **Automated proof of SMTP TLS negotiation** (2026-11-17, security-quick-wins step 5)
  MAIL-02. `SmtpSender::tls_trusting_for_tests` (`#[cfg(test)]`, 40f7090e) lets
  `modules/mail/src/smtp_tests.rs` complete STARTTLS and implicit-TLS handshakes and `AUTH`
  against a minted relay (7c21ceee), and prove both production arms of `SmtpSender::new`
  refuse an untrusted certificate (7c21ceee, 2ac03427). Not reached: production hostname
  checking, since production never trusts the minted CA.
