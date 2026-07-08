#!/usr/bin/env bash
# run-dev.sh — load .env.local and start the backend.
# Serves the account-linking web UI at http://localhost:8080
# Secrets live in .env.local (gitignored); this script never contains them.
set -euo pipefail
cd "$(dirname "$0")"

if [ -f .env.local ]; then
  set -a
  # shellcheck disable=SC1091
  . ./.env.local
  set +a
  echo "Loaded .env.local"
  [ -n "${EPIC_CLIENT_ID:-}" ] && echo "Epic OAuth: client ${EPIC_CLIENT_ID}, redirect ${EPIC_REDIRECT_URI:-}"
else
  echo "No .env.local — running without Epic OAuth"
fi

echo "Open http://localhost:8080"
exec go run ./cmd/server
