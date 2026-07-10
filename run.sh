#!/usr/bin/env bash
# run.sh -- build the rust-sketch binaries, then run either the monolith (one process
# hosting every module) or the FULL split (the 12-service microservice topology, each
# service a binary linking only its own modules and talking to peers over the mTLS QUIC
# edge). The split boot order + per-process env are transcribed from split-proof.sh
# (the source of truth); unlike that proof this script runs NO assertions and leaves
# every process RUNNING -- teardown is the explicit --teardown flag.
#
# Usage:
#   ./run.sh                 # monolith (server) on :8080  (DEFAULT)
#   ./run.sh split           # the full 12-service split (front door on :8082)
#   ./run.sh microservices   # deprecated alias for `split`
#   ./run.sh --teardown      # stop whatever run.sh started last
#
# Assumes a local Postgres is already running (DATABASE_URL or the default DSN).
# Env passthrough: DATABASE_URL, ACCOUNTS_DEV_AUTH, INVENTORY_DEV_GRANT, etc. Dev
# conveniences are now EXPLICIT opt-ins (the modules fail closed by default); this script
# sets them per process that hosts the module, all overridable from the caller's
# environment. The admin portal uses SESSION auth: this script seeds a dev login
# (admin/admin) via `adminctl create-user` before boot, and sets the dev opt-outs
# TLS_MODE=off (plain-http front door) + ADMIN_COOKIE_SECURE=0 (cookie sent over http)
# + TRUSTED_PROXY_CIDRS=127.0.0.1/32 (lockout sees the real client behind the proxy).
set -euo pipefail
cd "$(dirname "$0")"

# --- Live log tee: every invocation writes its full console output to a timestamped
# log file (in addition to the console), with the log path printed FIRST so a human or
# an agent can tail it live.
mkdir -p run/logs
LOG="run/logs/run-$(date +%Y%m%d-%H%M%S).log"
echo "[log] $(pwd)/$LOG"
exec > >(tee -a "$LOG") 2>&1

MODE="monolith"
TEARDOWN=0
for arg in "$@"; do
    case "$arg" in
        monolith) MODE="monolith" ;;
        split) MODE="split" ;;
        microservices) MODE="split"; echo "NOTE: 'microservices' is a deprecated alias for 'split'." >&2 ;;
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
        if curl -fsS -o /dev/null "http://localhost:$port/readyz" 2>/dev/null; then
            echo "$name healthy at http://localhost:$port/readyz"; return 0; fi
        tries=$((tries - 1)); sleep 0.5
    done
    echo "$name did not become healthy within ~30s" >&2
    curl -s "http://localhost:$port/readyz" >&2 2>&1
    return 1
}
write_pids_file() {
    : > "$PIDS_FILE"
    for i in "${!STARTED_NAMES[@]}"; do echo "${STARTED_NAMES[$i]}=${STARTED_PIDS[$i]}" >> "$PIDS_FILE"; done
}

# admin_note -- one-liner about the seeded dev admin login (session auth).
admin_note() {
    echo "    (session login: admin / admin -- seeded via adminctl; ADMIN_COOKIE_SECURE=0 so the cookie rides http)"
}

# seed_admin USER PASS -- mint/reset a dev admin login via adminctl (password over stdin,
# never argv). adminctl ensures schema `admin` + admin.users itself, so it is safe to run
# before the admin module migrates the rest of its schema.
seed_admin() {
    echo "Seeding dev admin login '$1' via adminctl ..."
    printf '%s\n' "$2" | DATABASE_URL="$DATABASE_URL" cargo run -q -p adminctl -- create-user "$1" --password-stdin
}

# --- Build ------------------------------------------------------------------
# Both modes build edgeca + playercli: each topology fronts players over QUIC
# (PLAYER_EDGE_ADDR), so both need the shared dev CA (edgeca) and a client (playercli).
if [ "$MODE" = "monolith" ]; then
    echo "Building server (monolith) + edgeca + adminctl + playercli + csharp-client-gen ..."
    cargo build -p server -p edgeca -p adminctl -p playercli -p csharp-client-gen
else
    echo "Building edgeca + the 12 split services + adminctl + playercli + csharp-client-gen ..."
    cargo build -p edgeca -p adminctl -p playercli -p csharp-client-gen \
        -p accounts-svc -p audit-svc -p scheduler-svc -p rating-svc -p leaderboard-svc \
        -p match-svc -p characters-svc -p config-svc -p inventory-svc -p gateway-svc -p admin-svc \
        -p apikeys-svc
fi
echo "Build OK."

# Windows Git Bash: the cargo binaries carry a .exe suffix; plain Linux does not.
EXE=""
[ -f "$BIN_DIR/server.exe" ] && EXE=".exe"
[ -f "$BIN_DIR/gateway-svc.exe" ] && EXE=".exe"

# --- Monolith ---------------------------------------------------------------
if [ "$MODE" = "monolith" ]; then
    # The monolith ALSO serves the QUIC player front (PLAYER_EDGE_ADDR=:9100, all ops
    # Local) -- per never-monolith-only-features both topologies serve the feature. It
    # needs the shared dev CA to derive the player-front server cert, so mint one here.
    EDGE_CA_CERT="$RUN_DIR/edge-ca.crt"
    EDGE_CA_KEY="$RUN_DIR/edge-ca.key"
    echo "Minting edge dev CA (player front) -> $EDGE_CA_CERT ..."
    "$BIN_DIR/edgeca$EXE" --cert "$EDGE_CA_CERT" --key "$EDGE_CA_KEY"
    # The admin portal uses SESSION auth on schema `admin`; seed a dev login before boot.
    seed_admin "${ADMIN_USER:-admin}" "${ADMIN_PASS:-admin}"
    # Dev conveniences are now EXPLICIT opt-ins (the modules fail closed by default): this
    # dev-boot script enables them (all still overridable by the caller's environment) so
    # local testing works out of the box -- APIKEYS_DEV_SEED (well-known dev keys),
    # ACCOUNTS_DEV_AUTH (/accounts/register+login), INVENTORY_DEV_GRANT (simulated IAP);
    # the admin dev opt-outs TLS_MODE=off + ADMIN_COOKIE_SECURE=0 + TRUSTED_PROXY_CIDRS
    # keep the plain-http portal + session cookie working locally.
    start_server monolith "$BIN_DIR/server$EXE" \
        PORT=:8080 \
        DATABASE_URL="$DATABASE_URL" \
        PLAYER_EDGE_ADDR=:9100 \
        EDGE_CA_CERT="$EDGE_CA_CERT" \
        EDGE_CA_KEY="$EDGE_CA_KEY" \
        APIKEYS_DEV_SEED="${APIKEYS_DEV_SEED:-1}" \
        ACCOUNTS_DEV_AUTH="${ACCOUNTS_DEV_AUTH:-1}" \
        INVENTORY_DEV_GRANT="${INVENTORY_DEV_GRANT:-1}" \
        TLS_MODE=off \
        ADMIN_COOKIE_SECURE=0 \
        TRUSTED_PROXY_CIDRS="127.0.0.1/32"
    wait_healthy 8080 monolith
    write_pids_file
    echo ""
    echo "======================= monolith running ======================="
    echo "  Web UI (SPA demo): http://localhost:8080/"
    echo "  Admin panel:       http://localhost:8080/admin"
    admin_note
    echo "  Player QUIC front: :9100   (drive it with target/debug/playercli$EXE)"
    echo "  Metrics:           http://localhost:8080/metrics"
    echo "  API keys (dev):    X-Api-Key: dev-key-client (player-facing)  |  dev-key-server (full/trusted)"
    echo "  Logs:              $RUN_DIR/monolith.{out,err}.log"
    echo "  Teardown:          ./run.sh --teardown"
    echo "================================================================"
    exit 0
fi

# --- Split (the full 11-service microservice topology) ----------------------
# Boot ORDER + per-process env are transcribed from split-proof.sh. Ordering notes:
#   - config-svc (C) MUST be up before inventory-svc (B): B's config stub boot-fills a
#     snapshot from C in `start` and fails loud if C is unreachable.
#   - accounts-svc (D) first: every gateway verifies bearers against it (lazy dial, so
#     not strictly required, but we mirror the proof's order).
#   - admin-svc (E) last: it dials A/B/C/D/F/H edges to fan out their admin pages.
# Durable events need NO per-process env: every DB process reads the ONE shared
# asyncevents log and pulls only its own subscriptions. Peer *_EDGE_ADDR values are
# NUMERIC host:port (Rust's SocketAddr needs a literal IP). All peers share ONE dev CA.

# HTTP ports 8080-8091, internal mTLS edge ports 9000-9009, player QUIC :9100.
A_PORT=8080; B_PORT=8081; G_PORT=8082; C_PORT=8083; D_PORT=8084; E_PORT=8085
F_PORT=8086; H_PORT=8087; I_PORT=8088; J_PORT=8089; K_PORT=8090; L_PORT=8091
A_EDGE=9000; B_EDGE=9001; C_EDGE=9002; D_EDGE=9003; F_EDGE=9004
H_EDGE=9005; I_EDGE=9006; J_EDGE=9007; K_EDGE=9008; L_EDGE=9009; PLAYER_PORT=9100

EDGE_CA_CERT="$RUN_DIR/edge-ca.crt"
EDGE_CA_KEY="$RUN_DIR/edge-ca.key"
echo "Minting shared edge dev CA -> $EDGE_CA_CERT ..."
"$BIN_DIR/edgeca$EXE" --cert "$EDGE_CA_CERT" --key "$EDGE_CA_KEY"

# The admin portal uses SESSION auth on schema `admin`; seed a dev login before boot
# (Postgres is already up -- adminctl connects via DATABASE_URL and self-heals the schema).
seed_admin "${ADMIN_USER:-admin}" "${ADMIN_PASS:-admin}"

# D: accounts-svc -- owns the accounts schema; serves accounts.verifySession on its edge
# (every other process verifies bearers against it). player.registered is appended to
# the shared durable log (audit-svc pulls it).
echo "Starting D (accounts-svc) on :$D_PORT, edge :$D_EDGE ..."
start_server accounts "$BIN_DIR/accounts-svc$EXE" \
    PORT=":$D_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$D_EDGE" \
    EDGE_CA_CERT="$EDGE_CA_CERT" EDGE_CA_KEY="$EDGE_CA_KEY" \
    ACCOUNTS_DEV_AUTH="${ACCOUNTS_DEV_AUTH:-1}"
wait_healthy "$D_PORT" "D (accounts-svc)"

# L: apikeys-svc -- owns the apikeys schema (plaintext key -> policy); serves
# apikeys.keys on its edge (gateway-svc + admin-svc resolve/dial it via
# APIKEYS_EDGE_ADDR). APIKEYS_DEV_SEED defaults ON for this dev-boot script (still
# overridable) so the well-known dev keys (dev-key-client, dev-key-server) exist.
echo "Starting L (apikeys-svc) on :$L_PORT, edge :$L_EDGE ..."
start_server apikeys "$BIN_DIR/apikeys-svc$EXE" \
    PORT=":$L_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$L_EDGE" \
    EDGE_CA_CERT="$EDGE_CA_CERT" EDGE_CA_KEY="$EDGE_CA_KEY" \
    APIKEYS_DEV_SEED="${APIKEYS_DEV_SEED:-1}"
wait_healthy "$L_PORT" "L (apikeys-svc)"

# F: audit-svc -- append-only ledger, a pure consumer: its pull workers drain its
# subscriptions from the shared log. Serves admin.adminData ("Audit Log") on its edge.
echo "Starting F (audit-svc) on :$F_PORT, edge :$F_EDGE ..."
start_server audit "$BIN_DIR/audit-svc$EXE" \
    PORT=":$F_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$F_EDGE" \
    EDGE_CA_CERT="$EDGE_CA_CERT" EDGE_CA_KEY="$EDGE_CA_KEY"
wait_healthy "$F_PORT" "F (audit-svc)"

# H: scheduler-svc -- DURABLE PRODUCER (1s loop fires scheduler.fired via advisory lock)
# appending to the shared log (audit-svc pulls it). Serves admin.adminData ("Schedules").
echo "Starting H (scheduler-svc) on :$H_PORT, edge :$H_EDGE ..."
start_server scheduler "$BIN_DIR/scheduler-svc$EXE" \
    PORT=":$H_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$H_EDGE" \
    EDGE_CA_CERT="$EDGE_CA_CERT" EDGE_CA_KEY="$EDGE_CA_KEY"
wait_healthy "$H_PORT" "H (scheduler-svc)"

# J: rating-svc -- provides rating.mmr on its edge (match-svc reads it sync) and pulls
# match.finished (+15/-15) from the shared log. In-memory MMR, DB pool for the plane.
echo "Starting J (rating-svc) on :$J_PORT, edge :$J_EDGE ..."
start_server rating "$BIN_DIR/rating-svc$EXE" \
    PORT=":$J_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$J_EDGE" \
    EDGE_CA_CERT="$EDGE_CA_CERT" EDGE_CA_KEY="$EDGE_CA_KEY"
wait_healthy "$J_PORT" "J (rating-svc)"

# K: leaderboard-svc -- owns schema leaderboard; pulls match.finished from the shared
# log (upsert wins+1); serves GET /leaderboard (gateway routes it Remote here).
echo "Starting K (leaderboard-svc) on :$K_PORT, edge :$K_EDGE ..."
start_server leaderboard "$BIN_DIR/leaderboard-svc$EXE" \
    PORT=":$K_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$K_EDGE" \
    EDGE_CA_CERT="$EDGE_CA_CERT" EDGE_CA_KEY="$EDGE_CA_KEY"
wait_healthy "$K_PORT" "K (leaderboard-svc)"

# I: match-svc -- records matches (schema match); DURABLE PRODUCER: `report` SYNC-reads
# both players' MMR from rating-svc (J) over the edge, INSERTs + emit_tx's match.finished
# in one tx onto the shared log (J, K and F pull it).
echo "Starting I (match-svc) on :$I_PORT, edge :$I_EDGE ..."
start_server match "$BIN_DIR/match-svc$EXE" \
    PORT=":$I_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$I_EDGE" \
    EDGE_CA_CERT="$EDGE_CA_CERT" EDGE_CA_KEY="$EDGE_CA_KEY" \
    RATING_EDGE_ADDR="127.0.0.1:$J_EDGE"
wait_healthy "$I_PORT" "I (match-svc)"

# A: characters-svc -- owns schema characters; appends character.created/.deleted to
# the shared log (inventory-svc and audit-svc pull them).
echo "Starting A (characters-svc) on :$A_PORT, edge :$A_EDGE ..."
start_server characters "$BIN_DIR/characters-svc$EXE" \
    PORT=":$A_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$A_EDGE" \
    EDGE_CA_CERT="$EDGE_CA_CERT" EDGE_CA_KEY="$EDGE_CA_KEY"
wait_healthy "$A_PORT" "A (characters-svc)"

# C: config-svc -- owns the config schema + LISTEN/NOTIFY listener; serves config.snapshot
# on its edge; appends config.changed durably (B and F pull it). MUST be up before B
# (B boot-fills from C).
echo "Starting C (config-svc) on :$C_PORT, edge :$C_EDGE ..."
start_server config "$BIN_DIR/config-svc$EXE" \
    PORT=":$C_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$C_EDGE" \
    EDGE_CA_CERT="$EDGE_CA_CERT" EDGE_CA_KEY="$EDGE_CA_KEY"
wait_healthy "$C_PORT" "C (config-svc)"

# B: inventory-svc -- owns schema inventory; serves its OWN edge (:9001) so gateway can
# dispatch inventory.* Remote to it; dials A (owner_of), C (CachedConfig), D (verify).
echo "Starting B (inventory-svc) on :$B_PORT, edge :$B_EDGE ..."
start_server inventory "$BIN_DIR/inventory-svc$EXE" \
    PORT=":$B_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$B_EDGE" \
    EDGE_CA_CERT="$EDGE_CA_CERT" EDGE_CA_KEY="$EDGE_CA_KEY" \
    CHARACTERS_EDGE_ADDR="127.0.0.1:$A_EDGE" \
    CONFIG_EDGE_ADDR="127.0.0.1:$C_EDGE" \
    INVENTORY_DEV_GRANT="${INVENTORY_DEV_GRANT:-1}"
wait_healthy "$B_PORT" "B (inventory-svc)"

# G: gateway-svc -- the dedicated front door: HTTP :8082 + player QUIC :9100. No DB, no
# provider modules: only remote::Stubs, so EVERY op resolves Remote over the edge. Also
# reverse-proxies /admin -> admin-svc (E) and /accounts/epic -> accounts-svc (D).
echo "Starting G (gateway-svc) on :$G_PORT, player QUIC :$PLAYER_PORT ..."
start_server gateway "$BIN_DIR/gateway-svc$EXE" \
    PORT=":$G_PORT" PLAYER_EDGE_ADDR=":$PLAYER_PORT" \
    TLS_MODE=off \
    EDGE_CA_CERT="$EDGE_CA_CERT" EDGE_CA_KEY="$EDGE_CA_KEY" \
    CHARACTERS_EDGE_ADDR="127.0.0.1:$A_EDGE" \
    INVENTORY_EDGE_ADDR="127.0.0.1:$B_EDGE" \
    ACCOUNTS_EDGE_ADDR="127.0.0.1:$D_EDGE" \
    MATCH_EDGE_ADDR="127.0.0.1:$I_EDGE" \
    LEADERBOARD_EDGE_ADDR="127.0.0.1:$K_EDGE" \
    APIKEYS_EDGE_ADDR="127.0.0.1:$L_EDGE" \
    ADMIN_HTTP_ADDR="127.0.0.1:$E_PORT" \
    ACCOUNTS_HTTP_ADDR="127.0.0.1:$D_PORT"
wait_healthy "$G_PORT" "G (gateway-svc)"

# E: admin-svc -- the admin portal (HTTP :8085, its OWN DB schema `admin`, no edge
# server). It DIALS the provider edges (A/B/C/D/F/H) to fan their admin pages out over
# QUIC. Session auth (dev login seeded above): ADMIN_COOKIE_SECURE=0 lets the cookie ride
# plain http and TRUSTED_PROXY_CIDRS=127.0.0.1/32 makes the lockout ip:<addr> subject the
# real client behind the gateway passthrough.
echo "Starting E (admin-svc) on :$E_PORT ..."
start_server admin "$BIN_DIR/admin-svc$EXE" \
    PORT=":$E_PORT" \
    DATABASE_URL="$DATABASE_URL" \
    ADMIN_COOKIE_SECURE=0 TRUSTED_PROXY_CIDRS="127.0.0.1/32" \
    EDGE_CA_CERT="$EDGE_CA_CERT" EDGE_CA_KEY="$EDGE_CA_KEY" \
    CHARACTERS_EDGE_ADDR="127.0.0.1:$A_EDGE" \
    INVENTORY_EDGE_ADDR="127.0.0.1:$B_EDGE" \
    CONFIG_EDGE_ADDR="127.0.0.1:$C_EDGE" \
    ACCOUNTS_EDGE_ADDR="127.0.0.1:$D_EDGE" \
    AUDIT_EDGE_ADDR="127.0.0.1:$F_EDGE" \
    SCHEDULER_EDGE_ADDR="127.0.0.1:$H_EDGE" \
    APIKEYS_EDGE_ADDR="127.0.0.1:$L_EDGE"
wait_healthy "$E_PORT" "E (admin-svc)"

write_pids_file
echo ""
echo "==================== split running (12 services) ===================="
echo "  Front door (gateway-svc): http://localhost:$G_PORT   (player QUIC :$PLAYER_PORT)"
echo "  Admin panel:              http://localhost:$G_PORT/admin   (through the gateway front)"
admin_note
echo "  Metrics (front door):     http://localhost:$G_PORT/metrics"
echo "  API keys (dev):           X-Api-Key: dev-key-client (player-facing)  |  dev-key-server (full/trusted)"
echo ""
echo "  Peers (direct HTTP, normally reached THROUGH the front door):"
echo "    A characters-svc :$A_PORT (edge :$A_EDGE)   B inventory-svc :$B_PORT (edge :$B_EDGE)"
echo "    C config-svc     :$C_PORT (edge :$C_EDGE)   D accounts-svc  :$D_PORT (edge :$D_EDGE)"
echo "    E admin-svc      :$E_PORT               F audit-svc     :$F_PORT (edge :$F_EDGE)"
echo "    H scheduler-svc  :$H_PORT (edge :$H_EDGE)   I match-svc     :$I_PORT (edge :$I_EDGE)"
echo "    J rating-svc     :$J_PORT (edge :$J_EDGE)   K leaderboard-svc :$K_PORT (edge :$K_EDGE)"
echo "    L apikeys-svc    :$L_PORT (edge :$L_EDGE)"
echo ""
echo "  Drive the player QUIC front: target/debug/playercli$EXE --addr 127.0.0.1:$PLAYER_PORT --ca $EDGE_CA_CERT ..."
echo "  Logs:     $RUN_DIR/<service>.{out,err}.log"
echo "  Teardown: ./run.sh --teardown"
echo "====================================================================="
