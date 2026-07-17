#!/usr/bin/env bash
# db-provision.sh -- seed script that provisions the dev Postgres role + database
# this repo assumes but never creates (CLAUDE.md: "the answer is a seed script
# minting fake data ... not a migration"). Idempotent: safe to run repeatedly.
#
# Creates ONLY:
#   - login role  gamebackend / gamebackend
#   - database    gamebackend  owned by that role
# It does NOT create module schemas or the asyncevents plane -- those are owned
# by each module's `migrate` step and the app-owned event plane, run at process
# boot (devctl up / cargo test / cargo run -p <svc>).
#
# Connects as a Postgres SUPERUSER (default: the current OS user, against the
# `postgres` maintenance database) to do the provisioning -- NOT as `gamebackend`,
# which does not exist yet on a fresh machine. Override with PGSUPERUSER /
# PGSUPERDB / PGSUPERHOST / PGSUPERPORT if the local superuser differs.
#
# Target role/db/password/host/port are read from DATABASE_URL if set, else
# fall back to this repo's documented default:
#   postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable
#
# macOS/BSD portable: bash 3.2, no GNU-only sed/date/grep flags, no bash-4
# constructs. psql may not be on PATH (Homebrew keg-only postgresql@N) --
# resolved via PATH first, then a fallback search.
#
# Usage: bash scripts/db-provision.sh
set -euo pipefail

# ---- locate psql -----------------------------------------------------------

find_psql() {
  if command -v psql >/dev/null 2>&1; then
    command -v psql
    return
  fi
  # Homebrew keg-only postgresql@N is common on macOS and not linked onto PATH.
  local candidate
  for candidate in \
    /opt/homebrew/opt/postgresql@18/bin/psql \
    /opt/homebrew/opt/postgresql@17/bin/psql \
    /opt/homebrew/opt/postgresql@16/bin/psql \
    /opt/homebrew/opt/postgresql/bin/psql \
    /usr/local/opt/postgresql@18/bin/psql \
    /usr/local/opt/postgresql@17/bin/psql \
    /usr/local/opt/postgresql@16/bin/psql \
    /usr/local/opt/postgresql/bin/psql \
    /usr/bin/psql \
    /usr/local/bin/psql ; do
    if [ -x "$candidate" ]; then
      echo "$candidate"
      return
    fi
  done
  echo "error: psql not found on PATH or in known Homebrew locations" >&2
  echo "  install it (e.g. 'brew install postgresql@18') or add it to PATH" >&2
  exit 1
}

PSQL="$(find_psql)"

# ---- parse DATABASE_URL (or fall back to the repo default) ----------------
# Expected form: postgres://USER:PASS@HOST:PORT/DBNAME?params

DSN="${DATABASE_URL:-postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable}"

parse_dsn() {
  local dsn="$1" rest userpass hostport
  rest="${dsn#*://}"                 # USER:PASS@HOST:PORT/DBNAME?params
  userpass="${rest%%@*}"             # USER:PASS
  rest="${rest#*@}"                  # HOST:PORT/DBNAME?params
  hostport="${rest%%/*}"             # HOST:PORT
  rest="${rest#*/}"                  # DBNAME?params
  DB_NAME="${rest%%\?*}"
  DB_USER="${userpass%%:*}"
  DB_PASS="${userpass#*:}"
  DB_HOST="${hostport%%:*}"
  DB_PORT="${hostport#*:}"
  [ -n "$DB_HOST" ] || DB_HOST="localhost"
  [ -n "$DB_PORT" ] || DB_PORT="5432"
  [ -n "$DB_USER" ] || DB_USER="gamebackend"
  [ -n "$DB_PASS" ] || DB_PASS="gamebackend"
  [ -n "$DB_NAME" ] || DB_NAME="gamebackend"
}

parse_dsn "$DSN"

# ---- superuser connection used to provision --------------------------------
# Default: current OS user (a Postgres superuser on this machine), against the
# `postgres` maintenance database on the same host/port as the target DSN.

SUPERUSER="${PGSUPERUSER:-$(id -un)}"
SUPERDB="${PGSUPERDB:-postgres}"
SUPERHOST="${PGSUPERHOST:-$DB_HOST}"
SUPERPORT="${PGSUPERPORT:-$DB_PORT}"

psql_super() {
  "$PSQL" -v ON_ERROR_STOP=1 -X -q -U "$SUPERUSER" -h "$SUPERHOST" -p "$SUPERPORT" -d "$SUPERDB" "$@"
}

echo "db-provision: using psql at $PSQL"
echo "db-provision: connecting as superuser '$SUPERUSER' to $SUPERHOST:$SUPERPORT/$SUPERDB"
echo "db-provision: target role/database '$DB_USER'/'$DB_NAME'"

# ---- role: create only if absent -------------------------------------------
# CREATE ROLE ... LOGIN with the password as a SQL string literal (doubled
# single quotes escape a literal quote per SQL syntax -- not a general
# injection defense, but this is a dev tool for a trusted local operator per
# CLAUDE.md's dev-tooling scope, and the value is this script's own DSN-derived
# password, never external input). Note: this psql build does not perform
# `-v name=value` variable interpolation with `-c`, only when reading a script,
# so the literal is built directly rather than via `:varname`.

DB_PASS_LITERAL=$(printf '%s' "$DB_PASS" | awk '{gsub(/\x27/,"\x27\x27"); print}')

ROLE_EXISTS="$(psql_super -tAc "SELECT 1 FROM pg_roles WHERE rolname = '$DB_USER'")"
if [ "$ROLE_EXISTS" = "1" ]; then
  # "provisioned" means the role exists WITH the DSN's password, not merely
  # that the row exists -- an existing role whose password has drifted from
  # the DSN must still be re-idempotent. ALTER ROLE converges both LOGIN (in
  # case an existing role lacks it) and PASSWORD unconditionally; this is
  # cheap and always correct to re-apply.
  psql_super -c "ALTER ROLE \"$DB_USER\" LOGIN PASSWORD '$DB_PASS_LITERAL'"
  echo "db-provision: role '$DB_USER' already exists -- converged password"
else
  psql_super -c "CREATE ROLE \"$DB_USER\" LOGIN PASSWORD '$DB_PASS_LITERAL'"
  echo "db-provision: created role '$DB_USER'"
fi

# ---- database: create only if absent ---------------------------------------
# CREATE DATABASE cannot run inside a transaction block or a DO $$ ... $$
# block, so check first from the shell and issue it directly, conditionally.

DB_EXISTS="$(psql_super -tAc "SELECT 1 FROM pg_database WHERE datname = '$DB_NAME'")"
if [ "$DB_EXISTS" = "1" ]; then
  echo "db-provision: database '$DB_NAME' already exists -- skipping create"
else
  psql_super -c "CREATE DATABASE \"$DB_NAME\" OWNER \"$DB_USER\""
  echo "db-provision: created database '$DB_NAME' owned by '$DB_USER'"
fi

# ---- grants -----------------------------------------------------------------
# The role owns the database (via CREATE DATABASE ... OWNER above), which
# already lets it CREATE SCHEMA inside its own database on modern Postgres
# (schema-per-module + the asyncevents plane both create schemas at boot).
# Explicitly (re-)grant CONNECT + CREATE on the database for idempotent
# clarity, and ensure it owns the default `public` schema too. No SUPERUSER,
# no elevated role membership -- ownership of its own database is sufficient.

psql_super -c "GRANT CONNECT, CREATE ON DATABASE \"$DB_NAME\" TO \"$DB_USER\""
psql_super -d "$DB_NAME" -c "ALTER SCHEMA public OWNER TO \"$DB_USER\"" >/dev/null 2>&1 || true

echo "db-provision: done. Verify with:"
echo "  PGPASSWORD=$DB_PASS $PSQL -U $DB_USER -h $DB_HOST -p $DB_PORT -d $DB_NAME -c 'select current_user, current_database()'"
