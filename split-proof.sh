#!/usr/bin/env bash
# split-proof.sh -- the SPLIT-topology proof for the rust-sketch (Steps 12 + 8).
#
# This is the whole point of the milestone: it exercises the TWELVE-PROCESS split
# (characters-svc = A on :8080 / edge :9000, inventory-svc = B on :8081 / edge :9001,
# gateway-svc = G on :8082 / player QUIC :9100, config-svc = C on :8083 / edge :9002,
# accounts-svc = D on :8084 / edge :9003, admin-svc = E on :8085, audit-svc = F on
# :8086 / edge :9004, scheduler-svc = H on :8087 / edge :9005, match-svc = I on :8088 /
# edge :9006, rating-svc = J on :8089 / edge :9007, leaderboard-svc = K on :8090 /
# edge :9008, apikeys-svc = L on :8091 / edge :9009),
# NOT the monolith, driving the real
#
# Port assignments here are manual config (this table); FLEET MEMBERSHIP (the set of
# `cmd/*-svc` processes) is the drift-guarded source of truth in
# tools/checkmodules::split_fleet_matches_cmd_dirs (Step 15) -- add a new svc there
# before adding it to this script. This script ALSO self-checks that assumption:
# fleet_preflight compares FLEET_SVCS against the cmd/*-svc directories on disk and
# dies (naming exactly what is missing/stale) BEFORE booting anything, so a
# forgotten svc is a loud failure, never a silently weaker proof.
# player flows over HTTP (through the gateway front-door with a REAL bearer minted
# by register+login through the front -- Step 6 replaced the dev-<uuid> tokens),
# the sync authz over QUIC/mTLS, AND the NEW dedicated QUIC player front (Step 8 of
# the QUIC player-front plan): external players connect to gateway-svc over QUIC
# (server-cert-only TLS), the front auth-verifies the bearer-in-envelope once and
# dispatches the method through the route table (allow-list gate) to the owning peer
# over the internal mTLS edge. It:
#
#   1. mints the shared dev CA via `edgeca`,
#   2. starts A, B, then G in the background, gating each on /healthz,
#   3. runs the assertions below, tearing ALL down on exit (even on failure),
#   4. as a final stage boots the monolith (cmd/server) with the SAME player QUIC
#      front and proves parity (never-monolith-only-features), and
#   5. exits non-zero if ANY assertion fails.
#
# THE PROOF (all against the SPLIT, over real HTTP/QUIC):
#   - API KEY POLICY (api-key-policy plan, Step 4): every op-dispatched request now
#     ALSO carries `X-Api-Key`/`--api-key`, checked BEFORE session auth. Every op curl
#     below carries `dev-key-client` (the player-facing policy) EXCEPT match/report,
#     which carries `dev-key-server` (`full`) -- `dev-client`'s policy deliberately
#     omits match.report. [K1]-[K4] right after [A5] assert the policy directly: no
#     key -> 401, bogus key -> 401, dev-key-client on match.report -> 403 (policy
#     denies), dev-key-server on match.report -> 202 (allowed). Keyless surfaces stay
#     keyless: /healthz, /metrics, /admin* (session-auth passthrough).
#   - REAL AUTH (Step 6): register + login through G's front (POST /accounts/register
#     -> 201, POST /accounts/login -> 200) mint a DB-backed session on D; the bearer
#     then authorizes ops on every process (each gateway verifies it against D's
#     accounts.verifySession over QUIC/mTLS). NEGATIVE: a garbage token and a
#     dev-<uuid> token are both 401 through G -- gateway-svc runs WITHOUT
#     ACCOUNTS_DEV_AUTH, so no dev- token is ever accepted.
#   - Async event, cross-process A->B: POST /characters on A -> 201; A appends
#     character.created to the shared durable log; B's pull worker delivers it to
#     inventory's durable on_tx, which grants the starter item. Poll
#     GET /inventory/character/<id> on B until the starter (starter_sword x1) appears.
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
#   - CONFIG live-reload cross-process C->B (Step 7): change inventory/starter_item at
#     runtime via psql; config's write trigger bumps the revision, pg_notifies
#     config_changed, and appends config.changed DURABLY. inventory-svc's invalidation
#     plane (LISTENing config_changed on the shared DB) refreshes CachedConfig; its
#     inventory starter spec reloads on the durable event. A NEWLY created character then
#     gets the NEW starter item -- proving the snapshot-backed remote config reader
#     live-reloads across the process boundary WITHOUT a restart (B booted with the
#     default starter_sword).
#
# THE QUIC PLAYER FRONT (Step 8, all through gateway-svc's :9100 QUIC front via the
# playercli tool -- exit 0 iff transport ok AND payload status=="Ok"):
#   - P1 characters.create over QUIC -> exit 0 (player QUIC -> G -> mTLS edge -> A).
#   - P2 inventory.listCharacter over QUIC -> exit 0 (player QUIC -> G -> Remote ->
#     B's NEW :9001 edge -> owner_of over QUIC -> A): the newest composition.
#   - P3 GET /inventory/character/<id> through G's HTTP :8082 -> 200 (the HTTP front
#     still routes cross-provider inventory.* -> B remote).
#   - P4 no token / bad token on an auth op -> exit 1 + {status:"Unauthorized"}
#     (bearer verified once at the front against the op's AuthReq).
#   - P5 characters.ownerOf (wire-only, absent from the route table) -> exit 1 +
#     {status:"NotFound"} (the method allow-list gate, live -- not a blind relay).
#
# ASCII only (no em-dashes): PowerShell 5.1 chokes on them; keep the sibling .ps1
# and this in lockstep.
#
# Assumes a local Postgres reachable at DATABASE_URL (or the default DSN).
set -uo pipefail
cd "$(dirname "$0")"

# --- Live log tee: every invocation writes its full console output to a timestamped
# log file (in addition to the console), with the log path printed FIRST so a human or
# an agent can tail it live. Process-substitution tee plays fine with the teardown traps
# registered below (INT/TERM/EXIT).
mkdir -p run/logs
LOG="run/logs/split-proof-$(date +%Y%m%d-%H%M%S).log"
echo "[log] $(pwd)/$LOG"
exec > >(tee -a "$LOG") 2>&1

RUN_DIR="run"
BIN_DIR="target/debug"
CA_CERT="$RUN_DIR/edge-ca.crt"
CA_KEY="$RUN_DIR/edge-ca.key"
A_PORT=8080
B_PORT=8081
G_PORT=8082
C_PORT=8083
D_PORT=8084
E_PORT=8085
F_PORT=8086
H_PORT=8087
I_PORT=8088
J_PORT=8089
K_PORT=8090
L_PORT=8091
EDGE_PORT=9000
B_EDGE_PORT=9001
C_EDGE_PORT=9002
D_EDGE_PORT=9003
F_EDGE_PORT=9004
H_EDGE_PORT=9005
I_EDGE_PORT=9006
J_EDGE_PORT=9007
K_EDGE_PORT=9008
L_EDGE_PORT=9009
PLAYER_PORT=9100

# The svc processes this proof boots -- the ONE hand-maintained fleet list (the build
# list and the straggler-kill lists all derive from it). fleet_preflight below pins
# it to the cmd/*-svc directories on disk.
FLEET_SVCS=(
    characters-svc inventory-svc gateway-svc config-svc accounts-svc admin-svc
    audit-svc scheduler-svc match-svc rating-svc leaderboard-svc apikeys-svc
)

DEFAULT_DSN="postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable"
DATABASE_URL="${DATABASE_URL:-$DEFAULT_DSN}"

# Session-auth admin logins for the proof. proofadmin is the happy-path operator;
# prooflock is a dedicated user we deliberately lock out ([AD2]). Both are minted
# pre-boot via adminctl (session auth replaced the old ADMIN_USER/ADMIN_PASS Basic
# gate). Passwords ride stdin into adminctl, never argv.
PROOF_ADMIN_USER="proofadmin"
PROOF_ADMIN_PASS="proofpass"
PROOF_LOCK_USER="prooflock"
PROOF_LOCK_PASS="lockpass"

FAILS=0
A_PID=""
B_PID=""
G_PID=""
C_PID=""
D_PID=""
E_PID=""
F_PID=""
H_PID=""
I_PID=""
J_PID=""
K_PID=""
L_PID=""
M_PID=""

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

# --- psql discovery (local Postgres is the test DB; REQUIRED -- the DB assertions
# below are not optional, so a missing psql fails the whole proof at startup) -----
find_psql() {
    if command -v psql >/dev/null 2>&1; then command -v psql; return; fi
    local p
    for p in /c/Program\ Files/PostgreSQL/*/bin/psql.exe; do
        [ -f "$p" ] && { echo "$p"; return; }
    done
    echo ""
}
PSQL="$(find_psql)"
if [ -z "$PSQL" ]; then
    echo "split-proof: psql not found -- local Postgres is the test DB and the DB assertions are mandatory; install psql or put it on PATH" >&2
    exit 1
fi

# Run one SQL statement against the test DB. Follows DATABASE_URL natively -- psql
# accepts a connection URI directly, so no DSN parsing is needed and percent-encoded
# passwords / sslmode query params ride along for free.
pg() {
    out=$("$PSQL" "$DATABASE_URL" -v ON_ERROR_STOP=1 -t -A -c "$1" 2>&1); rc=$?
    if [ $rc -ne 0 ]; then
        echo "FATAL psql rc=$rc for: $1" >&2
        echo "$out" >&2
        kill -s TERM $$
    fi
    printf '%s\n' "$out"
}

# --- teardown: kill all processes on ANY exit --------------------------------
# Send SIGTERM, then wait (bounded, ~10s) for the process to actually exit so a
# draining service (graceful HTTP/QUIC shutdown) isn't racing the next run's port
# bind. Forced kill is a last resort if it overstays the grace window.
stop_pid() {
    local pid="$1" label="$2" ec=""
    [ -n "$pid" ] || return 0
    kill "$pid" 2>/dev/null || return 0
    note "stopping $label (pid $pid)"
    local i=0
    while [ "$i" -lt 40 ]; do
        if ! kill -0 "$pid" 2>/dev/null; then
            # Gone within the grace period: reap it and check HOW it exited. A clean
            # SIGTERM handler exits 0; a signal-kill (or a crash mid-drain) yields
            # non-zero (128+signal) -- that's a drained-but-not-graceful exit, distinct
            # from the KILL-fallback case below (which STOP_FORCED already covers).
            wait "$pid" 2>/dev/null
            ec=$?
            if [ "$ec" -ne 0 ]; then
                STOP_NONZERO=1
                note "$label (pid $pid) exited non-zero ($ec) after SIGTERM"
            else
                note "stopped $label (pid $pid)"
            fi
            return 0
        fi
        sleep 0.25
        i=$((i + 1))
    done
    note "$label (pid $pid) still alive after grace; forcing"
    STOP_FORCED=1
    kill -9 "$pid" 2>/dev/null || true
}
TEARDOWN_DONE=""
STOP_FORCED=""
STOP_NONZERO=""
teardown() {
    [ -n "$TEARDOWN_DONE" ] && return 0
    TEARDOWN_DONE=1
    stop_pid "$A_PID" "A"
    stop_pid "$B_PID" "B"
    stop_pid "$G_PID" "G"
    stop_pid "$C_PID" "C"
    stop_pid "$D_PID" "D"
    stop_pid "$E_PID" "E"
    stop_pid "$F_PID" "F"
    stop_pid "$H_PID" "H"
    stop_pid "$I_PID" "I"
    stop_pid "$J_PID" "J"
    stop_pid "$K_PID" "K"
    stop_pid "$L_PID" "L"
    stop_pid "$M_PID" "monolith"
    A_PID=""; B_PID=""; G_PID=""; C_PID=""; D_PID=""; E_PID=""; F_PID=""; H_PID=""; I_PID=""; J_PID=""; K_PID=""; L_PID=""; M_PID=""
}
trap 'teardown; exit 1' INT TERM
trap teardown EXIT

# --- clear any stragglers from an aborted prior run (idempotent reruns) ------
kill_stragglers() {
    local n
    # By name (Windows), best-effort.
    if command -v taskkill >/dev/null 2>&1; then
        for n in "${FLEET_SVCS[@]}" server; do
            taskkill //F //IM "$n.exe" >/dev/null 2>&1 || true
        done
    fi
    for n in "${FLEET_SVCS[@]}"; do
        pkill -f "$n" 2>/dev/null || true
    done
    pkill -f "target/debug/server" 2>/dev/null || true
}

wait_healthy() {
    local port="$1" name="$2" tries=60
    while [ "$tries" -gt 0 ]; do
        if curl -fsS -o /dev/null "http://localhost:$port/readyz" 2>/dev/null; then
            note "$name healthy on :$port"; return 0
        fi
        tries=$((tries - 1)); sleep 0.5
    done
    note "$name NEVER became healthy on :$port"
    curl -s "http://localhost:$port/readyz" 2>&1 | while IFS= read -r line; do note "  readyz body: $line"; done
    return 1
}

# --- Fleet-membership tripwire: the boot blocks below are inherently manual
# (ports, env, named assertions), so VERIFY the "I didn't forget a svc" assumption
# instead of trusting memory. Compares FLEET_SVCS against the cmd/*-svc directories
# on disk and dies BEFORE booting anything, naming exactly what drifted and what to
# do about it.
fleet_preflight() {
    local disk=() missing=() stale=() d n
    for d in cmd/*-svc/; do
        [ -d "$d" ] && disk+=("$(basename "$d")")
    done
    for n in "${disk[@]}"; do
        case " ${FLEET_SVCS[*]} " in *" $n "*) ;; *) missing+=("$n") ;; esac
    done
    for n in "${FLEET_SVCS[@]}"; do
        case " ${disk[*]} " in *" $n "*) ;; *) stale+=("$n") ;; esac
    done
    if [ "${#missing[@]}" -gt 0 ] || [ "${#stale[@]}" -gt 0 ]; then
        note "FATAL fleet drift: the svcs this script boots != the cmd/*-svc directories on disk."
        for n in "${missing[@]}"; do
            note "  cmd/$n exists but this script never boots it -- add a port, a boot block, and a named assertion for it (CLAUDE.md 'Adding a module' step 4), then extend FLEET_SVCS"
        done
        for n in "${stale[@]}"; do
            note "  FLEET_SVCS lists '$n' but cmd/$n does not exist -- remove its entry and boot block, or restore the crate"
        done
        exit 1
    fi
    note "fleet preflight OK: ${#FLEET_SVCS[@]} svcs booted here == cmd/*-svc on disk"
}

# ============================================================================
fleet_preflight
BUILD_PKGS=(edgeca "${FLEET_SVCS[@]}" adminctl playercli csharp-client-gen server)
note "building ${BUILD_PKGS[*]} ..."
CARGO_ARGS=()
for p in "${BUILD_PKGS[@]}"; do CARGO_ARGS+=(-p "$p"); done
if ! cargo build "${CARGO_ARGS[@]}"; then
    echo "build failed"; exit 1
fi
PLAYERCLI="$BIN_DIR/playercli$EXE"

mkdir -p "$RUN_DIR"
kill_stragglers
sleep 1

note "minting shared edge dev CA -> $CA_CERT"
"$BIN_DIR/edgeca$EXE" --cert "$CA_CERT" --key "$CA_KEY"

# --- seed the admin logins PRE-BOOT (session auth replaced Basic auth). adminctl
# ensures schema `admin` + admin.users itself and upserts the login (password over
# stdin, never argv), so it runs before admin-svc migrates the rest of the schema.
seed_admin() {
    local user="$1" pass="$2"
    if ! printf '%s\n' "$pass" | DATABASE_URL="$DATABASE_URL" "$BIN_DIR/adminctl$EXE" create-user "$user" --password-stdin >/dev/null; then
        echo "split-proof: failed to seed admin user $user via adminctl" >&2
        exit 1
    fi
    note "seeded admin login $user (adminctl create-user)"
}
seed_admin "$PROOF_ADMIN_USER" "$PROOF_ADMIN_PASS"
seed_admin "$PROOF_LOCK_USER" "$PROOF_LOCK_PASS"

# --- start D (accounts-svc): gateway + accounts, edge :9003 ------------------
# D owns the accounts schema and serves accounts.verifySession + the auth op faces
# on its mTLS edge; EVERY other process's gateway verifies bearers against it.
# player.registered is appended to the shared durable log (audit-svc pulls it).
note "starting D (accounts-svc) on :$D_PORT, edge :$D_EDGE_PORT ..."
# ACCOUNTS_DEV_AUTH=1: dev/password auth is now an explicit opt-in (fail-closed default),
# and D hosts the accounts module, so the register/login the proof drives (via G Remote)
# need it enabled here. G itself never sets it -- [A5] still proves a dev- token is 401.
env PORT=":$D_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$D_EDGE_PORT" \
    EDGE_CA_CERT="$CA_CERT" EDGE_CA_KEY="$CA_KEY" \
    ACCOUNTS_DEV_AUTH=1 \
    EPIC_CLIENT_ID=test EPIC_CLIENT_SECRET=test EPIC_TOKEN_URL=http://127.0.0.1:1/token \
    "$BIN_DIR/accounts-svc$EXE" >"$RUN_DIR/accounts.out.log" 2>"$RUN_DIR/accounts.err.log" &
D_PID=$!
wait_healthy "$D_PORT" "D (accounts-svc)" || { echo "D failed to start"; exit 1; }

# --- start L (apikeys-svc): apikeys, edge :9009 ------------------------------
# L owns the apikeys schema (plaintext key -> policy) and serves apikeys.keys on its
# mTLS edge; gateway-svc (G) and admin-svc (E) resolve/dial it via APIKEYS_EDGE_ADDR.
# APIKEYS_DEV_SEED=1 self-heals the two well-known dev keys (dev-key-client,
# dev-key-server) on every boot so the K1-K4 assertions below have a stable fixture.
note "starting L (apikeys-svc) on :$L_PORT, edge :$L_EDGE_PORT ..."
env PORT=":$L_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$L_EDGE_PORT" \
    EDGE_CA_CERT="$CA_CERT" EDGE_CA_KEY="$CA_KEY" \
    APIKEYS_DEV_SEED=1 \
    "$BIN_DIR/apikeys-svc$EXE" >"$RUN_DIR/apikeys.out.log" 2>"$RUN_DIR/apikeys.err.log" &
L_PID=$!
wait_healthy "$L_PORT" "L (apikeys-svc)" || { echo "L failed to start"; exit 1; }

# --- start F (audit-svc): audit, edge :9004 ----------------------------------
# F owns the audit schema (append-only ledger). It PRODUCES nothing: its pull workers
# drain its six subscriptions from the shared log, and audit's on_tx_raw records each
# on the handed delivery tx (exactly-once with the cursor advance). It serves
# admin.adminData on its mTLS edge so admin-svc (E) fans its "Audit Log" page out
# over QUIC.
note "starting F (audit-svc) on :$F_PORT, edge :$F_EDGE_PORT ..."
env PORT=":$F_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$F_EDGE_PORT" \
    EDGE_CA_CERT="$CA_CERT" EDGE_CA_KEY="$CA_KEY" \
    "$BIN_DIR/audit-svc$EXE" >"$RUN_DIR/audit.out.log" 2>"$RUN_DIR/audit.err.log" &
F_PID=$!
wait_healthy "$F_PORT" "F (audit-svc)" || { echo "F failed to start"; exit 1; }

# --- start H (scheduler-svc): scheduler, edge :9005 --------------------------
# H owns the scheduler schema (a catalogue of named schedules) and is a DURABLE
# PRODUCER: its 1s loop fires scheduler.fired for every due schedule (race-safe via a
# per-schedule pg_try_advisory_lock), appending scheduler.fired to the shared log,
# where audit-svc's (F) prune subscription pulls it. It serves admin.adminData
# ("Schedules") on its mTLS edge so admin-svc (E) fans it out over QUIC.
note "starting H (scheduler-svc) on :$H_PORT, edge :$H_EDGE_PORT ..."
env PORT=":$H_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$H_EDGE_PORT" \
    EDGE_CA_CERT="$CA_CERT" EDGE_CA_KEY="$CA_KEY" \
    "$BIN_DIR/scheduler-svc$EXE" >"$RUN_DIR/scheduler.out.log" 2>"$RUN_DIR/scheduler.err.log" &
H_PID=$!
wait_healthy "$H_PORT" "H (scheduler-svc)" || { echo "H failed to start"; exit 1; }

# --- start J (rating-svc): rating, edge :9007 --------------------------------
# J provides `rating.mmr` on its mTLS edge (match-svc reads it sync before recording a
# result) and pulls match.finished (+15/-15) from the shared log. In-memory MMR
# (no schema) but it hosts a durable subscription, so it needs a DB pool (the
# durable-events plane is app-owned, not a module dependency).
note "starting J (rating-svc) on :$J_PORT, edge :$J_EDGE_PORT ..."
env PORT=":$J_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$J_EDGE_PORT" \
    EDGE_CA_CERT="$CA_CERT" EDGE_CA_KEY="$CA_KEY" \
    "$BIN_DIR/rating-svc$EXE" >"$RUN_DIR/rating.out.log" 2>"$RUN_DIR/rating.err.log" &
J_PID=$!
wait_healthy "$J_PORT" "J (rating-svc)" || { echo "J failed to start"; exit 1; }

# --- start K (leaderboard-svc): gateway + leaderboard, edge :9008 ------------
# K owns schema `leaderboard`, pulls match.finished (upsert wins+1) from the shared
# log, and serves GET /leaderboard (gateway-svc routes it Remote here).
note "starting K (leaderboard-svc) on :$K_PORT, edge :$K_EDGE_PORT ..."
env PORT=":$K_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$K_EDGE_PORT" \
    EDGE_CA_CERT="$CA_CERT" EDGE_CA_KEY="$CA_KEY" \
    "$BIN_DIR/leaderboard-svc$EXE" >"$RUN_DIR/leaderboard.out.log" 2>"$RUN_DIR/leaderboard.err.log" &
K_PID=$!
wait_healthy "$K_PORT" "K (leaderboard-svc)" || { echo "K failed to start"; exit 1; }

# --- start I (match-svc): gateway + match + rating stub, edge :9006
# I records matches (schema `match`) and is a DURABLE PRODUCER: `report` SYNC-reads both
# players' MMR from rating-svc (J) over the mTLS edge, INSERTs the match row + emit_tx's
# match.finished IN ONE TX onto the shared log; rating-svc (J), leaderboard-svc (K)
# and audit-svc (F) pull it.
note "starting I (match-svc) on :$I_PORT, edge :$I_EDGE_PORT ..."
env PORT=":$I_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$I_EDGE_PORT" \
    EDGE_CA_CERT="$CA_CERT" EDGE_CA_KEY="$CA_KEY" \
    RATING_EDGE_ADDR="127.0.0.1:$J_EDGE_PORT" \
    "$BIN_DIR/match-svc$EXE" >"$RUN_DIR/match.out.log" 2>"$RUN_DIR/match.err.log" &
I_PID=$!
wait_healthy "$I_PORT" "I (match-svc)" || { echo "I failed to start"; exit 1; }

# --- start A (characters-svc): gateway + characters, edge :9000 --------------
# A appends character.created/.deleted to the shared log; inventory-svc (B) and
# audit-svc (F) pull them.
note "starting A (characters-svc) on :$A_PORT, edge :$EDGE_PORT ..."
env PORT=":$A_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$EDGE_PORT" \
    EDGE_CA_CERT="$CA_CERT" EDGE_CA_KEY="$CA_KEY" \
    "$BIN_DIR/characters-svc$EXE" >"$RUN_DIR/characters.out.log" 2>"$RUN_DIR/characters.err.log" &
A_PID=$!
wait_healthy "$A_PORT" "A (characters-svc)" || { echo "A failed to start"; exit 1; }

# --- reset the config baseline: B must boot with the DEFAULT starter (starter_sword),
# so the later runtime change to health_potion is provably a LIVE reload, not a boot
# value. DELETE fires no NOTIFY, but C/B are not up yet, so their boot loads see no row.
pg "DELETE FROM config.settings WHERE namespace='inventory' AND key='starter_item'; DELETE FROM config.settings WHERE namespace='proof';" >/dev/null
note "reset config baseline (deleted inventory/starter_item + stale proof/* rows)"

# --- start C (config-svc): gateway + config, edge :9002 ----------------------
# C owns the config schema + write trigger and serves config.snapshot on its mTLS edge;
# the trigger bumps the revision, pg_notifies config_changed, and appends config.changed
# durably onto the shared log (B and F pull the event; B also LISTENs config_changed).
note "starting C (config-svc) on :$C_PORT, edge :$C_EDGE_PORT ..."
env PORT=":$C_PORT" DATABASE_URL="$DATABASE_URL" EDGE_ADDR=":$C_EDGE_PORT" \
    EDGE_CA_CERT="$CA_CERT" EDGE_CA_KEY="$CA_KEY" \
    "$BIN_DIR/config-svc$EXE" >"$RUN_DIR/config.out.log" 2>"$RUN_DIR/config.err.log" &
C_PID=$!
wait_healthy "$C_PORT" "C (config-svc)" || { echo "C failed to start"; exit 1; }

# --- start B (inventory-svc): gateway + inventory + characters/config stubs
# B now ALSO serves its OWN mTLS edge (EDGE_ADDR=:9001) so gateway-svc can dispatch
# inventory.* Remote to it; it dials A over CHARACTERS_EDGE_ADDR for owner_of and
# config-svc over CONFIG_EDGE_ADDR for the CachedConfig boot-fill + snapshot refresh.
note "starting B (inventory-svc) on :$B_PORT, edge :$B_EDGE_PORT ..."
# INVENTORY_DEV_GRANT=1: the simulated-IAP grant route is now an explicit opt-in
# (fail-closed default); B hosts the inventory module, so set it here for parity with
# the monolith stage (the starter-grant flow is durable-event driven and needs no flag).
env PORT=":$B_PORT" DATABASE_URL="$DATABASE_URL" \
    EDGE_ADDR=":$B_EDGE_PORT" \
    EDGE_CA_CERT="$CA_CERT" EDGE_CA_KEY="$CA_KEY" \
    CHARACTERS_EDGE_ADDR="127.0.0.1:$EDGE_PORT" \
    CONFIG_EDGE_ADDR="127.0.0.1:$C_EDGE_PORT" \
    INVENTORY_DEV_GRANT=1 \
    "$BIN_DIR/inventory-svc$EXE" >"$RUN_DIR/inventory.out.log" 2>"$RUN_DIR/inventory.err.log" &
B_PID=$!
wait_healthy "$B_PORT" "B (inventory-svc)" || { echo "B failed to start"; exit 1; }

# --- start G (gateway-svc): the dedicated front door -- HTTP :8082 + player QUIC --
# :9100. No DB (without_db), no provider modules: only remote::Stubs, so EVERY op it
# fronts resolves Remote and is dialed over the mTLS edge to A (:9000) / B (:9001). It
# needs the shared CA to dial peers AND to derive the player-front server cert.
# G ALSO fronts the browser flows via HTTP passthrough (Step 7): /admin -> admin-svc
# (E, :8085) and /accounts/epic -> accounts-svc (D). Typed /accounts ops
# (register/login/me) still route Remote as before; only the non-op /admin +
# /accounts/epic prefixes hit the reverse proxy.
note "starting G (gateway-svc) on :$G_PORT, player QUIC :$PLAYER_PORT ..."
env PORT=":$G_PORT" \
    PLAYER_EDGE_ADDR=":$PLAYER_PORT" \
    EDGE_CA_CERT="$CA_CERT" EDGE_CA_KEY="$CA_KEY" \
    CHARACTERS_EDGE_ADDR="127.0.0.1:$EDGE_PORT" \
    INVENTORY_EDGE_ADDR="127.0.0.1:$B_EDGE_PORT" \
    ACCOUNTS_EDGE_ADDR="127.0.0.1:$D_EDGE_PORT" \
    MATCH_EDGE_ADDR="127.0.0.1:$I_EDGE_PORT" \
    LEADERBOARD_EDGE_ADDR="127.0.0.1:$K_EDGE_PORT" \
    APIKEYS_EDGE_ADDR="127.0.0.1:$L_EDGE_PORT" \
    ADMIN_HTTP_ADDR="127.0.0.1:$E_PORT" \
    ACCOUNTS_HTTP_ADDR="127.0.0.1:$D_PORT" \
    "$BIN_DIR/gateway-svc$EXE" >"$RUN_DIR/gateway.out.log" 2>"$RUN_DIR/gateway.err.log" &
G_PID=$!
wait_healthy "$G_PORT" "G (gateway-svc)" || { echo "G failed to start"; exit 1; }

# --- start E (admin-svc): the admin portal fortress -- HTTP :8085, its OWN DB (schema
# admin: users/sessions/login_attempts), no edge server. It DIALS all six peer edges
# (A/B/C/D + audit + scheduler) to fan out their admin pages over QUIC. Session auth now
# gates the portal (no ADMIN_USER/ADMIN_PASS): TRUSTED_PROXY_CIDRS=127.0.0.1/32 makes the
# lockout ip:<addr> subject the real client behind G's passthrough, and
# ADMIN_COOKIE_SECURE=0 lets curl carry the session cookie over plain http.
note "starting E (admin-svc) on :$E_PORT ..."
env PORT=":$E_PORT" DATABASE_URL="$DATABASE_URL" \
    EDGE_CA_CERT="$CA_CERT" EDGE_CA_KEY="$CA_KEY" \
    CHARACTERS_EDGE_ADDR="127.0.0.1:$EDGE_PORT" \
    INVENTORY_EDGE_ADDR="127.0.0.1:$B_EDGE_PORT" \
    CONFIG_EDGE_ADDR="127.0.0.1:$C_EDGE_PORT" \
    ACCOUNTS_EDGE_ADDR="127.0.0.1:$D_EDGE_PORT" \
    AUDIT_EDGE_ADDR="127.0.0.1:$F_EDGE_PORT" \
    SCHEDULER_EDGE_ADDR="127.0.0.1:$H_EDGE_PORT" \
    APIKEYS_EDGE_ADDR="127.0.0.1:$L_EDGE_PORT" \
    TRUSTED_PROXY_CIDRS="127.0.0.1/32" ADMIN_COOKIE_SECURE=0 \
    "$BIN_DIR/admin-svc$EXE" >"$RUN_DIR/admin.out.log" 2>"$RUN_DIR/admin.err.log" &
E_PID=$!
wait_healthy "$E_PORT" "E (admin-svc)" || { echo "E failed to start"; exit 1; }

# --- [GW-RDY] the DB-less front's /readyz reflects its peers, not a bare 200 --------
# gateway-svc holds a Stub per consumed provider; each Stub contributes a
# `stub:<provider>` httpmw::ReadyCheck that dials the peer's edge. With the WHOLE fleet
# up, G's /readyz must be 200 "ok" (a bare-200 DB-less front used to answer ready even
# with the backend dead — the readiness probe closes that gap).
echo ""
echo "[GW-RDY] GET http://localhost:$G_PORT/readyz through G -> expect 200 ok (peers probed)"
RDY="$(curl -s -w $'\n%{http_code}' "http://localhost:$G_PORT/readyz")"
RDY_BODY="$(echo "$RDY" | sed '$d')"; RDY_CODE="$(echo "$RDY" | tail -1)"
echo "    -> HTTP $RDY_CODE  $RDY_BODY"
if [ "$RDY_CODE" = "200" ] && echo "$RDY_BODY" | grep -q "ok"; then
    pass "gateway-svc /readyz -> 200 ok with the full fleet up (per-stub probes pass)"
else
    fail "gateway-svc /readyz expected 200 ok, got $RDY_CODE ($RDY_BODY)"; exit 1
fi

RUN_SUFFIX="$(new_uuid | cut -c1-8)"

echo ""
echo "================ REAL AUTH (Step 6) ================"
# Register + login THROUGH the gateway front (G routes /accounts/* Remote to D over
# the mTLS edge), then use the REAL bearer everywhere below. No dev- tokens.

echo "[A1] POST http://localhost:$G_PORT/accounts/register (through G -> D)"
REG="$(curl -s -w $'\n%{http_code}' -X POST "http://localhost:$G_PORT/accounts/register" \
    -H "X-Api-Key: dev-key-client" \
    -H "Content-Type: application/json" \
    -d "{\"email\":\"proof-$RUN_SUFFIX@test.local\",\"password\":\"pw-$RUN_SUFFIX\",\"displayName\":\"Proof\"}")"
RBODY="$(echo "$REG" | sed '$d')"; RCODE="$(echo "$REG" | tail -1)"
echo "    -> HTTP $RCODE  $RBODY"
PID="$(echo "$RBODY" | grep -o '"player_id":"[^"]*"' | head -1 | sed 's/"player_id":"//;s/"//')"
if [ "$RCODE" = "201" ] && [ -n "$PID" ]; then
    pass "register through the front -> 201, player_id=$PID"
else
    fail "register expected 201 with player_id, got $RCODE"; exit 1
fi

echo "[A2] POST http://localhost:$G_PORT/accounts/login (fresh session through G -> D)"
LOGIN="$(curl -s -w $'\n%{http_code}' -X POST "http://localhost:$G_PORT/accounts/login" \
    -H "X-Api-Key: dev-key-client" \
    -H "Content-Type: application/json" \
    -d "{\"email\":\"proof-$RUN_SUFFIX@test.local\",\"password\":\"pw-$RUN_SUFFIX\"}")"
LBODY="$(echo "$LOGIN" | sed '$d')"; LCODE="$(echo "$LOGIN" | tail -1)"
TOKEN="$(echo "$LBODY" | grep -o '"token":"[^"]*"' | head -1 | sed 's/"token":"//;s/"//')"
echo "    -> HTTP $LCODE  token=$(echo "$TOKEN" | cut -c1-12)..."
if [ "$LCODE" = "200" ] && [ -n "$TOKEN" ]; then
    pass "login through the front -> 200 with a real bearer"
else
    fail "login expected 200 with token, got $LCODE"; exit 1
fi

echo "[A3] GET http://localhost:$G_PORT/accounts/me (Bearer <real token>)"
ME="$(curl -s -w $'\n%{http_code}' "http://localhost:$G_PORT/accounts/me" \
    -H "X-Api-Key: dev-key-client" \
    -H "Authorization: Bearer $TOKEN")"
MEBODY="$(echo "$ME" | sed '$d')"; MECODE="$(echo "$ME" | tail -1)"
echo "    -> HTTP $MECODE  $MEBODY"
if [ "$MECODE" = "200" ] && echo "$MEBODY" | grep -q "$PID"; then
    pass "me -> 200 with the registered player (auth-once verified the real session)"
else
    fail "me expected 200 with player_id, got $MECODE"
fi

# A second player for the negative authz assertion.
OREG="$(curl -s -X POST "http://localhost:$G_PORT/accounts/register" \
    -H "X-Api-Key: dev-key-client" \
    -H "Content-Type: application/json" \
    -d "{\"email\":\"other-$RUN_SUFFIX@test.local\",\"password\":\"pw2-$RUN_SUFFIX\",\"displayName\":\"Other\"}")"
OTHER_TOKEN="$(echo "$OREG" | grep -o '"token":"[^"]*"' | head -1 | sed 's/"token":"//;s/"//')"
[ -n "$OTHER_TOKEN" ] || { fail "second register produced no token"; exit 1; }

echo "[A4] GET /characters through G with a GARBAGE token -> 401"
G1="$(curl -s -o /dev/null -w '%{http_code}' "http://localhost:$G_PORT/characters" \
    -H "X-Api-Key: dev-key-client" \
    -H "Authorization: Bearer totally-bogus-token")"
echo "    -> HTTP $G1"
if [ "$G1" = "401" ]; then pass "garbage token -> 401"; else fail "garbage token expected 401, got $G1"; fi

echo "[A5] GET /characters through G with a dev-<uuid> token -> 401 (no ACCOUNTS_DEV_AUTH on G)"
G2="$(curl -s -o /dev/null -w '%{http_code}' "http://localhost:$G_PORT/characters" \
    -H "X-Api-Key: dev-key-client" \
    -H "Authorization: Bearer dev-$(new_uuid)")"
echo "    -> HTTP $G2"
if [ "$G2" = "401" ]; then
    pass "dev- token -> 401 (gateway-svc verifies REAL sessions only)"
else
    fail "dev- token expected 401, got $G2"
fi

echo ""
echo "================ EPIC OAUTH REDIRECT (browser flow: G passthrough -> D) ================"
# G reverse-proxies /accounts/epic/* to accounts-svc (D). D's callback exchanges the
# authorization code with EPIC_TOKEN_URL, which we pointed at an unreachable port
# (127.0.0.1:1) so the exchange fails deterministically and D answers
# `303 See Other -> /?epic=error` (axum Redirect::to). The proof: the gateway proxy RELAYS that 303 verbatim
# (reqwest Policy::none()) instead of following it server-side -- a server-side follow
# would swallow the redirect, and in the real Epic login the browser would never see
# the `#token` fragment. curl is run WITHOUT -L so we observe the raw 303 + Location.
echo "[EP1] POST /accounts/epic/start through G (passthrough, keyless) -> {authorize_url}"
ESTART="$(curl -s -X POST "http://localhost:$G_PORT/accounts/epic/start")"
echo "    -> $ESTART"
ESTATE="$(echo "$ESTART" | grep -o 'state=[^&"]*' | head -1 | sed 's/state=//')"
if [ -n "$ESTATE" ]; then
    pass "epic start relayed through G -> authorize_url with state=$(echo "$ESTATE" | cut -c1-8)..."
else
    fail "epic start expected authorize_url with a state param, got $ESTART"; exit 1
fi

echo "[EP2] GET /accounts/epic/callback?code=x&state=<state> through G (no -L) -> 303 relayed verbatim"
EHDRS="$(curl -s -o /dev/null -D - -w 'HTTP:%{http_code}\n' \
    "http://localhost:$G_PORT/accounts/epic/callback?code=x&state=$ESTATE")"
ECODE="$(echo "$EHDRS" | grep -o 'HTTP:[0-9]*' | sed 's/HTTP://')"
ELOC="$(echo "$EHDRS" | grep -i '^location:' | head -1 | sed 's/^[Ll]ocation:[[:space:]]*//' | tr -d '\r')"
echo "    -> HTTP $ECODE  Location=$ELOC"
if [ "$ECODE" = "303" ] && [ "$ELOC" = "/?epic=error" ]; then
    pass "epic-oauth-redirect-through-gateway: G relays D's 303 verbatim (Location: $ELOC) -- proxy does not follow"
else
    fail "epic callback expected 303 with Location /?epic=error, got HTTP $ECODE Location=$ELOC"; exit 1
fi

echo ""
echo "================ API KEY POLICY (apikeys-svc :$L_PORT via G) ================"
# K1-K4: the policy check runs BEFORE session auth on both planes (Decision 5 of the
# api-key-policy plan), so these assertions use an AuthNone op (GET /leaderboard,
# POST /match/report) to isolate the key check from bearer auth.

echo "[K1] GET /leaderboard through G with NO X-Api-Key -> 401 (missing api key)"
K1="$(curl -s -o /dev/null -w '%{http_code}' "http://localhost:$G_PORT/leaderboard")"
echo "    -> HTTP $K1"
if [ "$K1" = "401" ]; then pass "no api key -> 401 (missing key)"; else fail "no api key expected 401, got $K1"; fi

echo "[K2] GET /leaderboard through G with a BOGUS X-Api-Key -> 401 (invalid api key)"
K2="$(curl -s -o /dev/null -w '%{http_code}' "http://localhost:$G_PORT/leaderboard" \
    -H "X-Api-Key: totally-bogus-key")"
echo "    -> HTTP $K2"
if [ "$K2" = "401" ]; then pass "bogus api key -> 401 (invalid key)"; else fail "bogus api key expected 401, got $K2"; fi

echo "[K3] POST /match/report through G with dev-key-client (player-facing policy, NO match.report) -> 403"
K3="$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://localhost:$G_PORT/match/report" \
    -H "X-Api-Key: dev-key-client" -H "Content-Type: application/json" \
    -d "{\"ReportId\":\"k3-$RUN_SUFFIX\",\"Winner\":\"k3-winner\",\"Loser\":\"k3-loser\"}")"
echo "    -> HTTP $K3"
if [ "$K3" = "403" ]; then
    pass "dev-key-client on match.report -> 403 (policy forbids this operation)"
else
    fail "dev-key-client on match.report expected 403, got $K3"
fi

echo "[K4] POST /match/report through G with dev-key-server (full policy) -> 202"
K4="$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://localhost:$G_PORT/match/report" \
    -H "X-Api-Key: dev-key-server" -H "Content-Type: application/json" \
    -d "{\"ReportId\":\"k4-$RUN_SUFFIX\",\"Winner\":\"k4-winner\",\"Loser\":\"k4-loser\"}")"
echo "    -> HTTP $K4"
if [ "$K4" = "202" ]; then
    pass "dev-key-server (full) on match.report -> 202 (op's real success code)"
else
    fail "dev-key-server on match.report expected 202, got $K4"
fi
pg "DELETE FROM leaderboard.scores WHERE player IN ('k3-winner','k3-loser','k4-winner','k4-loser');" >/dev/null

# [K5] key-verifier load-shed is 503, never a mislabeled 401 (and never a crash).
# The verifier bounds concurrent uncached lookups with a global 64-permit semaphore
# (+ a flight-lock table); when exhausted it sheds with 503 Unavailable ("no verdict
# was reached"), NOT 401 ("your key is invalid"). A DEFINITE 503 is not reliably
# reproducible through the front door from this script: gateway-svc's always-on
# rate limiter (burst 40, asserted in [RL1]) admits fewer concurrent requests from
# one client IP than the semaphore has permits (64), so full saturation cannot be
# forced here. We assert the fix's GUARANTEED weaker observable instead: a parallel
# burst of DISTINCT uncached keys (each takes a real permit-guarded lookup over the
# edge to apikeys-svc) yields ONLY 401 (invalid key), 503 (shed) or 429 (rate
# limiter, orthogonal) -- never a 200 (bogus key admitted) and never another 5xx
# (crash). Any 503s observed are reported as best-effort shed evidence, not asserted.
sleep 2 # let the [RL] token bucket refill so the burst isn't eaten by 429s
echo "[K5] 30 PARALLEL GET /leaderboard with DISTINCT bogus keys -> every response 401/503/429, >=1 401, nothing else"
k5_args=()
for i in $(seq 1 30); do
    [ "${#k5_args[@]}" -gt 0 ] && k5_args+=(--next)
    k5_args+=(-s -o /dev/null -w '%{http_code}\n' -H "X-Api-Key: k5-$RUN_SUFFIX-$i" "http://localhost:$G_PORT/leaderboard")
done
K5_CODES="$(curl -Z --parallel-max 30 "${k5_args[@]}")"
K5_TOTAL="$(echo "$K5_CODES" | grep -c '[0-9]')"
K5_401="$(echo "$K5_CODES" | grep -c '401')"
K5_503="$(echo "$K5_CODES" | grep -c '503')"
K5_429="$(echo "$K5_CODES" | grep -c '429')"
K5_OTHER=$((K5_TOTAL - K5_401 - K5_503 - K5_429))
echo "    -> $K5_TOTAL responses: ${K5_401}x401, ${K5_503}x503 (shed, best-effort), ${K5_429}x429, ${K5_OTHER} other"
if [ "$K5_TOTAL" = "30" ] && [ "$K5_OTHER" = "0" ] && [ "$K5_401" -ge 1 ]; then
    pass "distinct-key burst: all 30 responses 401/503/429 -- no bogus key admitted, no 5xx crash ($K5_503 load-shed 503s observed)"
else
    fail "distinct-key burst expected 30 responses all 401/503/429 with >=1 401, got total=$K5_TOTAL 401=$K5_401 503=$K5_503 429=$K5_429 other=$K5_OTHER"
fi

# [K5b] the burst RELEASED its permits/flight locks (shed is transient, never sticky):
# one more fresh distinct key must reach a definitive verdict (401 invalid), not 503.
echo "[K5b] fresh distinct bogus key after the burst -> 401 (verifier recovered, permits released)"
K5B="$(curl -s -o /dev/null -w '%{http_code}' "http://localhost:$G_PORT/leaderboard" -H "X-Api-Key: k5b-$RUN_SUFFIX")"
echo "    -> HTTP $K5B"
if [ "$K5B" = "401" ]; then
    pass "post-burst distinct key -> 401 (semaphore permits released after the burst)"
else
    fail "post-burst distinct key expected 401, got $K5B"
fi

echo ""
echo "================ SPLIT PROOF ================"

# --- 1. CREATE through G (front-door HTTP op -> Remote -> characters-svc) -----
# characters-svc no longer hosts a FrontDoor, so the create op is fronted by gateway-svc
# (:8082) which dispatches characters.create Remote over the mTLS edge to A.
echo "[1] POST http://localhost:$G_PORT/characters (through G -> A, Bearer \$TOKEN)"
CREATE="$(curl -s -w $'\n%{http_code}' -X POST "http://localhost:$G_PORT/characters" \
    -H "X-Api-Key: dev-key-client" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    -d '{"name":"Aria","class":"mage"}')"
CBODY="$(echo "$CREATE" | sed '$d')"; CCODE="$(echo "$CREATE" | tail -1)"
echo "    -> HTTP $CCODE  $CBODY"
CID="$(echo "$CBODY" | grep -o '"id":"[^"]*"' | head -1 | sed 's/"id":"//;s/"//')"
if [ "$CCODE" = "201" ] && [ -n "$CID" ]; then pass "create -> 201, id=$CID"; else fail "create expected 201 with id"; fi

# --- 2. ASYNC event A->B + SYNC authz B->A over QUIC -------------------------
echo "[2] poll GET http://localhost:$G_PORT/inventory/character/$CID until starter appears (through G -> B)"
STARTER_OK=0
for i in $(seq 1 30); do
    R="$(curl -s -w $'\n%{http_code}' "http://localhost:$G_PORT/inventory/character/$CID" \
        -H "X-Api-Key: dev-key-client" \
        -H "Authorization: Bearer $TOKEN")"
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
echo "[3] GET /inventory/character/$CID through G as a DIFFERENT player (Bearer \$OTHER_TOKEN)"
NEG="$(curl -s -w $'\n%{http_code}' "http://localhost:$G_PORT/inventory/character/$CID" \
    -H "X-Api-Key: dev-key-client" \
    -H "Authorization: Bearer $OTHER_TOKEN")"
NBODY="$(echo "$NEG" | sed '$d')"; NCODE="$(echo "$NEG" | tail -1)"
echo "    -> HTTP $NCODE  $NBODY"
if [ "$NCODE" = "403" ] || [ "$NCODE" = "404" ]; then
    pass "other player -> $NCODE (owner_of over QUIC gates: not their character)"
else
    fail "negative authz expected 403/404, got $NCODE"
fi

# --- 4. DELETE on A ----------------------------------------------------------
echo "[4] DELETE http://localhost:$G_PORT/characters/$CID (through G -> A, Bearer \$TOKEN)"
DEL="$(curl -s -w $'\n%{http_code}' -X DELETE "http://localhost:$G_PORT/characters/$CID" \
    -H "X-Api-Key: dev-key-client" \
    -H "Authorization: Bearer $TOKEN")"
DCODE="$(echo "$DEL" | tail -1)"
echo "    -> HTTP $DCODE"
if [ "$DCODE" = "204" ]; then pass "delete -> 204"; else fail "delete expected 204, got $DCODE"; fi

# --- 5. INTEGRITY via event, not FK: holdings wiped in B --------------------
# The definitive assertion is the DB row count (the on_tx wipe handler ran). The HTTP
# 404 after delete alone only proves the character is gone via owner_of and would mask
# an un-wiped holdings row, so we assert the DB directly (local Postgres is the test DB).
echo "[5] poll B until the character's holdings are WIPED (character.deleted A->B)"
WIPED=0
for i in $(seq 1 30); do
    N="$(pg "SELECT count(*) FROM inventory.holdings WHERE owner_type='character' AND owner_id='$CID';" | tr -d '[:space:]')"
    echo "    attempt $i -> inventory.holdings rows for $CID = ${N:-?}"
    if [ "$N" = "0" ]; then pass "holdings row wiped in B (integrity via character.deleted event, no FK cascade)"; WIPED=1; break; fi
    sleep 0.5
done
[ "$WIPED" = "1" ] || fail "holdings never wiped in B (wipe on_tx handler did not run)"

# [5t] the wipe handler also plants the tombstone (inventory.wiped_characters) in the
# SAME delivery tx — the guard that keeps a reordered/late character.created grant
# from resurrecting holdings for this dead character.
T="$(pg "SELECT count(*) FROM inventory.wiped_characters WHERE character_id='$CID';" | tr -d '[:space:]')"
echo "[5t] inventory.wiped_characters rows for $CID = ${T:-?}"
if [ "$T" = "1" ]; then
    pass "wipe planted the tombstone (late character.created can no longer resurrect holdings)"
else
    fail "expected 1 tombstone row in inventory.wiped_characters for $CID, got ${T:-?}"
fi

# [5b] the same character is gone via owner_of over QUIC too (a second, independent
# signal alongside the DB wipe check above).
echo "[5b] post-delete GET /inventory/character/$CID through G (Bearer \$TOKEN) -> 404"
W2="$(curl -s -w $'\n%{http_code}' "http://localhost:$G_PORT/inventory/character/$CID" -H "X-Api-Key: dev-key-client" -H "Authorization: Bearer $TOKEN")"
W2CODE="$(echo "$W2" | tail -1)"
echo "    -> HTTP $W2CODE  $(echo "$W2" | sed '$d')"
if [ "$W2CODE" = "404" ]; then
    pass "post-delete GET -> 404 (character gone via owner_of over QUIC)"
else
    fail "post-delete GET expected 404, got $W2CODE"
fi

echo ""
echo "========= CONFIG LIVE-RELOAD (config-svc :$C_PORT -> inventory-svc) ========="
# Prove the Step-5 snapshot-backed remote config reader live-reloads across processes:
# change inventory/starter_item at RUNTIME on C's DB, and a NEWLY created character must
# be granted the NEW starter in B -- config.changed flowed C's append -> the shared
# log -> B's pull worker -> B's CachedConfig (snapshot refresh) + inventory starter
# spec, with no restart.
# [C1] baseline: B booted with the default starter (no config row) -> starter_sword.
echo "[C1] baseline: create a character through G -> starter should be the DEFAULT starter_sword"
BCID="$(curl -s -X POST "http://localhost:$G_PORT/characters" \
    -H "X-Api-Key: dev-key-client" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    -d '{"name":"Baseline","class":"mage"}' | grep -o '"id":"[^"]*"' | head -1 | sed 's/"id":"//;s/"//')"
BASE_OK=0
for i in $(seq 1 30); do
    R="$(curl -s "http://localhost:$G_PORT/inventory/character/$BCID" -H "X-Api-Key: dev-key-client" -H "Authorization: Bearer $TOKEN")"
    if echo "$R" | grep -q 'starter_sword'; then BASE_OK=1; break; fi
    if echo "$R" | grep -q 'health_potion'; then break; fi
    sleep 0.5
done
if [ "$BASE_OK" = "1" ]; then
    pass "baseline character granted starter_sword (B booted on the default via CachedConfig)"
else
    fail "baseline starter_sword not granted (BCID=$BCID) -- $R"
fi

# [C2] runtime change on C's DB: the write trigger bumps the revision, pg_notifies
# config_changed (B's invalidation plane refreshes CachedConfig), and appends
# config.changed durably (B's pull worker delivers it -> inventory reloads its spec).
echo "[C2] set config inventory/starter_item=health_potion (via psql on C's shared DB)"
pg "INSERT INTO config.settings (namespace,key,value) VALUES ('inventory','starter_item','health_potion') ON CONFLICT (namespace,key) DO UPDATE SET value=excluded.value;" >/dev/null

# [C3] a NEWLY created character must now be granted the NEW starter. The spec is
# materialized at grant time, so retry with fresh characters until the live-reloaded
# value takes effect (or time out).
echo "[C3] create fresh characters until one is granted health_potion (live reload C->B)"
RELOAD_OK=0
for i in $(seq 1 30); do
    NCID="$(curl -s -X POST "http://localhost:$G_PORT/characters" \
        -H "X-Api-Key: dev-key-client" \
        -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d '{"name":"Reloaded","class":"mage"}' | grep -o '"id":"[^"]*"' | head -1 | sed 's/"id":"//;s/"//')"
    for j in $(seq 1 10); do
        R="$(curl -s "http://localhost:$G_PORT/inventory/character/$NCID" -H "X-Api-Key: dev-key-client" -H "Authorization: Bearer $TOKEN")"
        echo "$R" | grep -qE 'starter_sword|health_potion' && break
        sleep 0.3
    done
    if echo "$R" | grep -q 'health_potion'; then
        echo "    attempt $i -> char $NCID granted health_potion"
        RELOAD_OK=1; break
    fi
    sleep 0.5
done
if [ "$RELOAD_OK" = "1" ]; then
    pass "new character granted health_potion (config.changed C->B live-reloaded CachedConfig + starter spec)"
else
    fail "starter never changed to health_potion cross-process (config live-reload failed) -- $R"
fi

# [C4] a >8KB config value must COMMIT (the pg_notify large-value fix). pg_notify
# hard-caps its payload at 8000 bytes; before the fix the write trigger put the full
# value into the NOTIFY payload, so ANY large config value ABORTED the writing tx.
# The trigger now NOTIFYs value-less (revision only) while the durable config.changed
# event still carries the full value. Assert: the INSERT commits (pg() dies on a
# psql error, so reaching the checks below IS the no-abort proof), the revision
# bumped by exactly one, the 9000-char value round-trips at full length, and the
# durable event appended in the same tx carries the full value.
C4_KEY="c4-large-$RUN_SUFFIX"
C4_REV_BEFORE="$(pg "SELECT revision FROM config.revision;" | tr -d '[:space:]')"
echo "[C4] write a >8KB config value (proof/$C4_KEY, 9000 chars) -- must NOT abort (revision $C4_REV_BEFORE -> +1)"
pg "INSERT INTO config.settings (namespace,key,value) VALUES ('proof','$C4_KEY',repeat('x',9000));" >/dev/null
C4_REV_AFTER="$(pg "SELECT revision FROM config.revision;" | tr -d '[:space:]')"
C4_LEN="$(pg "SELECT length(value) FROM config.settings WHERE namespace='proof' AND key='$C4_KEY';" | tr -d '[:space:]')"
C4_EVT="$(pg "SELECT count(*) FROM asyncevents.events WHERE topic='config.changed' AND payload->>'key'='$C4_KEY' AND length(payload->>'value')=9000;" | tr -d '[:space:]')"
echo "    -> revision $C4_REV_BEFORE -> $C4_REV_AFTER, stored length=$C4_LEN, durable config.changed rows (full value)=$C4_EVT"
if [ "$C4_REV_AFTER" = "$((C4_REV_BEFORE + 1))" ] && [ "$C4_LEN" = "9000" ] && [ "$C4_EVT" = "1" ]; then
    pass "large config value committed: revision +1, 9000-char value round-trips, durable event carries the full value (NOTIFY stayed value-less)"
else
    fail "large config write expected rev+1 / len=9000 / 1 durable event, got rev $C4_REV_BEFORE->$C4_REV_AFTER len=$C4_LEN events=$C4_EVT"
fi

# [C4b] the reload pipeline still works WITH the 9KB row in the store: the
# reset-to-default DELETE below bumps the revision and NOTIFYs, and B's refresh
# re-reads the WHOLE snapshot -- which now contains the large value -- so a fresh
# character reverting to starter_sword proves CachedConfig + the one-statement
# snapshot query swallow a large value cross-process. (This replaces the previously
# silent rerun-cleanliness reset with an asserted one.)
echo "[C4b] reset starter_item (DELETE) -> fresh characters revert to starter_sword (reload with the 9KB row present)"
pg "DELETE FROM config.settings WHERE namespace='inventory' AND key='starter_item';" >/dev/null
REVERT_OK=0
for i in $(seq 1 30); do
    VCID="$(curl -s -X POST "http://localhost:$G_PORT/characters" \
        -H "X-Api-Key: dev-key-client" \
        -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d '{"name":"Reverted","class":"mage"}' | grep -o '"id":"[^"]*"' | head -1 | sed 's/"id":"//;s/"//')"
    for j in $(seq 1 10); do
        R="$(curl -s "http://localhost:$G_PORT/inventory/character/$VCID" -H "X-Api-Key: dev-key-client" -H "Authorization: Bearer $TOKEN")"
        echo "$R" | grep -qE 'starter_sword|health_potion' && break
        sleep 0.3
    done
    if echo "$R" | grep -q 'starter_sword'; then
        echo "    attempt $i -> char $VCID granted starter_sword"
        REVERT_OK=1; break
    fi
    sleep 0.5
done
if [ "$REVERT_OK" = "1" ]; then
    pass "starter reverted to starter_sword after the delete (snapshot refresh works with a >8KB value in the store)"
else
    fail "starter never reverted to starter_sword after the reset (reload broken alongside the large value?) -- $R"
fi
# Drop the large row so reruns start clean (each run keys it by RUN_SUFFIX anyway).
pg "DELETE FROM config.settings WHERE namespace='proof' AND key='$C4_KEY';" >/dev/null

echo ""
echo "========= ADMIN PORTAL (gateway-svc passthrough -> admin-svc -> providers over edge) ========="
# The admin fan-out end-to-end: a browser hits gateway-svc :8082 /admin, which
# reverse-proxies (Step 7 passthrough) to admin-svc :8085, which fetches each
# provider's admin page over the mTLS QUIC edge. The characters page must render a
# character CREATED on characters-svc -- proving the data crossed TWO process hops
# (G's HTTP passthrough -> E, then E's admin.adminData -> A over QUIC).
APROOF="AdminProof-$RUN_SUFFIX"
echo "[AD0] create a character named $APROOF through G -> A (for the admin table assertion)"
ACID="$(curl -s -X POST "http://localhost:$G_PORT/characters" \
    -H "X-Api-Key: dev-key-client" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    -d "{\"name\":\"$APROOF\",\"class\":\"ranger\"}" | grep -o '"id":"[^"]*"' | head -1 | sed 's/"id":"//;s/"//')"
[ -n "$ACID" ] && pass "admin-proof character created (id=$ACID)" || fail "admin-proof character not created"

ADMIN_JAR="$RUN_DIR/admin-proof.jar"
rm -f "$ADMIN_JAR"

echo "[AD1] GET http://localhost:$G_PORT/admin WITHOUT a session -> 303 Location /admin/login"
AN="$(curl -s -o /dev/null -w '%{http_code} %{redirect_url}' "http://localhost:$G_PORT/admin")"
AN_CODE="${AN%% *}"; AN_LOC="${AN#* }"
echo "    -> HTTP $AN_CODE  Location=$AN_LOC"
if [ "$AN_CODE" = "303" ] && [ "${AN_LOC%/admin/login}" != "$AN_LOC" ]; then
    pass "unauthenticated /admin -> 303 to /admin/login through the passthrough (session gate live on admin-svc)"
else
    fail "unauthenticated /admin expected 303 -> /admin/login, got $AN_CODE $AN_LOC"
fi

# [AD2] asymmetric lockout: prooflock fails 6x. Each answer is the SAME generic 401
# body (no username/lock oracle). The user row locks at 5 fails; the ip row (shared
# subject, threshold 20) increments but does NOT lock. Clear both subjects first so the
# assertion is deterministic across reruns.
pg "DELETE FROM admin.login_attempts WHERE subject = 'user:$PROOF_LOCK_USER' OR subject LIKE 'ip:%';" >/dev/null
echo "[AD2] POST /admin/login as $PROOF_LOCK_USER x6 wrong password -> each 401 identical body; user locks, ip does not"
AD2_OK=1
AD2_FIRST=""
for i in $(seq 1 6); do
    L6="$(curl -s -w $'\n%{http_code}' -X POST "http://localhost:$G_PORT/admin/login" \
        -d "username=$PROOF_LOCK_USER&password=wrong-$PROOF_LOCK_USER")"
    L6BODY="$(echo "$L6" | sed '$d')"; L6CODE="$(echo "$L6" | tail -1)"
    [ "$L6CODE" = "401" ] || { AD2_OK=0; echo "    attempt $i -> HTTP $L6CODE (expected 401)"; }
    if [ -z "$AD2_FIRST" ]; then AD2_FIRST="$L6BODY"; elif [ "$L6BODY" != "$AD2_FIRST" ]; then AD2_OK=0; echo "    attempt $i -> body differs from the first 401 (oracle leak)"; fi
done
U_FAILS="$(pg "SELECT fails FROM admin.login_attempts WHERE subject='user:$PROOF_LOCK_USER';" | tr -d '[:space:]')"
U_LOCKED="$(pg "SELECT locked_until IS NOT NULL FROM admin.login_attempts WHERE subject='user:$PROOF_LOCK_USER';" | tr -d '[:space:]')"
IP_LOCKED="$(pg "SELECT count(*) FROM admin.login_attempts WHERE subject LIKE 'ip:%' AND locked_until IS NOT NULL;" | tr -d '[:space:]')"
echo "    -> user:$PROOF_LOCK_USER fails=${U_FAILS:-?} locked=${U_LOCKED:-?} ; ip-rows-locked=${IP_LOCKED:-?}"
if [ "$AD2_OK" = "1" ] && [ "${U_FAILS:-0}" -ge 5 ] && [ "$U_LOCKED" = "t" ] && [ "${IP_LOCKED:-1}" = "0" ]; then
    pass "6 wrong logins -> identical 401 bodies; user:$PROOF_LOCK_USER locked (fails>=5), ip subject NOT locked (asymmetric thresholds)"
else
    fail "lockout expected identical 401 x6 + user locked (fails>=5) + ip not locked, got ok=$AD2_OK fails=${U_FAILS:-?} locked=${U_LOCKED:-?} ip-locked=${IP_LOCKED:-?}"
fi

# [AD2b] Twelve requests race inside the admission burst. Advisory transaction
# locks must make the user threshold exact rather than allowing a parallel burst
# through the pre-check.
AD2B_IP="198.51.100.42"
pg "DELETE FROM admin.login_attempts WHERE subject IN ('user:$PROOF_LOCK_USER','ip:$AD2B_IP'); DELETE FROM asyncevents.events WHERE topic='admin.action' AND payload->>'actor'='$PROOF_LOCK_USER' AND payload->>'action'='login-locked';" >/dev/null
echo "[AD2b] 12 parallel wrong logins -> exact user threshold and one lock event"
AD2B_DIR="$RUN_DIR/ad2b"; rm -rf "$AD2B_DIR"; mkdir -p "$AD2B_DIR"
for i in $(seq 1 12); do
    curl -s -o "$AD2B_DIR/$i.body" -w '%{http_code}' -X POST "http://localhost:$G_PORT/admin/login" \
        -H "X-Forwarded-For: $AD2B_IP" -d "username=$PROOF_LOCK_USER&password=wrong-$i" >"$AD2B_DIR/$i.code" &
done
wait
AD2B_FAILS="$(pg "SELECT fails FROM admin.login_attempts WHERE subject='user:$PROOF_LOCK_USER';" | tr -d '[:space:]')"
AD2B_LOCKED="$(pg "SELECT locked_until > now() FROM admin.login_attempts WHERE subject='user:$PROOF_LOCK_USER';" | tr -d '[:space:]')"
AD2B_EVENTS="$(pg "SELECT count(*) FROM asyncevents.events WHERE topic='admin.action' AND payload->>'actor'='$PROOF_LOCK_USER' AND payload->>'action'='login-locked';" | tr -d '[:space:]')"
if [ "$AD2B_FAILS" = "5" ] && [ "$AD2B_LOCKED" = "t" ] && [ "$AD2B_EVENTS" = "1" ]; then
    pass "parallel lockout serialized: fails=5, active lock, one login-locked event"
else
    fail "parallel lockout expected fails=5/locked/one event, got $AD2B_FAILS/$AD2B_LOCKED/$AD2B_EVENTS"
fi

# [AD2c] Separate IP exhausts the production burst and receives the exact public
# admission response, independently of the transactional user-lock assertion.
AD2C_IP="198.51.100.43"
echo "[AD2c] login admission burst -> exact 429 + Retry-After: 1"
AD2C_DIR="$RUN_DIR/ad2c"; rm -rf "$AD2C_DIR"; mkdir -p "$AD2C_DIR"
for i in $(seq 1 40); do
    curl -s -D "$AD2C_DIR/$i.headers" -o /dev/null -w '%{http_code}' -X POST "http://localhost:$G_PORT/admin/login" \
        -H "X-Forwarded-For: $AD2C_IP" -d "username=ghost-ad2c-$i&password=wrong" >"$AD2C_DIR/$i.code" &
done
wait
AD2C_429="$(grep -l '^429$' "$AD2C_DIR"/*.code | wc -l | tr -d '[:space:]')"
AD2C_RETRY="$(grep -il '^retry-after: 1' "$AD2C_DIR"/*.headers | wc -l | tr -d '[:space:]')"
if [ "${AD2C_429:-0}" -ge 1 ] && [ "$AD2C_429" = "$AD2C_RETRY" ]; then
    pass "admin login limiter returns 429 with Retry-After: 1"
else
    fail "admin login limiter expected every 429 to carry Retry-After: 1, got 429=$AD2C_429 retry=$AD2C_RETRY"
fi

# [AD3] happy-path session login: proofadmin logs in through G's passthrough, gets a
# 303 + admin_session cookie in the jar, then the jar authorizes the cross-process
# admin pages -- /admin/characters (G -> E -> A over QUIC, renders the AD0 character)
# and /admin/api-keys (G -> E -> L over QUIC, renders the seeded dev-client key; the old
# [K5] rides this session).
echo "[AD3] POST /admin/login as $PROOF_ADMIN_USER (curl -c jar) -> 303 + admin_session cookie"
AD3CODE="$(curl -s -c "$ADMIN_JAR" -o /dev/null -w '%{http_code}' -X POST "http://localhost:$G_PORT/admin/login" \
    -d "username=$PROOF_ADMIN_USER&password=$PROOF_ADMIN_PASS")"
echo "    -> HTTP $AD3CODE  (jar $(grep -c admin_session "$ADMIN_JAR" 2>/dev/null || echo 0) admin_session cookie)"
if [ "$AD3CODE" = "303" ] && grep -q admin_session "$ADMIN_JAR" 2>/dev/null; then
    pass "$PROOF_ADMIN_USER login -> 303 + admin_session cookie minted (session auth live)"
else
    fail "$PROOF_ADMIN_USER login expected 303 + admin_session cookie, got $AD3CODE"
fi

echo "[AD3a] GET /admin/characters WITH the session jar -> 200 + contains $APROOF"
AD="$(curl -s -w $'\n%{http_code}' -b "$ADMIN_JAR" "http://localhost:$G_PORT/admin/characters")"
ADBODY="$(echo "$AD" | sed '$d')"; ADCODE="$(echo "$AD" | tail -1)"
echo "    -> HTTP $ADCODE  (body $(echo -n "$ADBODY" | wc -c) bytes)"
if [ "$ADCODE" = "200" ] && echo "$ADBODY" | grep -q "$APROOF"; then
    pass "admin /admin/characters renders $APROOF cross-process (session jar; G passthrough -> E -> A admin.adminData over QUIC)"
else
    fail "admin characters page expected 200 containing $APROOF, got $ADCODE"
fi

echo "[AD3b] GET /admin/api-keys WITH the session jar -> 200 + contains dev-client (old K5 rides the session)"
# The apikeys admin fan-out end-to-end: G's HTTP passthrough -> admin-svc :8085, then
# admin-svc's admin.adminData -> apikeys-svc :$L_PORT over the mTLS QUIC edge. The page
# must render the seeded `dev-client` key row (APIKEYS_DEV_SEED=1 on L), proving the
# remote apikeys admin item composed across TWO process hops. (The slug is `api-keys`:
# the admin portal derives it from the "API Keys" LABEL, like "Audit Log" -> audit-log.)
K5="$(curl -s -w $'\n%{http_code}' -b "$ADMIN_JAR" "http://localhost:$G_PORT/admin/api-keys")"
K5BODY="$(echo "$K5" | sed '$d')"; K5CODE="$(echo "$K5" | tail -1)"
echo "    -> HTTP $K5CODE  (body $(echo -n "$K5BODY" | wc -c) bytes)"
if [ "$K5CODE" = "200" ] && echo "$K5BODY" | grep -q "dev-client"; then
    pass "admin /admin/apikeys renders dev-client cross-process (session jar; G passthrough -> E -> L admin.adminData over QUIC)"
else
    fail "admin apikeys page expected 200 containing dev-client, got $K5CODE"
fi

# [AD4] CSRF: a POST with a valid session but NO _csrf field is rejected 403 -- the
# CSRF check runs BEFORE the local/remote editability decision, so the remote apikeys
# item answers 403 (not the 405 a remote form would otherwise get).
echo "[AD4] POST /admin/api-keys WITH the session jar but NO _csrf -> 403"
AD4CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$ADMIN_JAR" -X POST "http://localhost:$G_PORT/admin/api-keys" \
    -d "dummy=1")"
echo "    -> HTTP $AD4CODE"
if [ "$AD4CODE" = "403" ]; then
    pass "session POST without _csrf -> 403 (CSRF gate precedes the editability decision)"
else
    fail "CSRF-less POST expected 403, got $AD4CODE"
fi

# [AD5] the durable admin.action trail: the login-succeeded ([AD3]) and login-locked
# ([AD2]) events are on the shared log, and audit-svc's 7th subscription records them.
echo "[AD5] admin.action durable trail: asyncevents.events >= 2 AND audit.log has admin.action rows"
AD5_EVENTS="$(pg "SELECT count(*) FROM asyncevents.events WHERE topic='admin.action';" | tr -d '[:space:]')"
echo "    -> asyncevents.events admin.action rows = ${AD5_EVENTS:-?}"
AD5_OK=0
for i in $(seq 1 30); do
    AD5_AUDIT="$(pg "SELECT count(*) FROM audit.log WHERE topic='admin.action';" | tr -d '[:space:]')"
    echo "    attempt $i -> audit.log admin.action rows = ${AD5_AUDIT:-?}"
    if [ "${AD5_EVENTS:-0}" -ge 2 ] && [ "${AD5_AUDIT:-0}" -ge 2 ]; then
        pass "admin.action emitted (>=2 on the shared log) AND recorded by audit-svc (durable E->F, 7th subscription)"
        AD5_OK=1; break
    fi
    sleep 0.5
done
[ "$AD5_OK" = "1" ] || fail "admin.action never reached >=2 events + audit rows (asyncevents=${AD5_EVENTS:-?} audit=${AD5_AUDIT:-?})"

echo ""
echo "========= AUDIT LEDGER (durable events -> audit-svc :$F_PORT) ========="
# The append-only ledger end-to-end across processes: each producer appends its
# durable event to the shared log, audit-svc's pull worker delivers it, and audit's
# on_tx_raw records it in schema
# `audit` (exactly-once, on the delivery tx). We assert the ROWS directly on the
# shared DB (the definitive check that the cross-process record handler ran):
#   (i)  the character CREATED in [1] + DELETED in [4] -> character.created/.deleted,
#   (ii) the player REGISTERED in [A1] -> player.registered,
#   (iii) the "Audit Log" admin page renders through the gateway passthrough (G -> E ->
#         F over QUIC).
echo "[AU1] poll audit.log on F for character.created + character.deleted rows (CID=$CID)"
AU_OK=0
for i in $(seq 1 30); do
    AN_CREATED="$(pg "SELECT count(*) FROM audit.log WHERE topic='character.created' AND payload->>'character_id'='$CID';" | tr -d '[:space:]')"
    AN_DELETED="$(pg "SELECT count(*) FROM audit.log WHERE topic='character.deleted' AND payload->>'character_id'='$CID';" | tr -d '[:space:]')"
    echo "    attempt $i -> created=${AN_CREATED:-?} deleted=${AN_DELETED:-?}"
    if [ "$AN_CREATED" = "1" ] && [ "$AN_DELETED" = "1" ]; then
        pass "audit-svc recorded character.created + character.deleted for $CID (durable A->F, exactly-once)"
        AU_OK=1; break
    fi
    sleep 0.5
done
[ "$AU_OK" = "1" ] || fail "audit-svc never recorded both character events for $CID (durable delivery A->F)"

echo "[AU2] poll audit.log on F for the player.registered row (PID=$PID)"
AU2_OK=0
for i in $(seq 1 30); do
    AN_REG="$(pg "SELECT count(*) FROM audit.log WHERE topic='player.registered' AND payload->>'player_id'='$PID';" | tr -d '[:space:]')"
    echo "    attempt $i -> player.registered=${AN_REG:-?}"
    if [ "$AN_REG" = "1" ]; then
        pass "audit-svc recorded player.registered for $PID (durable D->F)"
        AU2_OK=1; break
    fi
    sleep 0.5
done
[ "$AU2_OK" = "1" ] || fail "audit-svc never recorded player.registered for $PID (durable delivery D->F)"

echo "[AU3] GET http://localhost:$G_PORT/admin/audit-log WITH the session jar -> 200 + a logged topic"
AUD="$(curl -s -w $'\n%{http_code}' -b "$ADMIN_JAR" "http://localhost:$G_PORT/admin/audit-log")"
AUDBODY="$(echo "$AUD" | sed '$d')"; AUDCODE="$(echo "$AUD" | tail -1)"
echo "    -> HTTP $AUDCODE  (body $(echo -n "$AUDBODY" | wc -c) bytes)"
if [ "$AUDCODE" = "200" ] && echo "$AUDBODY" | grep -qE 'character\.(created|deleted)|player\.registered'; then
    pass "admin /admin/audit-log renders the ledger cross-process (G passthrough -> E -> F admin.adminData over QUIC)"
else
    fail "admin audit-log page expected 200 containing a logged topic, got $AUDCODE"
fi

echo ""
echo "========= SCHEDULER (scheduler-svc :$H_PORT -> audit-svc :$F_PORT) ========="
# The data-driven durable emitter end-to-end: seed a short (2s) schedule on H's shared
# DB, immediately due. H's 1s loop acquires the per-schedule advisory lock, re-checks
# still-due, bumps last_fired + emit_tx's scheduler.fired IN ONE TX onto the shared
# log. We assert on the shared DB (the definitive proof the fire ran + was consumed):
#   (i)  a scheduler.fired event in asyncevents.events for proof-tick (advisory-lock fire),
#   (ii) audit-svc's pull cursor (subscription audit.prune-on-scheduler.v1) advanced
#        PAST that event's position -- H's event was delivered to F's worker.
echo "[SC0] seed a 2s schedule 'proof-tick' on the shared DB (immediately due, epoch last_fired)"
# Drop stale proof-tick events from earlier runs so SC1 proves THIS run's fire.
pg "DELETE FROM asyncevents.events WHERE topic='scheduler.fired' AND payload->>'name'='proof-tick';" >/dev/null
pg "INSERT INTO scheduler.schedules (name, interval_seconds, last_fired) VALUES ('proof-tick', 2, to_timestamp(0)) ON CONFLICT (name) DO UPDATE SET interval_seconds=2, last_fired=to_timestamp(0);" >/dev/null
echo "[SC1] poll the shared log for scheduler.fired{proof-tick} AND audit's pull cursor past it"
SC_OK=0
for i in $(seq 1 30); do
    SC_FIRED="$(pg "SELECT count(*) FROM asyncevents.events WHERE topic='scheduler.fired' AND payload->>'name'='proof-tick';" | tr -d '[:space:]')"
    SC_CONSUMED="$(pg "SELECT count(*) FROM asyncevents.subscriptions s, asyncevents.events e WHERE s.subscription_id='audit.prune-on-scheduler.v1' AND e.topic='scheduler.fired' AND e.payload->>'name'='proof-tick' AND (s.cursor_generation, s.cursor_xid, s.cursor_tie) >= (e.generation, e.producer_xid, e.tie_breaker);" | tr -d '[:space:]')"
    echo "    attempt $i -> fired=${SC_FIRED:-?} consumed=${SC_CONSUMED:-?}"
    if [ "${SC_FIRED:-0}" -ge 1 ] && [ "${SC_CONSUMED:-0}" -ge 1 ]; then
        pass "scheduler-svc fired proof-tick durably (advisory-lock + stillDue re-check) AND audit's cursor advanced past it (H's event pulled by F)"
        SC_OK=1; break
    fi
    sleep 0.5
done
[ "$SC_OK" = "1" ] || fail "scheduler.fired{proof-tick} never produced+consumed (scheduler H -> shared log -> audit F)"
# Clean up so reruns start fresh (retention GC arrives at plan Step 5).
pg "DELETE FROM scheduler.schedules WHERE name='proof-tick';" >/dev/null

echo ""
echo "========= SESSION PRUNE (scheduler-svc :$H_PORT -> accounts-svc :$D_PORT) ========="
# The durable session-prune reaction end-to-end across processes: accounts-svc (D)
# subscribes accounts.prune-on-scheduler.v1 and, on scheduler.fired{accounts-sessions-prune},
# DELETEs expired rows from accounts.sessions in the delivery tx. We plant an already-expired
# session on the shared DB, force the SEEDED daily schedule to fire NOW (reset last_fired to
# the epoch -> immediately due, like proof-tick above; a reused dev DB has it advanced, so
# the reset makes the fire deterministic), then poll until D's handler has removed the row.
# NOT via a synthetic asyncevents.append_event: forging an event the scheduler solely
# produces would violate publisher-owns-the-event (and feed audit's raw sink a fake row).
echo "[SP0] plant a throwaway player + an EXPIRED session on the shared DB (FK needs a real player)"
SP_PID="$(pg "INSERT INTO accounts.players (display_name) VALUES ('prune-proof-$RUN_SUFFIX') RETURNING id::text;" | head -n1 | tr -d '[:space:]')"
[ -n "$SP_PID" ] || { fail "could not insert throwaway player for the session-prune proof"; exit 1; }
SP_TOKEN="prune-proof-$RUN_SUFFIX"
pg "INSERT INTO accounts.sessions (token, player_id, expires_at) VALUES ('$SP_TOKEN', '$SP_PID'::uuid, now() - interval '1 day');" >/dev/null
echo "[SP1] force the seeded 'accounts-sessions-prune' schedule due NOW (reset last_fired to epoch)"
pg "UPDATE scheduler.schedules SET last_fired = to_timestamp(0) WHERE name = 'accounts-sessions-prune';" >/dev/null
echo "[SP2] poll accounts.sessions until D's prune handler removes the expired row"
SP_OK=0
for i in $(seq 1 30); do
    SP_LEFT="$(pg "SELECT count(*) FROM accounts.sessions WHERE token='$SP_TOKEN';" | tr -d '[:space:]')"
    echo "    attempt $i -> expired_rows_left=${SP_LEFT:-?}"
    if [ "${SP_LEFT:-1}" = "0" ]; then
        pass "scheduler-svc fired accounts-sessions-prune; accounts-svc pruned the expired session (durable H -> D on the delivery tx)"
        SP_OK=1; break
    fi
    sleep 0.5
done
[ "$SP_OK" = "1" ] || fail "expired session was never pruned (scheduler H -> shared log -> accounts D subscription accounts.prune-on-scheduler.v1)"
# Clean up the throwaway player (CASCADE removes any residual session) so reruns start fresh.
pg "DELETE FROM accounts.players WHERE id='$SP_PID'::uuid;" >/dev/null

echo ""
echo "========= MATCH TRIO (match-svc :$I_PORT + rating-svc :$J_PORT + leaderboard-svc :$K_PORT) ========="
# The match trio end-to-end across processes, all through the gateway front door:
#   (i)   POST /match/report through G (AuthNone) -> 202. G routes match.report Remote to
#         match-svc (I) over the mTLS edge; I SYNC-reads both players' MMR from rating-svc
#         (J) over QUIC (a 202 with J UP proves that sync seam resolved), records the
#         match + emit_tx's match.finished in one tx.
#   (ii)  GET /leaderboard through G shows the winner with wins=1 (poll -- durable delivery
#         I->K is async). Proves match.finished crossed I -> leaderboard-svc (K) and the
#         durable on_tx upsert ran, AND that G routes leaderboard.topScores Remote to K.
#   (iii) audit-svc (F) has a match.finished row (durable I->F, exactly-once).
#   (iv)  a second report for the SAME winner -> wins=2 (accumulating upsert).
#   (v)   rating (DB-backed projection, no public read endpoint): the sync MMR read is
#         proven by (i) succeeding with rating-svc UP -- a report cannot return 202 without
#         J answering rating.mmr over the edge. The +15/-15 durable handler persists to
#         rating.ratings on J, asserted directly in [MT5] after both reports.
#   (vi)  re-POSTing [MT1]'s exact ReportId is an idempotent no-op: still 202, one
#         match row, no third match.finished (a caller replay after an ambiguous
#         response MUST NOT double-commit a match).
WINNER="champ-$RUN_SUFFIX"
LOSER="chump-$RUN_SUFFIX"
# ReportId is the REQUIRED idempotency key. Per-run-unique (RUN_SUFFIX): the cleanup
# below deletes leaderboard/rating rows but NOT match.matches, so a constant id would
# dedup on the SECOND split-proof run and [MT2]/[MT4] would never see wins move.
MT1_RID="mt1-$RUN_SUFFIX"
MT4_RID="mt4-$RUN_SUFFIX"

echo "[MT1] POST http://localhost:$G_PORT/match/report (AuthNone, capitalized ReportId/Winner/Loser body keys)"
MR="$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://localhost:$G_PORT/match/report" \
    -H "X-Api-Key: dev-key-server" \
    -H "Content-Type: application/json" \
    -d "{\"ReportId\":\"$MT1_RID\",\"Winner\":\"$WINNER\",\"Loser\":\"$LOSER\"}")"
echo "    -> HTTP $MR"
if [ "$MR" = "202" ]; then
    pass "match.report through G -> 202 (AuthNone; match-svc read rating.mmr from rating-svc over QUIC, recorded + emit_tx'd match.finished)"
else
    fail "match.report expected 202, got $MR"
fi

echo "[MT2] poll GET http://localhost:$G_PORT/leaderboard through G until $WINNER shows wins=1"
LB_OK=0
for i in $(seq 1 30); do
    LB="$(curl -s "http://localhost:$G_PORT/leaderboard" -H "X-Api-Key: dev-key-client")"
    if echo "$LB" | grep -q "\"player\":\"$WINNER\",\"wins\":1"; then
        echo "    attempt $i -> $LB"
        pass "leaderboard shows $WINNER wins=1 (durable match.finished I->K + on_tx upsert; G routes leaderboard.topScores Remote to K)"
        LB_OK=1; break
    fi
    sleep 0.5
done
[ "$LB_OK" = "1" ] || fail "leaderboard never showed $WINNER wins=1 (durable I->K delivery / upsert / routing)"

echo "[MT3] poll audit.log on F for a match.finished row (winner=$WINNER)"
MT3_OK=0
for i in $(seq 1 30); do
    AN_MF="$(pg "SELECT count(*) FROM audit.log WHERE topic='match.finished' AND payload->>'winner'='$WINNER';" | tr -d '[:space:]')"
    echo "    attempt $i -> match.finished=${AN_MF:-?}"
    if [ "${AN_MF:-0}" -ge 1 ]; then
        pass "audit-svc recorded match.finished for $WINNER (durable I->F, exactly-once)"
        MT3_OK=1; break
    fi
    sleep 0.5
done
[ "$MT3_OK" = "1" ] || fail "audit-svc never recorded match.finished for $WINNER (durable delivery I->F)"

echo "[MT4] second POST /match/report same winner -> leaderboard wins=2 (accumulating upsert)"
MR2="$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://localhost:$G_PORT/match/report" \
    -H "X-Api-Key: dev-key-server" \
    -H "Content-Type: application/json" \
    -d "{\"ReportId\":\"$MT4_RID\",\"Winner\":\"$WINNER\",\"Loser\":\"$LOSER\"}")"
echo "    -> report#2 HTTP $MR2"
[ "$MR2" = "202" ] || fail "second match.report expected 202, got $MR2"
MT4_OK=0
for i in $(seq 1 30); do
    LB="$(curl -s "http://localhost:$G_PORT/leaderboard" -H "X-Api-Key: dev-key-client")"
    if echo "$LB" | grep -q "\"player\":\"$WINNER\",\"wins\":2"; then
        echo "    attempt $i -> $WINNER wins=2"
        pass "second report -> $WINNER wins=2 (leaderboard upsert accumulates across durable events)"
        MT4_OK=1; break
    fi
    sleep 0.5
done
[ "$MT4_OK" = "1" ] || fail "leaderboard never reached wins=2 for $WINNER (accumulating upsert)"

# rating is a DURABLE PROJECTION (Step 9), not in-memory: the +15/-15 handler upserts
# rating.ratings on J inside the delivery tx. After the two reports above the winner is
# 1000+15+15=1030 and the loser 1000-15-15=970 -- a persisted value the checkpoint
# advanced over, so a restart would NOT reset it.
echo "[MT5] poll rating.ratings on J for the persisted projection ($WINNER -> mmr=1030, $LOSER -> mmr=970)"
MT5_OK=0
for i in $(seq 1 30); do
    W_MMR="$(pg "SELECT coalesce((SELECT mmr FROM rating.ratings WHERE player='$WINNER'), -1);" | tr -d '[:space:]')"
    L_MMR="$(pg "SELECT coalesce((SELECT mmr FROM rating.ratings WHERE player='$LOSER'), -1);" | tr -d '[:space:]')"
    echo "    attempt $i -> winner mmr=${W_MMR:-?}, loser mmr=${L_MMR:-?}"
    if [ "${W_MMR:-0}" = "1030" ] && [ "${L_MMR:-0}" = "970" ]; then
        pass "rating.ratings persisted $WINNER=1030 / $LOSER=970 (durable +15/-15 projection on J, restart-safe)"
        MT5_OK=1; break
    fi
    sleep 0.5
done
[ "$MT5_OK" = "1" ] || fail "rating.ratings never reached winner=1030 / loser=970 (durable projection on J)"

# --- [MT6] duplicate-report-idempotent: mutating RPCs are not transport-retried, but
# a caller may re-send after an ambiguous result. Re-POST with [MT1]'s exact ReportId -> still 202,
# but NO second match row (psql, the strong assertion) and NO third match.finished
# (leaderboard wins stays 2, rating stays 1030/970).
echo "[MT6] duplicate POST /match/report with [MT1]'s ReportId ($MT1_RID) -> 202 no-op (dedup)"
MR3="$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://localhost:$G_PORT/match/report" \
    -H "X-Api-Key: dev-key-server" \
    -H "Content-Type: application/json" \
    -d "{\"ReportId\":\"$MT1_RID\",\"Winner\":\"$WINNER\",\"Loser\":\"$LOSER\"}")"
echo "    -> duplicate report HTTP $MR3"
[ "$MR3" = "202" ] || fail "duplicate match.report expected 202 (idempotent no-op), got $MR3"
sleep 2 # give a hypothetical (wrong) third match.finished time to reach leaderboard
MT6_ROWS="$(pg "SELECT count(*) FROM match.matches WHERE report_id='$MT1_RID';" | tr -d '[:space:]')"
MT6_LB="$(curl -s "http://localhost:$G_PORT/leaderboard" -H "X-Api-Key: dev-key-client")"
echo "    -> match.matches rows for $MT1_RID = ${MT6_ROWS:-?}"
if [ "${MT6_ROWS:-0}" = "1" ] && echo "$MT6_LB" | grep -q "\"player\":\"$WINNER\",\"wins\":2"; then
    pass "duplicate ReportId -> 202, 1 match row, leaderboard wins still 2 (dedup skipped the emit)"
else
    fail "duplicate ReportId not idempotent (rows=${MT6_ROWS:-?}, wins!=2?)"
fi

# Clean up this run's leaderboard + rating rows so reruns start fresh.
pg "DELETE FROM leaderboard.scores WHERE player IN ('$WINNER','$LOSER');" >/dev/null
pg "DELETE FROM rating.ratings WHERE player IN ('$WINNER','$LOSER');" >/dev/null

echo ""
echo "========= PLAYER QUIC FRONT (via gateway-svc :$PLAYER_PORT) ========="
# DEFERRED (documented, not faked): the player-QUIC ANTI-SPOOF branch -- admission
# gated on a validated source address (unvalidated Incoming -> quinn retry(), no
# connection slot consumed) -- cannot be asserted here: this script cannot forge an
# off-path UDP source address, and a happy-path playercli dial only proves the
# validated branch. Coverage lives in the unit tests (core/edge/src/player_tests.rs).
# The player RATE-LIMIT (a different, observable guard) IS asserted below in [P6].

# --- P1. player QUIC create -> G -> mTLS edge -> A ---------------------------
# A FRESH character owned by the registered player, created THROUGH the QUIC player front (the
# original CID from [1] was deleted in [4]). playercli exits 0 iff transport ok AND
# the payload's status=="Ok".
echo "[P1] playercli characters.create over QUIC :$PLAYER_PORT (--token <real> --api-key dev-key-client)"
P1_OUT="$("$PLAYERCLI" --addr "127.0.0.1:$PLAYER_PORT" --ca "$CA_CERT" --token "$TOKEN" --api-key "dev-key-client" \
    characters.create '{"name":"hero","class":""}' 2>/dev/null)"
P1_RC=$?
echo "    -> rc=$P1_RC  $P1_OUT"
PCID="$(echo "$P1_OUT" | grep -o '"id":"[^"]*"' | head -1 | sed 's/"id":"//;s/"//')"
if [ "$P1_RC" -eq 0 ] && [ -n "$PCID" ]; then
    pass "player create -> exit 0, id=$PCID (player QUIC -> G -> mTLS edge -> A)"
else
    fail "player create expected exit 0 with id, got rc=$P1_RC"
fi

# --- P2. player QUIC inventory list -> G -> Remote -> B's NEW :9001 edge ------
# The newest composition: assertion P1 alone only proves the G->A leg; this proves
# player QUIC -> G -> Remote -> B, and B in turn calls owner_of over QUIC/mTLS to A.
echo "[P2] playercli inventory.listCharacter over QUIC :$PLAYER_PORT (player QUIC -> G -> Remote -> B :$B_EDGE_PORT)"
P2_OUT="$("$PLAYERCLI" --addr "127.0.0.1:$PLAYER_PORT" --ca "$CA_CERT" --token "$TOKEN" --api-key "dev-key-client" \
    inventory.listCharacter "{\"character_id\":\"$PCID\"}" 2>/dev/null)"
P2_RC=$?
echo "    -> rc=$P2_RC  $P2_OUT"
if [ "$P2_RC" -eq 0 ]; then
    pass "player inventory list -> exit 0 (player QUIC -> G -> Remote -> B :$B_EDGE_PORT -> owner_of QUIC -> A)"
else
    fail "player inventory list expected exit 0, got rc=$P2_RC"
fi

# --- P3. gateway-svc HTTP front still routes cross-provider inventory.* -> B --
echo "[P3] GET http://localhost:$G_PORT/inventory/character/$PCID through gateway-svc HTTP front (Bearer \$TOKEN)"
P3="$(curl -s -w $'\n%{http_code}' "http://localhost:$G_PORT/inventory/character/$PCID" -H "X-Api-Key: dev-key-client" -H "Authorization: Bearer $TOKEN")"
P3BODY="$(echo "$P3" | sed '$d')"; P3CODE="$(echo "$P3" | tail -1)"
echo "    -> HTTP $P3CODE  $P3BODY"
if [ "$P3CODE" = "200" ]; then
    pass "gateway-svc HTTP front routes inventory.* -> B remote -> 200"
else
    fail "gateway-svc HTTP inventory expected 200, got $P3CODE"
fi

# --- P4. auth gate: no token / bad token on an auth op -> Unauthorized --------
# Per the pinned grammar an auth failure arrives as transport ok:true +
# {status:"Unauthorized"}, so playercli exits 1 and the envelope names it.
echo "[P4] playercli characters.create with NO token (--api-key dev-key-client) -> exit 1 + Unauthorized"
P4_OUT="$("$PLAYERCLI" --addr "127.0.0.1:$PLAYER_PORT" --ca "$CA_CERT" --api-key "dev-key-client" \
    characters.create '{"name":"x","class":""}' 2>/dev/null)"
P4_RC=$?
echo "    -> rc=$P4_RC  $P4_OUT"
if [ "$P4_RC" -ne 0 ] && echo "$P4_OUT" | grep -q 'Unauthorized'; then
    pass "no-token auth op -> exit 1 + Unauthorized (bearer required at the front)"
else
    fail "no-token expected exit 1 + Unauthorized, got rc=$P4_RC $P4_OUT"
fi

echo "[P4b] playercli characters.create with BAD token (nope-x, --api-key dev-key-client) -> exit 1 + Unauthorized"
P4B_OUT="$("$PLAYERCLI" --addr "127.0.0.1:$PLAYER_PORT" --ca "$CA_CERT" --token "nope-x" --api-key "dev-key-client" \
    characters.create '{"name":"x","class":""}' 2>/dev/null)"
P4B_RC=$?
echo "    -> rc=$P4B_RC  $P4B_OUT"
if [ "$P4B_RC" -ne 0 ] && echo "$P4B_OUT" | grep -q 'Unauthorized'; then
    pass "bad-token auth op -> exit 1 + Unauthorized (token verified, not just presence)"
else
    fail "bad-token expected exit 1 + Unauthorized, got rc=$P4B_RC $P4B_OUT"
fi

# --- P5. allow-list gate: wire-only method absent from the route table -------
# characters.ownerOf has no #[http] binding, so it is NOT in the front's route table
# -> NotFound. Proves dispatch is method-allow-listed, never a blind prefix relay.
echo "[P5] playercli characters.ownerOf (wire-only, not routable) -> exit 1 + NotFound"
P5_OUT="$("$PLAYERCLI" --addr "127.0.0.1:$PLAYER_PORT" --ca "$CA_CERT" --token "$TOKEN" --api-key "dev-key-client" \
    characters.ownerOf "{\"character_id\":\"$PCID\"}" 2>/dev/null)"
P5_RC=$?
echo "    -> rc=$P5_RC  $P5_OUT"
if [ "$P5_RC" -ne 0 ] && echo "$P5_OUT" | grep -q 'NotFound'; then
    pass "wire-only characters.ownerOf -> exit 1 + NotFound (allow-list gate live)"
else
    fail "ownerOf expected exit 1 + NotFound, got rc=$P5_RC $P5_OUT"
fi

echo "[P6] persistent player connection consumes burst, gets exact denial, refills, then succeeds"
P6_OUT="$("$PLAYERCLI" --addr "127.0.0.1:$PLAYER_PORT" --ca "$CA_CERT" --api-key "dev-key-client" \
    --repeat 22 --pause-before-last-ms 2000 leaderboard.topScores '{}' 2>&1)"
P6_RC=$?
echo "    -> rc=$P6_RC  $P6_OUT"
if [ "$P6_RC" -ne 0 ] && echo "$P6_OUT" | grep -q 'player request rate limit exceeded' && \
   [ "$(echo "$P6_OUT" | grep -c '"status":"Ok"')" -ge 21 ]; then
    pass "persistent player request limiter returns pinned denial and admits after refill"
else
    fail "persistent player request limiter proof failed: rc=$P6_RC $P6_OUT"
fi

echo "============================================"

echo ""
echo "========= HTTP METRICS (private Prometheus registry + /metrics, now a core-infra module) ========="
# metrics is now a lifecycle Module listed in EVERY main (the Config::without_metrics flag
# is gone). Two proofs:
#  - MX1 (peer pipeline): characters-svc (A) serves GET /metrics from its private registry.
#    Under the single front door the /characters ops route THROUGH gateway-svc over the mTLS
#    QUIC edge, NOT A's HTTP port, so A's HTTP surface sees only infra (skip-recorded). We
#    fire ONE recorded non-infra request at A (a 404, labeled path="unmatched") so A's own
#    counter has an http_requests_total child to render.
#  - MX2 (the point of this change): gateway-svc (G) now lists the metrics module too, so
#    GET /metrics is 200 (was 404 under without_metrics) AND records the op traffic that
#    flowed through the front door during the assertions above. G dispatches ops via an axum
#    FALLBACK (no per-op MatchedPath), but the front door now STAMPS each matched op's route
#    PATTERN onto the response (httpmw::RoutePattern), so metrics labels op traffic by
#    pattern -- e.g. the POST /characters create above records path="/characters",status="201"
#    instead of collapsing to path="unmatched". We assert that REAL op pattern label appears.
echo "[MX1] GET http://localhost:$A_PORT/metrics on characters-svc -> 200 + http_requests_total (peer pipeline)"
curl -s -o /dev/null "http://localhost:$A_PORT/__metrics_probe" || true  # one recorded non-infra hit -> a counter child
MX1="$(curl -s -w $'\n%{http_code}' "http://localhost:$A_PORT/metrics")"
MX1BODY="$(echo "$MX1" | sed '$d')"; MX1CODE="$(echo "$MX1" | tail -1)"
echo "    -> HTTP $MX1CODE  (body $(echo -n "$MX1BODY" | wc -c) bytes)"
if [ "$MX1CODE" = "200" ] && echo "$MX1BODY" | grep -q 'http_requests_total'; then
    pass "characters-svc /metrics -> 200 with http_requests_total (peer private registry serves the scrape)"
else
    fail "characters-svc /metrics expected 200 containing http_requests_total, got $MX1CODE"
fi

echo "[MX2] GET http://localhost:$G_PORT/metrics on gateway-svc -> 200 + a REAL op PATTERN label (front door now labelled per-op)"
MX2="$(curl -s -w $'\n%{http_code}' "http://localhost:$G_PORT/metrics")"
MX2BODY="$(echo "$MX2" | sed '$d')"; MX2CODE="$(echo "$MX2" | tail -1)"
echo "    -> HTTP $MX2CODE  (body $(echo -n "$MX2BODY" | wc -c) bytes)"
# The POST /characters create fronted above records under the op's route PATTERN with its
# 201 success -- path="/characters",status="201" -- proving RoutePattern labelling replaced
# the old path="unmatched" collapse. (Label order is alphabetical: method,path,status.)
if [ "$MX2CODE" = "200" ] \
   && echo "$MX2BODY" | grep -q 'http_requests_total' \
   && echo "$MX2BODY" | grep -qE 'http_requests_total\{[^}]*path="/characters"[^}]*status="2[0-9][0-9]"'; then
    pass "gateway-svc /metrics -> 200 recording real op traffic under path=\"/characters\" with a 2xx status (front door per-op route-pattern labels live)"
else
    fail "gateway-svc /metrics expected 200 with an http_requests_total{path=\"/characters\",status=2xx} op-pattern line, got $MX2CODE"
fi

echo "============================================"

echo ""
echo "========= RATE LIMITING (Step 13: gateway-svc always-on 20 rps / burst 40) ========="
# The front door ALWAYS rate limits (Config::with_rate_limit_default(20,40)); no env
# override here, so burst is 40. Hammer a cheap AuthNone op (GET /leaderboard) with 60
# rapid requests from ONE IP (127.0.0.1, untrusted -> its own bucket): with burst 40 at
# least one MUST come back 429 (the limiter short-circuits before dispatch). Then a pause
# lets the bucket refill (20 rps) and a normal request succeeds again. /healthz is
# SkipInfra: never throttled even under the same hammering.
# Fire the 60 requests in PARALLEL (curl -Z) from one process: sequential curls spawn
# slowly enough on Windows that the 20 rps refill outpaces the drain, so we hammer them
# concurrently — the bucket (burst 40) is then provably exceeded and >=20 get 429.
lb_urls=(); hz_urls=()
for i in $(seq 1 60); do
    lb_urls+=("http://localhost:$G_PORT/leaderboard")
    hz_urls+=("http://localhost:$G_PORT/healthz")
done

echo "[RL1] 60 PARALLEL GET /leaderboard through G (:$G_PORT) -> expect >=1 HTTP 429 (burst 40)"
RL_CODES="$(curl -Z --parallel-max 60 -s -o /dev/null -w '%{http_code}\n' -H "X-Api-Key: dev-key-client" "${lb_urls[@]}")"
RL_429="$(echo "$RL_CODES" | grep -c '429')"
echo "    -> $RL_429 of 60 responses were HTTP 429"
if [ "$RL_429" -ge 1 ]; then
    pass "gateway-svc rate limited a rapid burst (>=1 429 over 60 parallel requests, burst 40)"
else
    fail "gateway-svc never returned 429 over 60 parallel requests (rate limiting inactive?)"
fi

echo "[RL2] 60 PARALLEL GET /healthz through G -> expect ZERO 429 (SkipInfra)"
HZ_CODES="$(curl -Z --parallel-max 60 -s -o /dev/null -w '%{http_code}\n' "${hz_urls[@]}")"
RL_HZ="$(echo "$HZ_CODES" | grep -c '429')"
echo "    -> $RL_HZ of 60 /healthz responses were HTTP 429"
if [ "$RL_HZ" = "0" ]; then
    pass "/healthz never rate limited under 60 rapid probes (SkipInfra holds)"
else
    fail "/healthz returned 429 $RL_HZ times (SkipInfra broken)"
fi

echo "[RL3] pause 2s for the bucket to refill, then GET /leaderboard -> 200"
sleep 2
RL_OK="$(curl -s -o /dev/null -w '%{http_code}' "http://localhost:$G_PORT/leaderboard" -H "X-Api-Key: dev-key-client")"
echo "    -> post-pause GET /leaderboard -> HTTP $RL_OK"
if [ "$RL_OK" = "200" ]; then
    pass "token bucket refilled after a pause -> GET /leaderboard 200 (limiter recovers)"
else
    fail "post-pause GET /leaderboard expected 200, got $RL_OK"
fi

echo "============================================"

# ============================================================================
# MONOLITH PARITY: the SAME player QUIC front, all ops dispatched Local.
# Per the never-monolith-only-features rule both topologies must serve the feature.
# Tear the split down first (frees :8080 and :9100 and the DB), then boot cmd/server
# with PLAYER_EDGE_ADDR=:9100 + the shared CA and drive one player create.
# ============================================================================
echo ""
echo "================ MONOLITH PARITY ================"
note "tearing down the split before the monolith stage ..."
teardown
if [ -z "$STOP_FORCED" ]; then
    pass "[W1 split graceful shutdown] SIGTERM drained every process without KILL fallback"
else
    fail "[W1 split graceful shutdown] at least one process required KILL fallback"
fi
# W1b is deliberately narrower than "did it exit fast" -- it checks HOW every drained
# process exited (exit code 0), not just that it exited within the grace window. The
# in-flight-request-survives-drain probe itself is DEFERRED: the fleet has no
# artificial-delay endpoint to race a request against SIGTERM without being racy/low-value.
# DEFERRED for the same reason: the internal-edge STREAM-REAP fix (bounded
# read/stopped waits on a hung peer stream) -- there is no fault-injection endpoint
# in the fleet to wedge a stream cross-process. Coverage lives in the edge unit
# tests (core/edge/src/server_tests.rs, loopback peer with controlled waits).
if [ -z "$STOP_NONZERO" ]; then
    pass "[W1b split clean-exit] every drained process exited 0 after SIGTERM"
else
    fail "[W1b split clean-exit] at least one drained process exited non-zero after SIGTERM"
fi
STOP_FORCED=""
STOP_NONZERO=""
# Re-arm the once-guard: this mid-script teardown only freed the split's ports.
# Without the reset the EXIT-trap teardown no-ops and the monolith LEAKS, holding
# target/debug/server.exe locked and failing the next build (os error 5).
TEARDOWN_DONE=""
kill_stragglers
sleep 2

# Wipe the split phase's lockout + session residue so the monolith-parity login starts
# clean (proofadmin's split session and any prooflock lock rows must not poison parity).
# admin.users (the seeded logins) is left intact -- the monolith reuses proofadmin.
pg "DELETE FROM admin.login_attempts; DELETE FROM admin.sessions;" >/dev/null
note "cleared admin.login_attempts + admin.sessions before the monolith parity phase"

note "starting monolith (cmd/server) on :$A_PORT, player QUIC :$PLAYER_PORT ..."
# The monolith hosts every module, so it needs each module's dev opt-in explicitly:
# APIKEYS_DEV_SEED (dev keys), ACCOUNTS_DEV_AUTH ([M0] register/login), INVENTORY_DEV_GRANT
# (parity). Admin now boots on the shared `admin` schema with session auth (no
# ADMIN_USER/ADMIN_PASS); ADMIN_COOKIE_SECURE=0 + TRUSTED_PROXY_CIDRS make the [M3]
# session login work over plain http, and TLS_MODE=off keeps the front door plain.
env PORT=":$A_PORT" DATABASE_URL="$DATABASE_URL" \
    PLAYER_EDGE_ADDR=":$PLAYER_PORT" \
    EDGE_CA_CERT="$CA_CERT" EDGE_CA_KEY="$CA_KEY" \
    APIKEYS_DEV_SEED=1 \
    ACCOUNTS_DEV_AUTH=1 \
    INVENTORY_DEV_GRANT=1 \
    TLS_MODE=off ADMIN_COOKIE_SECURE=0 TRUSTED_PROXY_CIDRS="127.0.0.1/32" \
    "$BIN_DIR/server$EXE" >"$RUN_DIR/monolith.out.log" 2>"$RUN_DIR/monolith.err.log" &
M_PID=$!
if wait_healthy "$A_PORT" "monolith (server)"; then
    MSUFFIX="$(new_uuid | cut -c1-8)"
    echo "[M0] register a player on the monolith (accounts module local, real session)"
    MREG="$(curl -s -X POST "http://localhost:$A_PORT/accounts/register" \
        -H "X-Api-Key: dev-key-client" \
        -H "Content-Type: application/json" \
        -d "{\"email\":\"mono-$MSUFFIX@test.local\",\"password\":\"pw-$MSUFFIX\",\"displayName\":\"Mono\"}")"
    MTOKEN="$(echo "$MREG" | grep -o '"token":"[^"]*"' | head -1 | sed 's/"token":"//;s/"//')"
    if [ -n "$MTOKEN" ]; then
        pass "monolith register -> real bearer (parity: same auth flow, all Local)"
    else
        fail "monolith register produced no token -- $MREG"
    fi
    echo "[M1] playercli characters.create over QUIC :$PLAYER_PORT against the monolith (--token <real> --api-key dev-key-client)"
    M1_OUT="$("$PLAYERCLI" --addr "127.0.0.1:$PLAYER_PORT" --ca "$CA_CERT" --token "$MTOKEN" --api-key "dev-key-client" \
        characters.create '{"name":"solo","class":""}' 2>/dev/null)"
    M1_RC=$?
    echo "    -> rc=$M1_RC  $M1_OUT"
    if [ "$M1_RC" -eq 0 ]; then
        pass "monolith player QUIC front -> exit 0 (all ops Local, parity proven)"
    else
        fail "monolith player create expected exit 0, got rc=$M1_RC"
    fi
    echo "[M2] monolith rejects a dev- token (real verifier resolved from the local accounts module)"
    M2_OUT="$("$PLAYERCLI" --addr "127.0.0.1:$PLAYER_PORT" --ca "$CA_CERT" --token "dev-$MSUFFIX" --api-key "dev-key-client" \
        characters.create '{"name":"x","class":""}' 2>/dev/null)"
    M2_RC=$?
    echo "    -> rc=$M2_RC  $M2_OUT"
    if [ "$M2_RC" -ne 0 ] && echo "$M2_OUT" | grep -q 'Unauthorized'; then
        pass "monolith dev- token -> Unauthorized (parity with the split front)"
    else
        fail "monolith dev- token expected Unauthorized, got rc=$M2_RC $M2_OUT"
    fi
    # [M3] admin portal parity: the monolith hosts the admin module LOCAL (no fan-out),
    # with session auth on the SAME shared `admin` schema. A fresh cookie jar logs in as
    # proofadmin, then the jar renders the LOCAL characters page (the just-created "solo"
    # character) -- proving the same session-auth portal serves both topologies.
    MONO_JAR="$RUN_DIR/admin-mono.jar"
    rm -f "$MONO_JAR"
    echo "[M3] POST /admin/login on the monolith as $PROOF_ADMIN_USER (fresh jar) -> 303, then GET /admin/characters -> 200 + solo"
    M3L="$(curl -s -c "$MONO_JAR" -o /dev/null -w '%{http_code}' -X POST "http://localhost:$A_PORT/admin/login" \
        -d "username=$PROOF_ADMIN_USER&password=$PROOF_ADMIN_PASS")"
    M3="$(curl -s -w $'\n%{http_code}' -b "$MONO_JAR" "http://localhost:$A_PORT/admin/characters")"
    M3BODY="$(echo "$M3" | sed '$d')"; M3CODE="$(echo "$M3" | tail -1)"
    echo "    -> login HTTP $M3L ; characters HTTP $M3CODE  (body $(echo -n "$M3BODY" | wc -c) bytes)"
    if [ "$M3L" = "303" ] && [ "$M3CODE" = "200" ] && echo "$M3BODY" | grep -q 'solo'; then
        pass "monolith session login + /admin/characters renders LOCAL items (admin portal parity)"
    else
        fail "monolith admin parity expected login 303 + characters 200 containing solo, got login=$M3L characters=$M3CODE"
    fi

    # [M3b] LOCAL form-submit durable trail: in the monolith the apikeys page is LOCAL
    # (editable form present). Fetch it, extract the session's _csrf + the current field
    # values, resubmit them unchanged WITH _csrf (a valid no-op edit), and assert a NEW
    # admin.action{form-submit} row landed on the shared log. Remote forms in the split
    # are read-only, so this parity leg is the only place form-submit is exercised.
    echo "[M3b] submit the LOCAL apikeys edit form WITH _csrf -> a new admin.action form-submit row"
    M3B_BEFORE="$(pg "SELECT count(*) FROM asyncevents.events WHERE topic='admin.action' AND payload->>'action'='form-submit';" | tr -d '[:space:]')"
    M3B_PAGE="$(curl -s -b "$MONO_JAR" "http://localhost:$A_PORT/admin/api-keys")"
    M3B_CSRF="$(printf '%s' "$M3B_PAGE" | grep -o 'name="_csrf" value="[^"]*"' | head -1 | sed 's/.*value="//;s/"$//')"
    M3B_ARGS=(--data-urlencode "_csrf=$M3B_CSRF")
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        fname="$(printf '%s' "$line" | sed 's/.*name="\([^"]*\)".*/\1/')"
        fval="$(printf '%s' "$line" | sed 's/.*value="\([^"]*\)".*/\1/')"
        M3B_ARGS+=(--data-urlencode "$fname=$fval")
    done < <(printf '%s' "$M3B_PAGE" | grep -o '<input type="text" name="[^"]*" value="[^"]*">')
    M3B_CODE="$(curl -s -o /dev/null -w '%{http_code}' -b "$MONO_JAR" -X POST "http://localhost:$A_PORT/admin/api-keys" "${M3B_ARGS[@]}")"
    M3B_AFTER="$(pg "SELECT count(*) FROM asyncevents.events WHERE topic='admin.action' AND payload->>'action'='form-submit';" | tr -d '[:space:]')"
    echo "    -> csrf=$(echo "$M3B_CSRF" | cut -c1-8)... submit HTTP $M3B_CODE ; form-submit rows ${M3B_BEFORE:-?} -> ${M3B_AFTER:-?}"
    if { [ "$M3B_CODE" = "303" ] || [ "$M3B_CODE" = "200" ]; } && [ -n "$M3B_CSRF" ] && [ "${M3B_AFTER:-0}" -gt "${M3B_BEFORE:-0}" ]; then
        pass "monolith LOCAL apikeys form-submit -> new admin.action{form-submit} on the shared log (durable trail in the co-hosting process)"
    else
        fail "monolith form-submit expected a new admin.action row (submit 303/200 + count up), got HTTP $M3B_CODE csrf='$M3B_CSRF' ${M3B_BEFORE:-?}->${M3B_AFTER:-?}"
    fi
else
    fail "monolith (server) never became healthy on :$A_PORT"
fi
teardown

if [ -z "$STOP_FORCED" ]; then
    pass "[W2 monolith graceful shutdown] SIGTERM drained without KILL fallback"
else
    fail "[W2 monolith graceful shutdown] monolith required KILL fallback"
fi
# W2b: symmetric with W1b -- the monolith's own SIGTERM handler must exit 0, not just
# exit within the grace window.
if [ -z "$STOP_NONZERO" ]; then
    pass "[W2b monolith clean-exit] monolith exited 0 after SIGTERM"
else
    fail "[W2b monolith clean-exit] monolith exited non-zero after SIGTERM"
fi

echo "============================================"
if [ "$FAILS" -eq 0 ]; then
    echo "SPLIT PROOF: PASS (all assertions held on the twelve-process split + monolith parity)"
    exit 0
else
    echo "SPLIT PROOF: FAIL ($FAILS assertion(s) failed)"
    exit 1
fi
