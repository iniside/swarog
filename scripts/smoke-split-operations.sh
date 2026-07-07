#!/usr/bin/env bash
# smoke-split-operations.sh — end-to-end proof that the SYNC OPERATION TRANSPORT
# (the unified typed-operation plane) works in the MICROSERVICES SPLIT with
# gateway-svc as the SINGLE front door: every player request enters :8082 and is
# dispatched to the owning backend over the mutually-authenticated QUIC edge as a
# typed OPERATION (gateway.RemoteBackend), NOT HTTP-reverse-proxied to the
# backend's own front-handler. The former double-layer (gateway-svc HTTP proxy →
# backend front-handler → op) is collapsed into ONE hop: gateway-svc → backend
# edge op.
#
# This is the at-risk topology (the monolith path is a same-process direct call;
# the split is where the gateway front, the mTLS edge hop, and the remote auth
# actually have to compose). Boots the full split (characters-svc A, inventory-svc
# B, scheduler-svc, gateway-svc C) via run.sh, then asserts, driving EVERY player
# request through gateway-svc :8082 — never the backends' own ports:
#
#   1. REGISTER over the edge: POST :8082/accounts/register. /accounts is NOT in
#      gateway-svc's HTTP proxy map — the ONLY way this returns 201 is the op table
#      dispatching accounts.register over the edge to A's accounts edge server. This
#      is the decisive single-hop proof: the double-layer is gone (previously :8082
#      /accounts/register was 404 because accounts was un-proxied).
#   2. CREATE over the edge: POST :8082/characters → 201 + a character id
#      (characters.create dispatched to A's edge, auth verified once at the front).
#   3. LIST over the edge: GET :8082/characters → 200 lists it back.
#   4. CROSS-PROCESS mTLS-EDGE AUTH + SYNC op: GET :8082/inventory/character/{id}.
#      gateway-svc verifies the bearer over the edge to A (accounts), then dispatches
#      inventory.listCharacter to B's edge; the inventory op SYNC-asks
#      characters.OwnerOf over the edge to A to authorize the character. A starter
#      grant coming back proves auth-once + the sync op both traversed the mTLS edge
#      to DIFFERENT processes — all fronted by the single gateway.
#   5. DELETE over the edge: DELETE :8082/characters/{id} → 204.
#
# Exits NON-ZERO on any failed assertion. Repeatable, committed artifact — the
# proof that replaces "trust me, I ran it" for the at-risk (split) topology
# (memory: verify-the-at-risk-path-not-the-safe-one).
#
# Requires: reachable local Postgres (same as run.sh) + curl.
# Run from repo root: ./scripts/smoke-split-operations.sh
set -uo pipefail
cd "$(dirname "$0")/.."

G=http://localhost:8082     # gateway-svc — the SINGLE player front door (edge op dispatch)

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

# --- assertion 1: register THROUGH the gateway as an edge op ----------------
# /accounts is NOT proxied by gateway-svc (only /admin, /accounts/epic are HTTP
# passthrough). A 201 here can ONLY come from the op table dispatching
# accounts.register over the edge to A — the single-gateway path, double-layer gone.
EMAIL="op-smoke-$(date +%s)@test.local"
REG=$(curl -fsS -X POST "$G/accounts/register" -H 'Content-Type: application/json' \
    -d "{\"email\":\"$EMAIL\",\"password\":\"pw12345678\",\"displayName\":\"OpSmoke\"}") \
    || fail "register through gateway-svc (:8082) failed — accounts edge op path broken"
TOKEN=$(echo "$REG" | sed -E 's/.*"token":"([^"]+)".*/\1/')
[ -n "$TOKEN" ] || fail "no token in register response: $REG"
pass "registered a player via POST :8082/accounts/register (edge op → A accounts edge; NOT proxied) — token acquired"

# --- assertion 2: create a character THROUGH the gateway as an edge op -------
CH=$(curl -fsS -X POST "$G/characters" -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/json' -d '{"name":"OpSmoke","class":"novice"}') \
    || fail "character create through gateway-svc (:8082) failed — op plane broken in split"
CID=$(echo "$CH" | sed -E 's/.*"id":"([^"]+)".*/\1/')
[ -n "$CID" ] || fail "no character id in gateway response: $CH"
pass "created character $CID via POST :8082/characters (gateway → characters.create edge op on A)"

# --- assertion 3: list it back through the gateway --------------------------
LIST=$(curl -fsS "$G/characters" -H "Authorization: Bearer $TOKEN") \
    || fail "character list through gateway-svc failed"
echo "$LIST" | grep -q "\"$CID\"" \
    || fail "created character $CID not in list via :8082 — op state not visible: $LIST"
pass "listed character $CID via GET :8082/characters (gateway → characters.list edge op)"

# --- assertion 4: cross-process mTLS-edge auth + sync op (the at-risk hop) ---
# GET :8082/inventory/character/{id} → gateway-svc verifies the bearer over the
# edge to A (accounts), then dispatches inventory.listCharacter to B's edge, whose
# op sync-asks characters.OwnerOf over that same mTLS edge to A. A starter grant
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
pass "cross-process mTLS-edge auth+op: GET :8082/inventory/character/$CID → '$GRANTED' (gateway verified bearer + dispatched inventory op + OwnerOf over the edge)"

# --- assertion 5: delete it through the gateway -----------------------------
DCODE=$(curl -s -o /dev/null -w "%{http_code}" -X DELETE "$G/characters/$CID" -H "Authorization: Bearer $TOKEN")
[ "$DCODE" = "204" ] || fail "delete through gateway-svc returned $DCODE, expected 204"
pass "deleted character $CID via DELETE :8082/characters/$CID (204, gateway → characters.delete edge op)"

echo ""
echo "=== SMOKE PASSED: gateway-svc is the SINGLE front door — every player op dispatched over the mTLS edge (single hop), the double-layer is gone ==="
