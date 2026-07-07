#!/usr/bin/env bash
# smoke-split-operations.sh — end-to-end proof that the SYNC OPERATION TRANSPORT
# (the unified typed-operation plane: gateway front-handler → OperationBackend →
# op) works in the MICROSERVICES SPLIT, cross-process, through the gateway path,
# with auth done ONCE at the front door over the mutually-authenticated QUIC edge.
#
# This is the at-risk topology for the unified-operation-transport work (the
# monolith path is a same-process direct call; the split is where the gateway
# front, the mTLS edge hop, and the remote auth actually have to compose). Boots
# the full split (characters-svc A, inventory-svc B, scheduler-svc, gateway-svc C)
# via run.sh, then asserts, driving EVERY player request through the gateway-svc
# player front door (:8082) — never the backends' own ports:
#
#   1. GATEWAY DOUBLE-LAYER for player ops: create a character via POST
#      :8082/characters. gateway-svc HTTP-reverse-proxies /characters → A:8080,
#      where A's OWN gateway front-handler (the leaf-slot front, present in every
#      app.Run process) matches the "POST /characters" operation, authenticates
#      the bearer once, and dispatches characters.create via its LocalBackend
#      (accounts + characters are co-hosted in A). 201 + a character id proves the
#      op plane serves a player operation through the gateway front, cross-process.
#   2. The op is real state: GET :8082/characters lists it back (200).
#   3. CROSS-PROCESS mTLS-EDGE AUTH + SYNC op: GET
#      :8082/inventory/character/{id}. gateway-svc proxies /inventory → B:8081,
#      where B has NO local accounts and NO local characters — B's gateway
#      front-handler must VerifySession the bearer over the QUIC edge to A
#      (remote accounts), and the inventory op then SYNC-asks characters.OwnerOf
#      over that SAME mTLS edge to authorize the character's inventory. A starter
#      grant (starter_sword) coming back proves auth-once + the sync op both
#      traversed the mutually-authenticated edge to a DIFFERENT process.
#   4. DELETE :8082/characters/{id} → 204 (the delete op through the gateway).
#   5. BOUNDARY: POST :8082/accounts/register → 404. gateway-svc's proxy map
#      fronts /characters,/inventory,/admin only — /accounts is NOT proxied, so
#      accounts is reachable only on A's own port. This documents the HONEST
#      remaining shape: gateway-svc still HTTP-proxies to the backends' own
#      front-handlers (a functional double-layer), it does NOT yet dispatch ops to
#      backends via a RemoteBackend edge path itself. That unification is the
#      documented remaining work (see the unified-operation-transport status doc).
#
# Exits NON-ZERO on any failed assertion. Repeatable, committed artifact — the
# proof that replaces "trust me, I ran it" for the at-risk (split) topology
# (memory: verify-the-at-risk-path-not-the-safe-one).
#
# Requires: reachable local Postgres (same as run.sh) + curl.
# Run from repo root: ./scripts/smoke-split-operations.sh
set -uo pipefail
cd "$(dirname "$0")/.."

G=http://localhost:8082     # gateway-svc  (player front door: proxies /characters,/inventory,/admin)
A=http://localhost:8080     # characters-svc (accounts + characters)  — accounts' only HTTP home

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

# --- register on A (accounts is NOT fronted by gateway-svc) -----------------
EMAIL="op-smoke-$(date +%s)@test.local"
REG=$(curl -fsS -X POST "$A/accounts/register" -H 'Content-Type: application/json' \
    -d "{\"email\":\"$EMAIL\",\"password\":\"pw12345678\",\"displayName\":\"OpSmoke\"}") \
    || fail "register on A failed"
TOKEN=$(echo "$REG" | sed -E 's/.*"token":"([^"]+)".*/\1/')
[ -n "$TOKEN" ] || fail "no token in register response: $REG"
pass "registered a player on A (accounts) — token acquired"

# --- assertion 1: create a character THROUGH the gateway front door ---------
CH=$(curl -fsS -X POST "$G/characters" -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/json' -d '{"name":"OpSmoke","class":"novice"}') \
    || fail "character create through gateway-svc (:8082) failed — op plane broken in split"
CID=$(echo "$CH" | sed -E 's/.*"id":"([^"]+)".*/\1/')
[ -n "$CID" ] || fail "no character id in gateway response: $CH"
pass "created character $CID via POST :8082/characters (gateway front → A front-handler op → LocalBackend)"

# --- assertion 2: list it back through the gateway --------------------------
LIST=$(curl -fsS "$G/characters" -H "Authorization: Bearer $TOKEN") \
    || fail "character list through gateway-svc failed"
echo "$LIST" | grep -q "\"$CID\"" \
    || fail "created character $CID not in list via :8082 — op state not visible: $LIST"
pass "listed character $CID via GET :8082/characters"

# --- assertion 3: cross-process mTLS-edge auth + sync op (the at-risk hop) ---
# GET :8082/inventory/character/{id} → gateway-svc proxies to B, whose front-handler
# has NO local accounts (verifies the bearer over the mTLS edge to A) and whose
# inventory op sync-asks characters.OwnerOf over that same edge. A starter grant
# lands via the async plane after create, so poll.
GRANTED=""
t=40
while [ "$t" -gt 0 ]; do
    INV=$(curl -fsS "$G/inventory/character/$CID" -H "Authorization: Bearer $TOKEN" 2>/dev/null) || INV=""
    if echo "$INV" | grep -q '"item_id"'; then
        GRANTED=$(echo "$INV" | sed -E 's/.*"item_id":"([^"]+)".*/\1/')
        break
    fi
    t=$((t - 1)); sleep 0.25
done
[ -n "$GRANTED" ] || fail "inventory for $CID never returned a holding via :8082 — cross-process mTLS-edge auth/op path broken"
pass "cross-process mTLS-edge auth+op: GET :8082/inventory/character/$CID → '$GRANTED' (B verified bearer + OwnerOf over the edge to A)"

# --- assertion 4: delete it through the gateway -----------------------------
DCODE=$(curl -s -o /dev/null -w "%{http_code}" -X DELETE "$G/characters/$CID" -H "Authorization: Bearer $TOKEN")
[ "$DCODE" = "204" ] || fail "delete through gateway-svc returned $DCODE, expected 204"
pass "deleted character $CID via DELETE :8082/characters/$CID (204)"

# --- assertion 5: boundary — accounts is NOT fronted by gateway-svc ---------
# Documents the honest remaining shape: gateway-svc HTTP-proxies /characters,
# /inventory, /admin to the backends' OWN front-handlers (double-layer); it does
# not itself dispatch every op via a RemoteBackend edge path, and /accounts is not
# in its proxy map at all.
RCODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$G/accounts/register" \
    -H 'Content-Type: application/json' -d '{"email":"x@y.z","password":"pw12345678","displayName":"X"}')
[ "$RCODE" = "404" ] || fail "POST :8082/accounts/register returned $RCODE, expected 404 (accounts not in gateway-svc proxy map)"
pass "boundary: POST :8082/accounts/register → 404 (gateway-svc fronts /characters,/inventory,/admin only)"

echo ""
echo "=== SMOKE PASSED: sync operation transport works in the split through the gateway path; auth-once over the mTLS edge holds cross-process ==="
