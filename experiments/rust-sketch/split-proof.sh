#!/usr/bin/env bash
# split-proof.sh -- the SPLIT-topology proof for the rust-sketch (Step 12).
#
# This is the whole point of the milestone: it exercises the TWO-PROCESS split
# (characters-svc = A on :8080 / edge :9000, inventory-svc = B on :8081), NOT the
# monolith, driving the real player flows over HTTP (through the gateway front-door
# with a dev-<uuid> bearer) and the sync authz over QUIC/mTLS. It:
#
#   1. mints the shared dev CA via `edgeca`,
#   2. starts A then B in the background, gating each on /healthz,
#   3. runs the assertions below, tearing BOTH down on exit (even on failure),
#   4. exits non-zero if ANY assertion fails.
#
# THE PROOF (all against the SPLIT, over real HTTP/QUIC):
#   - Async event, cross-process A->B: POST /characters on A -> 201; A emits
#     character.created; its relay POSTs to B /events; inventory's durable on_tx
#     grants the starter item. Poll GET /inventory/character/<id> on B until the
#     starter (starter_sword x1) appears.
#   - Sync call over QUIC B->A: that same GET forces inventory's list_character to
#     call owner_of via the remote Stub over QUIC/mTLS to A for the authz check -- a
#     200 with the holding proves the sync cross-process path AND mTLS worked. The
#     NEGATIVE authz: the same GET with a DIFFERENT player's bearer -> 403 (the
#     character is not theirs), proving OwnerOf actually gates.
#   - Integrity via event, not FK, A->B: DELETE /characters/<id> on A -> 204; A emits
#     character.deleted; inventory's on_tx wipes the character's holdings. We assert
#     the DB holdings row is genuinely gone (the wipe handler ran) -- the HTTP 404
#     after delete alone only proves the character is gone via owner_of, which would
#     mask an un-wiped row, so the DB check is the real integrity assertion.
#
# ASCII only (no em-dashes): PowerShell 5.1 chokes on them; keep the sibling .ps1
# and this in lockstep.
#
# Assumes a local Postgres reachable at DATABASE_URL (or the default DSN).
set -uo pipefail
cd "$(dirname "$0")"

RUN_DIR="run"
BIN_DIR="target/debug"
CA_CERT="$RUN_DIR/edge-ca.crt"
CA_KEY="$RUN_DIR/edge-ca.key"
A_PORT=8080
B_PORT=8081
EDGE_PORT=9000

DEFAULT_DSN="postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable"
DATABASE_URL="${DATABASE_URL:-$DEFAULT_DSN}"

FAILS=0
A_PID=""
B_PID=""

note()  { echo "[proof] $*"; }
pass()  { echo "  PASS  $*"; }
fail()  { echo "  FAIL  $*"; FAILS=$((FAILS + 1)); }

# Windows Git Bash appends .exe; plain Linux does not.
EXE=""
[ -f "$BIN_DIR/edgeca.exe" ] && EXE=".exe"

# --- uuid: a fresh player id per run keeps reruns idempotent (owner rows are keyed
# by player/character, so nothing to clean up) --------------------------------
new_uuid() {
    if [ -r /proc/sys/kernel/random/uuid ]; then
        cat /proc/sys/kernel/random/uuid
    elif command -v powershell >/dev/null 2>&1; then
        powershell -NoProfile -Command "[guid]::NewGuid().ToString()" | tr -d '\r'
    else
        python -c 'import uuid;print(uuid.uuid4())'
    fi
}

# --- psql discovery (local Postgres is the test DB) --------------------------
find_psql() {
    if command -v psql >/dev/null 2>&1; then command -v psql; return; fi
    local p
    for p in /c/Program\ Files/PostgreSQL/*/bin/psql.exe; do
        [ -f "$p" ] && { echo "$p"; return; }
    done
    echo ""
}
PSQL="$(find_psql)"

# --- teardown: kill both processes on ANY exit -------------------------------
teardown() {
    [ -n "$A_PID" ] && kill "$A_PID" 2>/dev/null && note "stopped A (pid $A_PID)"
    [ -n "$B_PID" ] && kill "$B_PID" 2>/dev/null && note "stopped B (pid $B_PID)"
    A_PID=""; B_PID=""
}
trap teardown EXIT INT TERM

# --- clear any stragglers from an aborted prior run (idempotent reruns) ------
kill_stragglers() {
    # By name (Windows), best-effort.
    if command -v taskkill >/dev/null 2>&1; then
        taskkill //F //IM characters-svc.exe >/dev/null 2>&1 || true
        taskkill //F //IM inventory-svc.exe >/dev/null 2>&1 || true
    fi
    pkill -f "characters-svc" 2>/dev/null || true
    pkill -f "inventory-svc"  2>/dev/null || true
}

wait_healthy() {
    local port="$1" name="$2" tries=60
    while [ "$tries" -gt 0 ]; do
        if curl -fsS -o /dev/null "http://localhost:$port/healthz" 2>/dev/null; then
            note "$name healthy on :$port"; return 0
        fi
        tries=$((tries - 1)); sleep 0.5
    done
    note "$name NEVER became healthy on :$port"; return 1
}

# ============================================================================
note "building edgeca + characters-svc + inventory-svc ..."
if ! cargo build -p edgeca -p characters-svc -p inventory-svc; then
    echo "build failed"; exit 1
fi

mkdir -p "$RUN_DIR"
kill_stragglers
sleep 1

note "minting shared edge dev CA -> $CA_CERT"
"$BIN_DIR/edgeca$EXE" --cert "$CA_CERT" --key "$CA_KEY"

# --- start A (characters-svc): gateway + characters + messaging, edge :9000 --
# MESSAGING_ORIGIN MUST be distinct per process (never the "monolith" default): the
# relay drains ONLY its own origin's outbox rows.
note "starting A (characters-svc) on :$A_PORT, edge :$EDGE_PORT ..."
env PORT=":$A_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$EDGE_PORT" \
    EDGE_CA_CERT="$CA_CERT" EDGE_CA_KEY="$CA_KEY" \
    MESSAGING_ORIGIN=characters-svc \
    EVENTS_SUBSCRIBERS="character.created=http://localhost:$B_PORT/events;character.deleted=http://localhost:$B_PORT/events" \
    "$BIN_DIR/characters-svc$EXE" >"$RUN_DIR/characters.out.log" 2>"$RUN_DIR/characters.err.log" &
A_PID=$!
wait_healthy "$A_PORT" "A (characters-svc)" || { echo "A failed to start"; exit 1; }

# --- start B (inventory-svc): gateway + config + inventory + messaging + stub -
note "starting B (inventory-svc) on :$B_PORT ..."
env PORT=":$B_PORT" DATABASE_URL="$DATABASE_URL" \
    EDGE_CA_CERT="$CA_CERT" EDGE_CA_KEY="$CA_KEY" \
    CHARACTERS_EDGE_ADDR="127.0.0.1:$EDGE_PORT" \
    MESSAGING_ORIGIN=inventory-svc \
    "$BIN_DIR/inventory-svc$EXE" >"$RUN_DIR/inventory.out.log" 2>"$RUN_DIR/inventory.err.log" &
B_PID=$!
wait_healthy "$B_PORT" "B (inventory-svc)" || { echo "B failed to start"; exit 1; }

PID="$(new_uuid)"
OTHER="$(new_uuid)"
note "player PID=$PID  (other player=$OTHER)"

echo ""
echo "================ SPLIT PROOF ================"

# --- 1. CREATE on A (gateway HTTP op -> characters) --------------------------
echo "[1] POST http://localhost:$A_PORT/characters (Bearer dev-\$PID)"
CREATE="$(curl -s -w $'\n%{http_code}' -X POST "http://localhost:$A_PORT/characters" \
    -H "Authorization: Bearer dev-$PID" -H "Content-Type: application/json" \
    -d '{"name":"Aria","class":"mage"}')"
CBODY="$(echo "$CREATE" | sed '$d')"; CCODE="$(echo "$CREATE" | tail -1)"
echo "    -> HTTP $CCODE  $CBODY"
CID="$(echo "$CBODY" | grep -o '"id":"[^"]*"' | head -1 | sed 's/"id":"//;s/"//')"
if [ "$CCODE" = "201" ] && [ -n "$CID" ]; then pass "create -> 201, id=$CID"; else fail "create expected 201 with id"; fi

# --- 2. ASYNC event A->B + SYNC authz B->A over QUIC -------------------------
echo "[2] poll GET http://localhost:$B_PORT/inventory/character/$CID until starter appears"
STARTER_OK=0
for i in $(seq 1 30); do
    R="$(curl -s -w $'\n%{http_code}' "http://localhost:$B_PORT/inventory/character/$CID" \
        -H "Authorization: Bearer dev-$PID")"
    BODY="$(echo "$R" | sed '$d')"; CODE="$(echo "$R" | tail -1)"
    if [ "$CODE" = "200" ] && echo "$BODY" | grep -q 'starter_sword'; then
        echo "    attempt $i -> HTTP 200 $BODY"
        pass "starter_sword materialized in B (async event A->B) AND 200 proves owner_of over QUIC/mTLS B->A"
        STARTER_OK=1; break
    fi
    sleep 0.5
done
[ "$STARTER_OK" = "1" ] || fail "starter never appeared in B (async cross-process grant / QUIC authz)"

# --- 3. NEGATIVE authz: a different player is forbidden ----------------------
echo "[3] GET /inventory/character/$CID as a DIFFERENT player (Bearer dev-\$OTHER)"
NEG="$(curl -s -w $'\n%{http_code}' "http://localhost:$B_PORT/inventory/character/$CID" \
    -H "Authorization: Bearer dev-$OTHER")"
NBODY="$(echo "$NEG" | sed '$d')"; NCODE="$(echo "$NEG" | tail -1)"
echo "    -> HTTP $NCODE  $NBODY"
if [ "$NCODE" = "403" ] || [ "$NCODE" = "404" ]; then
    pass "other player -> $NCODE (owner_of over QUIC gates: not their character)"
else
    fail "negative authz expected 403/404, got $NCODE"
fi

# --- 4. DELETE on A ----------------------------------------------------------
echo "[4] DELETE http://localhost:$A_PORT/characters/$CID (Bearer dev-\$PID)"
DEL="$(curl -s -w $'\n%{http_code}' -X DELETE "http://localhost:$A_PORT/characters/$CID" \
    -H "Authorization: Bearer dev-$PID")"
DCODE="$(echo "$DEL" | tail -1)"
echo "    -> HTTP $DCODE"
if [ "$DCODE" = "204" ]; then pass "delete -> 204"; else fail "delete expected 204, got $DCODE"; fi

# --- 5. INTEGRITY via event, not FK: holdings wiped in B --------------------
# The definitive assertion is the DB row count (the on_tx wipe handler ran). The HTTP
# 404 after delete alone only proves the character is gone via owner_of and would mask
# an un-wiped holdings row, so we assert the DB directly (local Postgres is the test DB).
echo "[5] poll B until the character's holdings are WIPED (character.deleted A->B)"
if [ -n "$PSQL" ]; then
    WIPED=0
    for i in $(seq 1 30); do
        N="$(PGPASSWORD=gamebackend "$PSQL" -U gamebackend -h localhost -d gamebackend -t -A -c \
            "SELECT count(*) FROM inventory.holdings WHERE owner_type='character' AND owner_id='$CID';" 2>/dev/null | tr -d '[:space:]')"
        echo "    attempt $i -> inventory.holdings rows for $CID = ${N:-?}"
        if [ "$N" = "0" ]; then pass "holdings row wiped in B (integrity via character.deleted event, no FK cascade)"; WIPED=1; break; fi
        sleep 0.5
    done
    [ "$WIPED" = "1" ] || fail "holdings never wiped in B (wipe on_tx handler did not run)"
else
    note "psql not found -- falling back to HTTP 404 as a WEAKER wipe signal"
    W="$(curl -s -o /dev/null -w '%{http_code}' "http://localhost:$B_PORT/inventory/character/$CID" -H "Authorization: Bearer dev-$PID")"
    echo "    -> HTTP $W"
    if [ "$W" = "404" ]; then pass "post-delete GET -> 404 (character gone; DB wipe unverified, psql missing)"; else fail "post-delete expected 404, got $W"; fi
fi

# Also record the HTTP 404 (character gone via owner_of over QUIC) for the evidence doc.
echo "[5b] post-delete GET /inventory/character/$CID (Bearer dev-\$PID)"
W2="$(curl -s -w $'\n%{http_code}' "http://localhost:$B_PORT/inventory/character/$CID" -H "Authorization: Bearer dev-$PID")"
echo "    -> HTTP $(echo "$W2" | tail -1)  $(echo "$W2" | sed '$d')"

echo "============================================"
if [ "$FAILS" -eq 0 ]; then
    echo "SPLIT PROOF: PASS (all assertions held on the two-process topology)"
    exit 0
else
    echo "SPLIT PROOF: FAIL ($FAILS assertion(s) failed)"
    exit 1
fi
