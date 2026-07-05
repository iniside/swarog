#!/usr/bin/env bash
# run.sh - build the server once, then run it as a monolith or as the
# accounts+characters / inventory+admin microservices split.
#
# Usage:
#   ./run.sh                       # monolith, default DB
#   ./run.sh microservices         # A (accounts,characters) + B (inventory,admin)
#   ./run.sh --teardown            # stop whatever run.sh started last
#   DATABASE_URL=postgres://... ./run.sh
#
# Assumes a local Postgres is already running (same assumption as run-dev.sh).
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
PIDS_FILE="$RUN_DIR/pids"
BIN_DIR="bin"
BIN_PATH="$BIN_DIR/server"

DEFAULT_DSN="postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable"
DATABASE_URL="${DATABASE_URL:-$DEFAULT_DSN}"

# --- Teardown ---------------------------------------------------------------
if [ "$TEARDOWN" -eq 1 ]; then
    if [ ! -f "$PIDS_FILE" ]; then
        echo "No $PIDS_FILE found — nothing to tear down."
        exit 0
    fi
    while IFS='=' read -r name pid; do
        [ -z "$name" ] && continue
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid" && echo "Stopped $name (pid $pid)"
        else
            echo "$name (pid $pid) was not running"
        fi
    done < "$PIDS_FILE"
    rm -f "$PIDS_FILE"
    echo "Teardown complete."
    exit 0
fi

mkdir -p "$BIN_DIR" "$RUN_DIR"

echo "Building ./cmd/server -> $BIN_PATH ..."
go build -o "$BIN_PATH" ./cmd/server
echo "Build OK."

STARTED_PIDS=()
STARTED_NAMES=()

cleanup_on_failure() {
    echo "Launch failed — stopping already-started processes." >&2
    for pid in "${STARTED_PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
    done
}
trap cleanup_on_failure ERR

# start_server NAME PORT VAR=val VAR=val ... — launches bin/server in the
# background with the given env vars set, redirecting stdout/stderr to
# run/<name>.{out,err}.log. Appends the pid to STARTED_PIDS/STARTED_NAMES.
start_server() {
    local name="$1"; shift
    local out="$RUN_DIR/$name.out.log"
    local err="$RUN_DIR/$name.err.log"
    env "$@" "$BIN_PATH" >"$out" 2>"$err" &
    local pid=$!
    STARTED_PIDS+=("$pid")
    STARTED_NAMES+=("$name")
    echo "$pid"
}

# wait_healthy PORT NAME — polls GET /healthz until 200, or fails after ~30s.
wait_healthy() {
    local port="$1" name="$2"
    local url="http://localhost:$port/healthz"
    local tries=60
    while [ "$tries" -gt 0 ]; do
        if curl -fsS -o /dev/null "$url" 2>/dev/null; then
            echo "$name healthy at $url"
            return 0
        fi
        tries=$((tries - 1))
        sleep 0.5
    done
    echo "$name did not become healthy at $url within ~30s" >&2
    return 1
}

write_pids_file() {
    : > "$PIDS_FILE"
    for i in "${!STARTED_NAMES[@]}"; do
        echo "${STARTED_NAMES[$i]}=${STARTED_PIDS[$i]}" >> "$PIDS_FILE"
    done
}

if [ "$MODE" = "monolith" ]; then
    start_server monolith \
        ROLES= \
        PORT=8080 \
        DATABASE_URL="$DATABASE_URL"
    wait_healthy 8080 monolith
    write_pids_file

    echo ""
    echo "=== monolith running ==="
    echo "  http://localhost:8080"
    echo "  logs: $RUN_DIR/monolith.out.log, $RUN_DIR/monolith.err.log"
    echo "  teardown: ./run.sh --teardown"
    exit 0
fi

# --- microservices ---------------------------------------------------------
# Process A: accounts + characters. Hosts the QUIC edge server (:9000) and the
# outbox relay for character.* events. Started FIRST — B's remote stubs and
# the shared accounts schema migration must not race A's first boot (S7).
echo "Starting A (accounts,characters) on :8080, edge :9000 ..."
start_server characters \
    ROLES=accounts,characters \
    PORT=8080 \
    DATABASE_URL="$DATABASE_URL" \
    EDGE_ADDR=:9000 \
    EVENTS_SUBSCRIBERS='character.created=http://localhost:8081/events/character-created;character.deleted=http://localhost:8081/events/character-deleted'
    # EVENTS_SUBSCRIBERS is read by the outbox relay, which runs in the
    # process hosting `characters` — i.e. THIS process (A) — because the
    # relay drains A's own characters.outbox table to remote sinks. It points
    # at B's synchronous sink endpoints, not the other way around.
wait_healthy 8080 "A (accounts,characters)"

# Process B: inventory + admin. accounts/characters resolve via remote stubs
# dialing A's edge server; admin fan-out reaches A's /admin-data/characters
# over PEER_HTTP_ADDR.
echo "Starting B (inventory,admin) on :8081 ..."
start_server inventory \
    ROLES=inventory,admin \
    PORT=8081 \
    DATABASE_URL="$DATABASE_URL" \
    CHARACTERS_EDGE_ADDR=localhost:9000 \
    ACCOUNTS_EDGE_ADDR=localhost:9000 \
    PEER_HTTP_ADDR=localhost:8080
wait_healthy 8081 "B (inventory,admin)"

write_pids_file

echo ""
echo "=== microservices running ==="
echo "  A (accounts,characters): http://localhost:8080  (edge :9000)"
echo "  B (inventory,admin):     http://localhost:8081"
echo "  admin UI (B):            http://localhost:8081/admin"
echo "  logs: $RUN_DIR/characters.{out,err}.log, $RUN_DIR/inventory.{out,err}.log"
echo "  teardown: ./run.sh --teardown"
