#!/usr/bin/env bash
# smoke-split-config.sh — end-to-end proof that the config module works in the
# MICROSERVICES SPLIT (not just the monolith). Boots characters-svc + inventory-svc
# + gateway-svc via run.sh, then asserts:
#
#   1. inventory-svc booted WITH config hosted (fail-loud satisfied — the binary
#      would refuse to boot if inventory Requires("config") and config were absent).
#   2. IN-PROCESS live reload in the split: editing inventory/starter_item at
#      inventory-svc's own /admin makes a freshly-created character grant the new item.
#   3. ANY-WRITER live reload: a RAW psql UPDATE (bypassing all app code) propagates
#      via the DB trigger's NOTIFY to inventory-svc's listener — a freshly-created
#      character then grants the psql-set item.
#
# Exits NON-ZERO on any failed assertion. This is the repeatable, committed artifact
# that replaces "trust me, I ran it".
#
# Requires: a reachable local Postgres (same assumption as run.sh) and psql on PATH
# or via PSQL=<path>. Run from the repo root: ./scripts/smoke-split-config.sh
set -uo pipefail
cd "$(dirname "$0")/.."

A=http://localhost:8080     # characters-svc (accounts + characters)
B=http://localhost:8081     # inventory-svc (inventory + admin + config)
CONFIG_SLUG="game-config--flags"   # slugify("Game Config & Flags")
PSQL="${PSQL:-psql}"
if ! command -v "$PSQL" >/dev/null 2>&1; then
    PSQL="/c/Program Files/PostgreSQL/18/bin/psql.exe"
fi
PGURL="${DATABASE_URL:-postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable}"

pass() { echo "PASS: $1"; }
fail() { echo "FAIL: $1" >&2; exit 1; }

cleanup() {
    echo "--- teardown ---"
    PGPASSWORD=gamebackend "$PSQL" "$PGURL" -tAc \
        "DELETE FROM config.settings WHERE namespace='inventory';" >/dev/null 2>&1 || true
    ./run.sh --teardown >/dev/null 2>&1 || true
}
trap cleanup EXIT

# --- boot the split --------------------------------------------------------
./run.sh --teardown >/dev/null 2>&1 || true
echo "--- booting microservices split ---"
./run.sh microservices || fail "split did not boot"

# --- assertion 1: config is hosted in inventory-svc (fail-loud satisfied) ---
if grep -qE 'msg="module ready" module=config' run/inventory.out.log; then
    pass "inventory-svc booted WITH config hosted"
else
    fail "inventory-svc did not report 'module ready module=config' — config not hosted in the split binary"
fi

# --- helpers ---------------------------------------------------------------
# register once on A, reuse the token
EMAIL="smoke-$(date +%s)@test.local"
REG=$(curl -fsS -X POST "$A/accounts/register" -H 'Content-Type: application/json' \
    -d "{\"email\":\"$EMAIL\",\"password\":\"pw12345678\",\"displayName\":\"Smoke\"}") \
    || fail "register on A failed"
TOKEN=$(echo "$REG" | sed -E 's/.*"token":"([^"]+)".*/\1/')
[ -n "$TOKEN" ] || fail "no token in register response: $REG"

# grant_item_for_fresh_char — create a NEW character on A and echo the item_id its
# inventory holds on B. A fresh character each call because the starter is granted
# ONCE at character.created against whatever the materialized spec is AT THAT MOMENT;
# a character created before propagation is frozen to the old item.
grant_item_for_fresh_char() {
    local ch cid inv
    ch=$(curl -fsS -X POST "$A/characters" -H "Authorization: Bearer $TOKEN" \
        -H 'Content-Type: application/json' -d '{"name":"Smoke","class":"novice"}') || return 1
    cid=$(echo "$ch" | sed -E 's/.*"id":"([^"]+)".*/\1/')
    [ -n "$cid" ] || return 1
    # small settle for the async grant (outbox -> relay -> sink) to land
    local t=20
    while [ "$t" -gt 0 ]; do
        inv=$(curl -fsS "$B/inventory/character/$cid" -H "Authorization: Bearer $TOKEN" 2>/dev/null) || inv=""
        if echo "$inv" | grep -q '"item_id"'; then
            echo "$inv" | sed -E 's/.*"item_id":"([^"]+)".*/\1/'
            return 0
        fi
        t=$((t - 1)); sleep 0.25
    done
    return 1
}

# poll_until_grant EXPECTED DESC — retry fresh-create until a new character's grant
# is EXPECTED, within a deadline. This is the correct method: retry FRESH creates,
# not poll one frozen character.
poll_until_grant() {
    local expected="$1" desc="$2" deadline=$((SECONDS + 30)) got
    while [ "$SECONDS" -lt "$deadline" ]; do
        got=$(grant_item_for_fresh_char) || { sleep 0.5; continue; }
        if [ "$got" = "$expected" ]; then
            pass "$desc — fresh character granted '$expected'"
            return 0
        fi
        sleep 0.5
    done
    fail "$desc — never granted '$expected' within deadline (last='$got')"
}

# baseline: before any edit the split grants the fallback constant
BASE=$(grant_item_for_fresh_char) || fail "baseline grant failed"
[ "$BASE" = "starter_sword" ] && pass "baseline (empty config) grants fallback 'starter_sword'" \
    || echo "note: baseline grant was '$BASE' (expected starter_sword on empty config)"

# --- assertion 2: in-process reload via inventory-svc's own /admin ---------
echo "--- editing inventory:starter_item=health_potion at $B/admin ---"
code=$(curl -fsS -o /dev/null -w '%{http_code}' -X POST "$B/admin/$CONFIG_SLUG" \
    --data-urlencode "_new_namespace=inventory" \
    --data-urlencode "_new_key=starter_item" \
    --data-urlencode "_new_value=health_potion") || fail "admin POST failed"
[ "$code" = "303" ] || fail "admin edit POST returned $code, expected 303"
pass "admin edit POST accepted (303)"
poll_until_grant "health_potion" "in-process split reload (admin edit)"

# --- assertion 3: any-writer reload via a RAW psql UPDATE ------------------
echo "--- raw psql UPDATE inventory:starter_item=coin (bypasses all app code) ---"
PGPASSWORD=gamebackend "$PSQL" "$PGURL" -tAc \
    "UPDATE config.settings SET value='coin' WHERE namespace='inventory' AND key='starter_item';" \
    >/dev/null || fail "psql UPDATE failed"
poll_until_grant "coin" "cross-connection reload (raw psql write via DB trigger NOTIFY)"

echo ""
echo "=== SMOKE PASSED: config live-reload works in the microservices split ==="
