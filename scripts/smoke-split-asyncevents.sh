#!/usr/bin/env bash
# smoke-split-asyncevents.sh — end-to-end proof that the DURABLE EVENTS PLANE (the
# app-owned asyncevents transport) works in the MICROSERVICES SPLIT, and that the
# single-owner relay redesign holds. Boots the full split (characters-svc,
# inventory-svc, scheduler-svc, gateway-svc) via run.sh, then asserts:
#
#   1. DURABLE CROSS-PROCESS DELIVERY: a character created on characters-svc (A)
#      lands a starter grant in inventory on inventory-svc (B) — i.e.
#      character.created flowed A's domain tx -> asyncevents.outbox(origin=
#      characters-svc) -> A's relay -> POST /events -> B's inbound sink ->
#      inventory.grantStarter, all with NO module knowing the topology. (The
#      plane itself is app-owned infrastructure, constructed unconditionally by
#      `app::run` whenever the process has a DB pool — there is no per-module
#      "hosted" gate left to probe; a booted characters-svc/inventory-svc IS the
#      plane-present proof.)
#   2. BLOCKER-1 REGRESSION (the whole redesign's reason to exist): with
#      scheduler-svc (origin=scheduler-svc) running its OWN relay over the SAME
#      shared asyncevents.outbox, A's character.created row is NOT swallowed by a
#      foreign-origin relay. Proven two ways: the grant lands (assertion 1), AND
#      the outbox row is stamped origin=characters-svc and marked sent, with the
#      inbox carrying (event_id, subscriber='inventory') — the row was consumed
#      by inventory, not mark-sent-to-nobody by scheduler-svc.
#
# Exits NON-ZERO on any failed assertion. Repeatable, committed artifact — the
# proof that replaces "trust me, I ran it" for the at-risk (split) topology.
#
# Requires: reachable local Postgres (same as run.sh) + psql (or PSQL=<path>).
# Run from repo root: ./scripts/smoke-split-asyncevents.sh
set -uo pipefail
cd "$(dirname "$0")/.."

A=http://localhost:8080     # characters-svc (accounts + characters)  origin=characters-svc
B=http://localhost:8081     # inventory-svc  (inventory + audit + admin) origin=inventory-svc
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

# --- boot the full split ---------------------------------------------------
./run.sh --teardown >/dev/null 2>&1 || true
echo "--- booting microservices split (A + B + scheduler-svc + gateway) ---"
./run.sh microservices || fail "split did not boot"

# --- register + create a character on A ------------------------------------
EMAIL="msg-smoke-$(date +%s)@test.local"
REG=$(curl -fsS -X POST "$A/accounts/register" -H 'Content-Type: application/json' \
    -d "{\"email\":\"$EMAIL\",\"password\":\"pw12345678\",\"displayName\":\"MsgSmoke\"}") \
    || fail "register on A failed"
TOKEN=$(echo "$REG" | sed -E 's/.*"token":"([^"]+)".*/\1/')
[ -n "$TOKEN" ] || fail "no token in register response: $REG"

CH=$(curl -fsS -X POST "$A/characters" -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/json' -d '{"name":"MsgSmoke","class":"novice"}') \
    || fail "character create on A failed"
CID=$(echo "$CH" | sed -E 's/.*"id":"([^"]+)".*/\1/')
[ -n "$CID" ] || fail "no character id in response: $CH"
echo "created character $CID on A (characters-svc)"

# --- assertion 1: the grant lands on B via the durable plane ---------------
GRANTED=""
t=40
while [ "$t" -gt 0 ]; do
    INV=$(curl -fsS "$B/inventory/character/$CID" -H "Authorization: Bearer $TOKEN" 2>/dev/null) || INV=""
    if echo "$INV" | grep -q '"item_id"'; then
        GRANTED=$(echo "$INV" | sed -E 's/.*"item_id":"([^"]+)".*/\1/')
        break
    fi
    t=$((t - 1)); sleep 0.25
done
[ -n "$GRANTED" ] || fail "starter grant never landed in inventory on B within deadline — durable cross-process delivery broken"
pass "durable cross-process delivery: character.created (A) -> starter grant '$GRANTED' in inventory (B)"

# --- assertion 2: BLOCKER-1 — the row is A's origin, consumed by inventory, not swallowed
# The outbox row for THIS character's created event: stamped origin=characters-svc.
OID=$(psql_q "SELECT id FROM asyncevents.outbox WHERE topic='character.created' AND payload->>'CharacterID' = '$CID' LIMIT 1;")
[ -n "$OID" ] || fail "no asyncevents.outbox row found for character.created of $CID"
OORIGIN=$(psql_q "SELECT origin FROM asyncevents.outbox WHERE id = $OID;")
[ "$OORIGIN" = "characters-svc" ] || fail "outbox row origin is '$OORIGIN', expected 'characters-svc' (wrong producer stamped it)"
pass "outbox row $OID stamped origin=characters-svc (produced by characters-svc, not a foreign origin)"

# The own-origin relay marks the row sent — POLL, since markSent commits shortly AFTER
# B has already made the grant visible (the relay holds its drain tx open across the
# delivery round-trip), so a single read can race the commit. If a FOREIGN-origin relay
# had swallowed the row it would be mark-sent-to-nobody with no inbox rows (checked next);
# if delivery were failing it would never become sent.
OSENT=""
t=40
while [ "$t" -gt 0 ]; do
    if [ "$(psql_q "SELECT sent_at IS NOT NULL FROM asyncevents.outbox WHERE id = $OID;")" = "t" ]; then
        OSENT=t; break
    fi
    t=$((t - 1)); sleep 0.25
done
[ "$OSENT" = "t" ] || fail "outbox row $OID for $CID never marked sent within deadline — stranded or delivery failing"
pass "outbox row $OID marked sent by its own-origin (characters-svc) relay"

# The inbox proves the receivers on B consumed it — NOT scheduler-svc mark-sent-to-nobody.
# Both durable subscribers of character.created live in inventory-svc (B): inventory
# (OnTx) and audit (OnTxRaw). Poll for both (event_id, subscriber) rows.
EVID="asyncevents:$OID"
INBOX=""
t=40
while [ "$t" -gt 0 ]; do
    INBOX=$(psql_q "SELECT string_agg(subscriber, ',' ORDER BY subscriber) FROM asyncevents.inbox WHERE event_id='$EVID';")
    if [ "$INBOX" = "audit,inventory" ]; then break; fi
    t=$((t - 1)); sleep 0.25
done
[ "$INBOX" = "audit,inventory" ] || fail "inbox for $EVID = '[$INBOX]', expected 'audit,inventory' — a subscriber did not durably consume (scheduler-svc may have swallowed it, or a sink is down)"
pass "BLOCKER-1 regression: event $EVID consumed by BOTH 'audit' and 'inventory' on B — scheduler-svc's relay did NOT swallow characters-svc's event"

echo ""
echo "=== SMOKE PASSED: durable event plane works in the microservices split; single-owner relay holds ==="
