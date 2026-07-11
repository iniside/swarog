# Hetzner deploy checklist (public front + hardened admin)

Manual, deploy-day steps that CANNOT be verified locally (ACME needs public DNS +
port 443). Everything else is covered by `./verify.sh` + `./split-proof.sh`.
Companion plan: `docs/plans/2026-07-10-2314-admin-hardening-plan.md`.

## Prerequisites

- [ ] DNS `A`/`AAAA` record for the public domain points at the server
      (e.g. `api.example.com`); propagation confirmed (`nslookup`).
- [ ] Port **443** reachable from the internet on the gateway host (ACME here is
      TLS-ALPN-01 — **no port 80 needed**, no HTTP→HTTPS redirect exists).
- [ ] Postgres provisioned; `DATABASE_URL` set for every backend process.
- [ ] Real (non-dev) `EDGE_CA_CERT`/`EDGE_CA_KEY` minted via `cargo run -p edgeca`
      and distributed to all processes (never the ephemeral in-memory dev CA —
      watch for its loud warn in logs).

## Gateway (cmd/gateway-svc) env

- [ ] `TLS_MODE=acme`
- [ ] `ACME_DOMAINS=api.example.com` (comma-separated for more)
- [ ] `ACME_CONTACT=mailto:you@example.com` (optional but recommended —
      expiry mails from Let's Encrypt)
- [ ] `ACME_CACHE_DIR=<persistent path>` (default `run/acme-cache`; MUST survive
      restarts or you re-issue on every boot and hit LE rate limits)
- [ ] `PORT=:443`
- [ ] `ADMIN_HTTP_ADDR=<admin-svc host:port>` (the `/admin` passthrough origin)
- [ ] Dev knobs NOT set: no `APIKEYS_DEV_ALLOW`, no `ACCOUNTS_DEV_AUTH` unless
      deliberately on.

## Admin process (cmd/admin-svc or monolith) env

- [ ] `TRUSTED_PROXY_CIDRS=<gateway host IP>/32` — REQUIRED for correct per-IP
      lockout: without it every client resolves to the gateway's address and one
      attacker's failures throttle everyone (IP row locks at 20 fails).
- [ ] `ADMIN_COOKIE_SECURE` NOT set (defaults to Secure ON — correct behind
      https). Never set `=0` in production.
- [ ] `ADMIN_OPEN` NOT set (it disables sessions AND CSRF).

## First boot

- [ ] Create the real admin user: `ADMINCTL_PASSWORD` unset →
      `./install.sh <username>` (no-echo prompt; NEVER pass the password as an
      argument). Repeat later to reset a password.
- [ ] Boot; grep gateway logs for rustls-acme issuance events (order/finalize/
      cert). First request may be slow while the order completes.
- [ ] `curl -I https://api.example.com/leaderboard -H "X-Api-Key: <real key>"` →
      200, valid LE chain (no `-k`!).
- [ ] Browser: `https://api.example.com/admin` → redirects to `/admin/login` →
      log in → portal renders; devtools shows `admin_session` cookie with
      `Secure; HttpOnly; SameSite=Strict; Path=/admin`.

## Post-deploy security spot-checks

- [ ] Wrong-password burst (6×) from one machine → all answers are identical
      generic 401s; then
      `SELECT * FROM admin.login_attempts;` shows the user row locked
      (`locked_until` set) and the ip row counting but unlocked below 20.
- [ ] `SELECT * FROM asyncevents.events WHERE topic='admin.action' ORDER BY id DESC
      LIMIT 5;` shows login-succeeded/login-locked rows; `audit.log` mirrors them.
- [ ] A mutating admin form POST without `_csrf` (devtools-edited) → 403.
- [ ] `/admin` reached ONLY via the gateway passthrough; admin-svc's own port is
      not exposed publicly (firewall check).
- [ ] Restart gateway-svc → no re-issuance (ACME cache hit), https still valid.

## Known non-goals (documented decisions)

- No HTTP→HTTPS redirect on :80 (nothing listens there).
- No TOTP/2FA yet (sessions design accommodates it later).
- Player QUIC (:9100) has no per-IP rate limit yet — separate work item.
- Monolith (`cmd/server`) does not parse TLS env — TLS is gateway-svc-only
  (single public front door). If the monolith ever fronts the internet, add the
  same ~10 parse lines to `cmd/server/main.rs`.
