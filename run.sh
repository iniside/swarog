#!/usr/bin/env bash
# run.sh - build the per-service binaries, then run either the monolith (one
# binary hosting every module) or the two-process microservices split, where
# EACH service is its OWN binary linking only its own modules.
#
# Usage:
#   ./run.sh                       # monolith (bin/server), default DB
#   ./run.sh microservices         # characters-svc + inventory-svc (two binaries)
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
SERVER_BIN="$BIN_DIR/server"
CHARACTERS_BIN="$BIN_DIR/characters-svc"
INVENTORY_BIN="$BIN_DIR/inventory-svc"
SCHEDULER_BIN="$BIN_DIR/scheduler-svc"
GATEWAY_BIN="$BIN_DIR/gateway-svc"

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

# Build only the binaries this mode needs. Each `go build` links ONLY the
# packages its entrypoint imports — the microservice binaries do not carry the
# other service's modules.
if [ "$MODE" = "monolith" ]; then
    echo "Building ./cmd/server -> $SERVER_BIN ..."
    go build -o "$SERVER_BIN" ./cmd/server
else
    echo "Building ./cmd/characters-svc -> $CHARACTERS_BIN ..."
    go build -o "$CHARACTERS_BIN" ./cmd/characters-svc
    echo "Building ./cmd/inventory-svc -> $INVENTORY_BIN ..."
    go build -o "$INVENTORY_BIN" ./cmd/inventory-svc
    echo "Building ./cmd/scheduler-svc -> $SCHEDULER_BIN ..."
    go build -o "$SCHEDULER_BIN" ./cmd/scheduler-svc
    echo "Building ./cmd/gateway-svc -> $GATEWAY_BIN ..."
    go build -o "$GATEWAY_BIN" ./cmd/gateway-svc
fi
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

# start_server NAME BIN VAR=val VAR=val ... — launches BIN in the background
# with the given env vars set, redirecting stdout/stderr to
# run/<name>.{out,err}.log. Appends the pid to STARTED_PIDS/STARTED_NAMES.
start_server() {
    local name="$1"; shift
    local bin="$1"; shift
    local out="$RUN_DIR/$name.out.log"
    local err="$RUN_DIR/$name.err.log"
    env "$@" "$bin" >"$out" 2>"$err" &
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
    start_server monolith "$SERVER_BIN" \
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
# Mint ONE shared dev CA for the edge mutual-TLS hop. Every edge process
# (characters-svc + inventory-svc + gateway-svc) mints its own short-lived leaf
# under THIS CA, so a backend accepts a stream ONLY from a peer holding a
# CA-signed client cert (and each client verifies the server against the same
# anchor). scheduler-svc has NO edge (no server, no client) so it needs no CA;
# the monolith runs no edge at all and sets nothing.
EDGE_CA_CERT="$RUN_DIR/edge-ca.crt"
EDGE_CA_KEY="$RUN_DIR/edge-ca.key"
echo "Minting shared edge dev CA -> $EDGE_CA_CERT ..."
go run ./tools/edgeca -cert "$EDGE_CA_CERT" -key "$EDGE_CA_KEY"

# Process A: characters-svc (accounts + characters, its OWN binary). Hosts the
# QUIC edge server (:9000) and the outbox relay for character.* events. Started
# FIRST — B's remote stubs and the shared accounts schema migration must not
# race A's first boot (S7).
echo "Starting A (characters-svc: accounts,characters) on :8080, edge :9000 ..."
start_server characters "$CHARACTERS_BIN" \
    PORT=8080 \
    DATABASE_URL="$DATABASE_URL" \
    EDGE_ADDR=:9000 \
    EDGE_CA_CERT="$EDGE_CA_CERT" \
    EDGE_CA_KEY="$EDGE_CA_KEY" \
    MESSAGING_ORIGIN=characters-svc \
    EVENTS_SUBSCRIBERS='character.created=http://localhost:8081/events;character.deleted=http://localhost:8081/events'
    # EVENTS_SUBSCRIBERS is read by messaging's relay, which runs in the
    # process hosting `characters` — i.e. THIS process (A) — because the
    # relay drains only ITS OWN origin's rows in messaging.outbox (origin=
    # characters-svc) and delivers them to remote peers. Both topics point at
    # B's single consolidated inbound route (POST /events, topic in the
    # X-Event-Topic header) — there is no more per-topic sink path.
    # MESSAGING_ORIGIN must be stable across restarts (never a pid/hostname)
    # so a crashed process resumes draining its own unsent outbox rows.
wait_healthy 8080 "A (characters-svc)"

# Process B: inventory-svc (inventory + admin, its OWN binary). accounts/
# characters resolve via remote stubs dialing A's edge server; admin fan-out
# reaches A's /admin-data/characters over PEER_HTTP_ADDR.
echo "Starting B (inventory-svc: inventory,admin) on :8081, edge :9001 ..."
start_server inventory "$INVENTORY_BIN" \
    PORT=8081 \
    DATABASE_URL="$DATABASE_URL" \
    EDGE_ADDR=:9001 \
    EDGE_CA_CERT="$EDGE_CA_CERT" \
    EDGE_CA_KEY="$EDGE_CA_KEY" \
    CHARACTERS_EDGE_ADDR=localhost:9000 \
    ACCOUNTS_EDGE_ADDR=localhost:9000 \
    PEER_HTTP_ADDR=localhost:8080 \
    MESSAGING_ORIGIN=inventory-svc
wait_healthy 8081 "B (inventory-svc)"

# Process D: scheduler-svc (scheduler ONLY, its OWN binary, no edge). A pure
# event producer: its messaging relay POSTs scheduler.fired to B's consolidated
# POST /events route, where audit (hosted in B) durably consumes it via OnTx.
# Started after B so the sink exists; the relay retries regardless.
echo "Starting D (scheduler-svc: scheduler) on :8083 ..."
start_server scheduler "$SCHEDULER_BIN" \
    PORT=8083 \
    DATABASE_URL="$DATABASE_URL" \
    MESSAGING_ORIGIN=scheduler-svc \
    EVENTS_SUBSCRIBERS='scheduler.fired=http://localhost:8081/events'
wait_healthy 8083 "D (scheduler-svc)"

# Process C: gateway-svc (stateless QUIC prefix router + HTTP reverse proxy
# front door). Fronts both A and B, so it starts LAST, once both are healthy.
echo "Starting C (gateway-svc: player front door) on :8082, edge :9100 ..."
start_server gateway "$GATEWAY_BIN" \
    PORT=8082 \
    GATEWAY_EDGE_ADDR=:9100 \
    EDGE_CA_CERT="$EDGE_CA_CERT" \
    EDGE_CA_KEY="$EDGE_CA_KEY" \
    CHARACTERS_EDGE_ADDR=localhost:9000 \
    INVENTORY_EDGE_ADDR=localhost:9001 \
    CHARACTERS_HTTP_ADDR=localhost:8080 \
    INVENTORY_HTTP_ADDR=localhost:8081
wait_healthy 8082 "C (gateway-svc)"

write_pids_file

echo ""
echo "=== microservices running ==="
echo "  A (characters-svc: accounts,characters): http://localhost:8080  (edge :9000)"
echo "  B (inventory-svc: inventory,admin):      http://localhost:8081  (edge :9001)"
echo "  D (scheduler-svc: scheduler):            http://localhost:8083  (event producer, no edge)"
echo "  admin UI (B):                            http://localhost:8081/admin"
echo "  player front door (gateway):              quic localhost:9100 / http http://localhost:8082"
echo "  logs: $RUN_DIR/characters.{out,err}.log, $RUN_DIR/inventory.{out,err}.log, $RUN_DIR/scheduler.{out,err}.log, $RUN_DIR/gateway.{out,err}.log"
echo "  teardown: ./run.sh --teardown"
