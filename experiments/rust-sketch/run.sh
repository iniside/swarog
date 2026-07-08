#!/usr/bin/env bash
# run.sh -- build the rust-sketch binaries, then run either the monolith (one process
# hosting every module) or the two-process split (characters-svc = A, inventory-svc =
# B), where each service is its own binary linking only its own modules.
#
# Usage:
#   ./run.sh                 # monolith (server) on :8080
#   ./run.sh microservices   # A (characters-svc) + B (inventory-svc)
#   ./run.sh --teardown      # stop whatever run.sh started last
#
# Assumes a local Postgres is already running (DATABASE_URL or the default DSN).
set -euo pipefail
cd "$(dirname "$0")"

MODE="monolith"
TEARDOWN=0
for arg in "$@"; do
    case "$arg" in
        monolith|microservices) MODE="$arg" ;;
        --teardown) TEARDOWN=1 ;;
        *) echo "unknown arg: $arg" >&2; exit 1 ;;
    esac
done

RUN_DIR="run"
PIDS_FILE="$RUN_DIR/pids.txt"
BIN_DIR="target/debug"

DEFAULT_DSN="postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable"
DATABASE_URL="${DATABASE_URL:-$DEFAULT_DSN}"

# --- Teardown ---------------------------------------------------------------
if [ "$TEARDOWN" -eq 1 ]; then
    if [ ! -f "$PIDS_FILE" ]; then echo "No $PIDS_FILE -- nothing to tear down."; exit 0; fi
    while IFS='=' read -r name pid; do
        [ -z "$name" ] && continue
        if kill -0 "$pid" 2>/dev/null; then kill "$pid" && echo "Stopped $name (pid $pid)"
        else echo "$name (pid $pid) was not running"; fi
    done < "$PIDS_FILE"
    rm -f "$PIDS_FILE"
    echo "Teardown complete."
    exit 0
fi

mkdir -p "$RUN_DIR"

STARTED_PIDS=()
STARTED_NAMES=()
# start_server NAME EXE VAR=val ... -- launch EXE in the background with env set.
start_server() {
    local name="$1"; shift
    local exe="$1"; shift
    env "$@" "$exe" >"$RUN_DIR/$name.out.log" 2>"$RUN_DIR/$name.err.log" &
    STARTED_PIDS+=("$!"); STARTED_NAMES+=("$name")
}
wait_healthy() {
    local port="$1" name="$2" tries=60
    while [ "$tries" -gt 0 ]; do
        if curl -fsS -o /dev/null "http://localhost:$port/healthz" 2>/dev/null; then
            echo "$name healthy at http://localhost:$port/healthz"; return 0; fi
        tries=$((tries - 1)); sleep 0.5
    done
    echo "$name did not become healthy within ~30s" >&2; return 1
}
write_pids_file() {
    : > "$PIDS_FILE"
    for i in "${!STARTED_NAMES[@]}"; do echo "${STARTED_NAMES[$i]}=${STARTED_PIDS[$i]}" >> "$PIDS_FILE"; done
}

# --- Build ------------------------------------------------------------------
# Both modes build edgeca + playercli: the monolith ALSO fronts players over QUIC
# (PLAYER_EDGE_ADDR), so it needs the shared dev CA (edgeca) and a client (playercli).
if [ "$MODE" = "monolith" ]; then
    echo "Building server (monolith) + edgeca + playercli ..."
    cargo build -p server -p edgeca -p playercli
else
    echo "Building edgeca + characters-svc + inventory-svc + gateway-svc + playercli ..."
    cargo build -p edgeca -p characters-svc -p inventory-svc -p gateway-svc -p playercli
fi
echo "Build OK."

# Windows Git Bash: the cargo binaries carry a .exe suffix; plain Linux does not.
EXE=""
[ -f "$BIN_DIR/server.exe" ] && EXE=".exe"

# --- Monolith ---------------------------------------------------------------
if [ "$MODE" = "monolith" ]; then
    # The monolith ALSO serves the QUIC player front (PLAYER_EDGE_ADDR=:9100, all ops
    # Local) -- per never-monolith-only-features both topologies serve the feature. It
    # needs the shared dev CA to derive the player-front server cert, so mint one here.
    EDGE_CA_CERT="$RUN_DIR/edge-ca.crt"
    EDGE_CA_KEY="$RUN_DIR/edge-ca.key"
    echo "Minting edge dev CA (player front) -> $EDGE_CA_CERT ..."
    "$BIN_DIR/edgeca$EXE" --cert "$EDGE_CA_CERT" --key "$EDGE_CA_KEY"
    # default MESSAGING_ORIGIN ("monolith") is fine -- one process, one origin.
    start_server monolith "$BIN_DIR/server$EXE" \
        PORT=:8080 \
        DATABASE_URL="$DATABASE_URL" \
        PLAYER_EDGE_ADDR=:9100 \
        EDGE_CA_CERT="$EDGE_CA_CERT" \
        EDGE_CA_KEY="$EDGE_CA_KEY"
    wait_healthy 8080 monolith
    write_pids_file
    echo ""
    echo "=== monolith running ==="
    echo "  http://localhost:8080  (player QUIC :9100)"
    echo "  teardown: ./run.sh --teardown"
    exit 0
fi

# --- Microservices ----------------------------------------------------------
# Mint ONE shared dev CA for the edge mutual-TLS hop. Both A and B load it via
# EDGE_CA_CERT / EDGE_CA_KEY, so a backend accepts a stream ONLY from a peer holding a
# CA-signed client cert (and each client verifies the server against the same root).
EDGE_CA_CERT="$RUN_DIR/edge-ca.crt"
EDGE_CA_KEY="$RUN_DIR/edge-ca.key"
echo "Minting shared edge dev CA -> $EDGE_CA_CERT ..."
"$BIN_DIR/edgeca$EXE" --cert "$EDGE_CA_CERT" --key "$EDGE_CA_KEY"

# Process A: characters-svc. Hosts the QUIC edge server (:9000) and the outbox relay
# for character.* events. MESSAGING_ORIGIN MUST be distinct per process (never the
# "monolith" default): the relay drains ONLY its own origin's outbox rows, so a shared
# origin would have B's relay drain A's rows -- the async-split correctness lynchpin.
echo "Starting A (characters-svc: gateway,characters,messaging) on :8080, edge :9000 ..."
start_server characters "$BIN_DIR/characters-svc$EXE" \
    PORT=:8080 \
    DATABASE_URL="$DATABASE_URL" \
    EDGE_ADDR=:9000 \
    EDGE_CA_CERT="$EDGE_CA_CERT" \
    EDGE_CA_KEY="$EDGE_CA_KEY" \
    MESSAGING_ORIGIN=characters-svc \
    EVENTS_SUBSCRIBERS='character.created=http://localhost:8081/events;character.deleted=http://localhost:8081/events'
wait_healthy 8080 "A (characters-svc)"

# Process B: inventory-svc. characters resolves via a remote::Stub dialing A's edge
# server. B ALSO serves its OWN mTLS edge (EDGE_ADDR=:9001) so gateway-svc can dispatch
# inventory.* Remote to it. CHARACTERS_EDGE_ADDR is a NUMERIC host:port (Rust's
# SocketAddr needs a literal IP, unlike Go's dialer).
echo "Starting B (inventory-svc: gateway,config,inventory,messaging,remote) on :8081, edge :9001 ..."
start_server inventory "$BIN_DIR/inventory-svc$EXE" \
    PORT=:8081 \
    DATABASE_URL="$DATABASE_URL" \
    EDGE_ADDR=:9001 \
    EDGE_CA_CERT="$EDGE_CA_CERT" \
    EDGE_CA_KEY="$EDGE_CA_KEY" \
    CHARACTERS_EDGE_ADDR=127.0.0.1:9000 \
    MESSAGING_ORIGIN=inventory-svc
wait_healthy 8081 "B (inventory-svc)"

# Process G: gateway-svc. The dedicated front door -- HTTP :8082 + player QUIC :9100.
# No DB, no provider modules: only remote::Stubs, so EVERY op it fronts resolves Remote
# and is dialed over the mTLS edge to A (:9000) / B (:9001). It needs the shared CA to
# dial peers AND to derive the player-front server cert.
echo "Starting G (gateway-svc: gateway + characters/inventory stubs) on :8082, player QUIC :9100 ..."
start_server gateway "$BIN_DIR/gateway-svc$EXE" \
    PORT=:8082 \
    PLAYER_EDGE_ADDR=:9100 \
    EDGE_CA_CERT="$EDGE_CA_CERT" \
    EDGE_CA_KEY="$EDGE_CA_KEY" \
    CHARACTERS_EDGE_ADDR=127.0.0.1:9000 \
    INVENTORY_EDGE_ADDR=127.0.0.1:9001
wait_healthy 8082 "G (gateway-svc)"

write_pids_file
echo ""
echo "=== microservices running ==="
echo "  A (characters-svc): http://localhost:8080  (edge :9000)"
echo "  B (inventory-svc):  http://localhost:8081  (edge :9001)"
echo "  G (gateway-svc):    http://localhost:8082  (player QUIC :9100)"
echo "  teardown: ./run.sh --teardown"
