#!/usr/bin/env bash
# install.sh -- create (or password-reset) a GameOps admin login for the hardened admin
# portal. Thin no-echo-prompt wrapper over `adminctl create-user` (tools/adminctl): the
# CLI never accepts a password on argv, so this script prompts for it silently (or reads
# ADMINCTL_PASSWORD) and pipes it in over stdin. Paired with install.ps1 (Windows).
#
# Usage:
#   ./install.sh <username>          # prompt for the password (no echo)
#   ADMINCTL_PASSWORD=... ./install.sh <username>   # non-interactive (CI/scripted)
#
# Connection: DATABASE_URL (default local dev DSN, same as the services). The admin
# schema is created on the fly if this is a fresh database.
set -euo pipefail
cd "$(dirname "$0")"

USERNAME="${1:-}"
if [ -z "$USERNAME" ]; then
    echo "usage: ./install.sh <username>" >&2
    echo "  creates (or resets the password of) an admin portal login." >&2
    exit 1
fi

# Password: ADMINCTL_PASSWORD wins (non-interactive); otherwise prompt twice, no echo.
if [ -n "${ADMINCTL_PASSWORD:-}" ]; then
    PASSWORD="$ADMINCTL_PASSWORD"
else
    read -r -s -p "Password for admin user '$USERNAME': " PASSWORD
    echo
    read -r -s -p "Confirm password: " PASSWORD_CONFIRM
    echo
    if [ "$PASSWORD" != "$PASSWORD_CONFIRM" ]; then
        echo "error: passwords do not match" >&2
        exit 1
    fi
fi
if [ -z "$PASSWORD" ]; then
    echo "error: password must not be empty" >&2
    exit 1
fi

# Pipe the password over stdin -- never on argv, never in the process table.
printf '%s\n' "$PASSWORD" | cargo run -q -p adminctl -- create-user "$USERNAME" --password-stdin

cat <<EOF

Admin user '$USERNAME' is ready. Next steps for a public deployment:
  - TLS_MODE=acme ACME_DOMAINS=admin.example.com ACME_CONTACT=you@example.com
      (or TLS_MODE=files TLS_CERT_PATH=... TLS_KEY_PATH=...) on cmd/gateway-svc.
  - ADMIN_HTTP_ADDR=0.0.0.0:8085   (bind the admin process publicly / behind the proxy).
  - TRUSTED_PROXY_CIDRS=<proxy-hop-cidr>   so the login lockout sees the real client IP,
      not the gateway. Required whenever admin runs behind a reverse proxy.
  - Log in at https://<your-domain>/admin with the credentials you just set.
EOF
