#!/usr/bin/env bash
# smoke-split-asyncevents.sh — end-to-end proof that the DURABLE EVENTS PLANE (the
# app-owned asyncevents V2 log + pull subscriptions) works in the MICROSERVICES
# SPLIT. Boots the full split via run.sh, then asserts:
#
#   1. DURABLE CROSS-PROCESS DELIVERY: a character created on characters-svc (A)
#      lands a starter grant in inventory on inventory-svc (B) — i.e.
#      character.created flowed A's domain tx -> asyncevents.append_event (the ONE
#      shared XID-ordered log) -> B's pull worker -> inventory.grantStarter, with
#      NO module knowing the topology and NO per-process events env (no
#      EVENTS_ORIGIN, no EVENTS_SUBSCRIBERS, no relay, no POST /events).
#   2. THE LOG + CHECKPOINTS: the character.created event EXISTS in
#      asyncevents.events, and BOTH consumer-owned checkpoints —
#      inventory.character-created.v1 (B) and audit.character-created.v1
#      (audit-svc) — have advanced to (or past) that event's position
#      (generation, producer_xid, tie_breaker). Consumers own their subscriptions;
#      a foreign process cannot swallow another's delivery by construction (each
#      worker drains only its own subscription ids).
#
# Exits NON-ZERO on any failed assertion. Repeatable, committed artifact — the
# proof that replaces "trust me, I ran it" for the at-risk (split) topology.
#
# Requires: reachable local Postgres (same as run.sh) + psql (or PSQL=<path>).
# Run from repo root: ./scripts/smoke-split-asyncevents.sh
set -uo pipefail
cd "$(dirname "$0")/.."

# Ops are driven THROUGH the single front door (gateway-svc): characters-svc (A) and
# inventory-svc (B) serve their ops only over the internal mTLS edge. The DOMAIN
# EFFECTS still land in A's and B's processes — that is what the assertions check.
G=http://localhost:8082     # gateway-svc front door
KEY='X-Api-Key: dev-key-client'
PSQL="${PSQL:-psql}"
if ! command -v "$PSQL" >/dev/null 2>&1; then
    PSQL="/c/Program Files/PostgreSQL/18/bin/psql.exe"
fi
PGURL="${DATABASE_URL:-postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable}"
psql_q() { PGPASSWORD=gamebackend "$PSQL" "$PGURL" -tAc "$1"; }

pass() { echo "PASS: $1"; }
fail() { echo "FAIL: $1" >&2; exit 1; }

cleanup() {
    echo "--- teardown ---"
    ./run.sh --teardown >/dev/null 2>&1 || true
}
trap cleanup EXIT

# --- boot the full split -----------------------------------------------------
./run.sh --teardown >/dev/null 2>&1 || true
echo "--- booting the microservices split ---"
./run.sh split || fail "split did not boot"

# --- register + create a character (through G, executed on A) -----------------
EMAIL="msg-smoke-$(date +%s)@test.local"
REG=$(curl -fsS -X POST "$G/accounts/register" -H "$KEY" -H 'Content-Type: application/json' \
    -d "{\"email\":\"$EMAIL\",\"password\":\"pw12345678\",\"displayName\":\"MsgSmoke\"}") \
    || fail "register through G failed"
TOKEN=$(echo "$REG" | sed -E 's/.*"token":"([^"]+)".*/\1/')
[ -n "$TOKEN" ] || fail "no token in register response: $REG"

CH=$(curl -fsS -X POST "$G/characters" -H "$KEY" -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/json' -d '{"name":"MsgSmoke","class":"novice"}') \
    || fail "character create through G failed"
CID=$(echo "$CH" | sed -E 's/.*"id":"([^"]+)".*/\1/')
[ -n "$CID" ] || fail "no character id in response: $CH"
echo "created character $CID (G front door -> characters-svc)"

# --- assertion 1: the grant lands on B via the durable pull plane -------------
GRANTED=""
t=40
while [ "$t" -gt 0 ]; do
    INV=$(curl -fsS "$G/inventory/character/$CID" -H "$KEY" -H "Authorization: Bearer $TOKEN" 2>/dev/null) || INV=""
    if echo "$INV" | grep -q '"item_id"'; then
        GRANTED=$(echo "$INV" | sed -E 's/.*"item_id":"([^"]+)".*/\1/')
        break
    fi
    t=$((t - 1)); sleep 0.25
done
[ -n "$GRANTED" ] || fail "starter grant never landed in inventory on B within deadline — durable cross-process delivery broken"
pass "durable cross-process delivery: character.created (A) -> starter grant '$GRANTED' in inventory (B)"

# --- assertion 2: the shared log holds the event ------------------------------
EVROWS=$(psql_q "SELECT count(*) FROM asyncevents.events WHERE topic='character.created' AND payload->>'character_id'='$CID';")
[ "$EVROWS" = "1" ] || fail "expected exactly 1 asyncevents.events row for character.created of $CID, got '$EVROWS'"
pass "asyncevents.events holds the character.created event for $CID (single shared XID-ordered log, one append)"

# --- assertion 3: BOTH consumer checkpoints advanced past the event's position.
# Cursors advance transactionally with the handler effect, so a cursor >= the
# event's (generation, producer_xid, tie_breaker) proves that subscription's
# worker DELIVERED it. Poll: audit's delivery is independent of inventory's.
cursor_past() {
    psql_q "SELECT count(*) FROM asyncevents.subscriptions s, asyncevents.events e \
            WHERE s.subscription_id='$1' \
              AND e.topic='character.created' AND e.payload->>'character_id'='$CID' \
              AND (s.cursor_generation, s.cursor_xid, s.cursor_tie) \
                  >= (e.generation, e.producer_xid, e.tie_breaker);"
}
for SUB in inventory.character-created.v1 audit.character-created.v1; do
    OK=""
    t=40
    while [ "$t" -gt 0 ]; do
        if [ "$(cursor_past "$SUB")" = "1" ]; then OK=1; break; fi
        t=$((t - 1)); sleep 0.25
    done
    [ -n "$OK" ] || fail "subscription $SUB cursor never advanced past the character.created event — its pull worker did not deliver it"
    pass "checkpoint $SUB advanced past the event's position (transactional cursor = delivered)"
done

echo ""
echo "=== SMOKE PASSED: the V2 durable event log delivers in the microservices split; consumer-owned pull checkpoints hold ==="
