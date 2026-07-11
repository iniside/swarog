#!/usr/bin/env bash
# verify.sh -- the umbrella verification gate for the rust-sketch (Steps 12, 14a).
#
# Usage:
#   ./verify.sh                # --fast: blocking stages only (default)
#   ./verify.sh --fast         # same as default
#   ./verify.sh --all          # + advisory: public-api, fuzz, csharp-client, topiccheck
#   ./verify.sh --slow         # + cargo-mutants mutation testing (very slow)
#   ./verify.sh --all --strict # advisory failures ALSO flip the exit code
#   ./verify.sh --all --no-install  # never auto-install a missing CLI (it SKIPs)
#   ./verify.sh --bless-public-api  # regenerate the committed public-api snapshots and exit
#   ./verify.sh --bless-contract-golden  # regenerate the committed contract-golden and exit
#
# BLOCKING (always runs):
#   1. build         cargo build --workspace
#   2. clippy        cargo clippy --workspace --all-targets -- -D warnings
#   3. test          cargo test --workspace (unit + proptest properties, see
#                    core/asyncevents/src/store_tests.rs, core/edge/src/{frame,codec,server}_tests.rs)
#   4. cargo-audit   cargo audit against the RustSec advisory DB (auto-installs
#                    cargo-audit; SKIPs if the advisory DB fetch fails offline)
#   5. fortress      every domain module builds as its own -svc + archcheck dependency law
#   6. routecheck    static monolith/split front-door route parity: builds every
#                    process of both profiles (register->init, lazy pool, no live DB)
#                    under both env-gate configs and fails on any structural
#                    divergence between the two topologies' route sets (the
#                    inventory-dev-grant bug-class net)
#   7. codegen-fresh regenerates clients/csharp/Generated via csharp-client-gen and
#                    diffs against the working tree -- FAILs if a contract changed
#                    without regenerating. Pure Rust + git, no dotnet/QUIC, runs
#                    everywhere.
#   8. contract-golden  the VALUE-level contract baseline (topiccheck contract-golden):
#                    every bus::define's topic/version/history and every generated
#                    Operation's verb/path/auth/success/retry_mode, diffed against the
#                    COMMITTED golden in docs/reference/contract-golden/contracts.txt
#                    (values cargo-public-api structurally cannot see; re-bless
#                    intentional changes with --bless-contract-golden)
#   9. split-proof   ./split-proof.sh -- the eleven-process topology proof
#
# ADVISORY (--all):
#  10. public-api    cargo-public-api diff of the api/*api and api/*events contract
#                    crates vs COMMITTED snapshots in docs/reference/public-api-baseline/
#                    (crate list derived from the filesystem; ANY diff FAILs, removed
#                    symbols flagged BREAKING, added ADDITIVE; re-bless with
#                    --bless-public-api). Needs a nightly toolchain for rustdoc JSON --
#                    auto-installed, SKIPs cleanly if unavailable.
#  11. fuzz          cargo-fuzz targets in core/edge/fuzz/ (frame_decode, wire_decode),
#                    10s each. SKIPs if cargo-fuzz can't execute on this platform
#                    (Windows lacks the libFuzzer sanitizer runtime as of this writing --
#                    the targets still build/check and are exercised for real on Linux/CI)
#  12. csharp-client builds clients/csharp (gbclient) and drives it over pure QUIC
#                    against a self-contained monolith: raw Unauthorized/NotFound
#                    negatives + a typed register->create->list flow. SKIPs if dotnet
#                    is absent or QuicConnection.IsSupported is false (msquic missing).
#  13. topiccheck    builds the monolith module set with a recording bus transport and
#                    fails (under --strict) on any bus::define'd topic with no subscriber
#                    (the Rust redesign of Go's whole-program topiccheck)
#
# SLOW (--slow):
#   14. mutants    cargo-mutants over the pure foundation crates (edge, gateway,
#                  asyncevents, registry, bus)
#
# Prints a PASS/FAIL/SKIP summary and exits non-zero iff a BLOCKING stage failed (or
# ANY stage failed under --strict). Deliberately NOT `set -e` in the run phase: a
# failing stage must not abort the runner.
#
# ASCII only (no em-dashes) so the sibling verify.ps1 stays byte-parallel and
# PowerShell 5.1 never chokes.
set -uo pipefail
cd "$(dirname "$0")"

# --- Live log tee: every invocation writes its full console output to a timestamped
# log file (in addition to the console), with the log path printed FIRST so a human or
# an agent can tail it live.
mkdir -p run/logs
LOG="run/logs/verify-$(date +%Y%m%d-%H%M%S).log"
echo "[log] $(pwd)/$LOG"
exec > >(tee -a "$LOG") 2>&1

# --- Flags -------------------------------------------------------------------
LEVEL="fast"
STRICT=0
INSTALL=1
BLESS_PUBLIC_API=0
BLESS_CONTRACT_GOLDEN=0
for arg in "$@"; do
    case "$arg" in
        --fast) LEVEL="fast" ;;
        --all) LEVEL="all" ;;
        --slow) LEVEL="slow" ;;
        --strict) STRICT=1 ;;
        --no-install) INSTALL=0 ;;
        --bless-public-api) BLESS_PUBLIC_API=1 ;;
        --bless-contract-golden) BLESS_CONTRACT_GOLDEN=1 ;;
        *) echo "unknown arg: $arg" >&2; exit 2 ;;
    esac
done

RUN_ADVISORY=0
RUN_SLOW=0
case "$LEVEL" in
    all)  RUN_ADVISORY=1 ;;
    slow) RUN_ADVISORY=1; RUN_SLOW=1 ;;
esac

RUN_DIR="run"
VERIFY_DIR="$RUN_DIR/verify"
mkdir -p "$VERIFY_DIR"

# Directory holding the committed public-api snapshots (the trusted baseline).
PUBLIC_API_BASELINE_DIR="docs/reference/public-api-baseline"

# public_api_crates -- the public-api gate's scope, DERIVED from the filesystem: the
# `name = "..."` of every api/*/api/Cargo.toml and api/*/events/Cargo.toml. A new domain
# joins the gate automatically; rpc crates stay out by construction (not globbed).
public_api_crates() {
    local f name
    for f in api/*/api/Cargo.toml api/*/events/Cargo.toml; do
        [ -f "$f" ] || continue
        name="$(sed -n 's/^name = "\(.*\)"/\1/p' "$f" | head -1)"
        [ -n "$name" ] && echo "$name"
    done
}

# fortress_crates -- the fortress stage's build scope, DERIVED from the filesystem
# (twin of public_api_crates): the `name = "..."` of every cmd/*-svc/Cargo.toml, plus
# the monolith `server`. A new svc crate joins the fortress build automatically; the
# module-set-membership drift itself is guarded separately by
# checkmodules::split_fleet_matches_cmd_dirs (tools/checkmodules).
fortress_crates() {
    local f name
    echo "server"
    for f in cmd/*-svc/Cargo.toml; do
        [ -f "$f" ] || continue
        name="$(sed -n 's/^name = "\(.*\)"/\1/p' "$f" | head -1)"
        [ -n "$name" ] && echo "$name"
    done
}

# --- Result accumulation ------------------------------------------------------
STAGE_NAMES=()
STAGE_STATUS=()
STAGE_BLOCKING=()

add_result() {
    STAGE_NAMES+=("$1")
    STAGE_STATUS+=("$2")
    STAGE_BLOCKING+=("$3")
}

# ensure_tool BIN CMD... -- returns 0 if BIN is available (installing via CMD... if
# missing and installs are enabled), 1 if unavailable (stage SKIPs). Port of Go's
# verify.sh ensure_tool, generalized from `go install` to an arbitrary install command.
ensure_tool() {
    local bin="$1"; shift
    if command -v "$bin" >/dev/null 2>&1; then return 0; fi
    if [ "$INSTALL" -eq 0 ]; then return 1; fi
    echo "installing $bin ($*) ..."
    "$@" >/dev/null 2>&1
    hash -r
    command -v "$bin" >/dev/null 2>&1
}

# simple_stage NAME BLOCKING CMD... -- runs CMD, logging to run/verify/NAME.log,
# recording PASS on exit 0 else FAIL.
simple_stage() {
    local name="$1" blocking="$2"; shift 2
    local log="$VERIFY_DIR/$name.log"
    echo "== $name =="
    if "$@" >"$log" 2>&1; then
        echo "  PASS"
        add_result "$name" PASS "$blocking"
    else
        echo "  FAIL (see run/verify/$name.log)"
        add_result "$name" FAIL "$blocking"
    fi
}

# --- Blocking stage: fortress (Step 5) ---------------------------------------
# Every domain module must compile + boot as its own -svc binary, and archcheck
# enforces the dependency law (no module->module / module->foreign-rpc edge, no
# resurrected Option<edge::Server> under modules/).
fortress() {
    local -a pkg_args=()
    local crate
    while IFS= read -r crate; do
        pkg_args+=(-p "$crate")
    done < <(fortress_crates)
    cargo build "${pkg_args[@]}" \
        && cargo run -q -p archcheck \
        && cargo run -q -p requirecheck -- --strict \
        && cargo run -q -p topiccheck -- --durability-strict
}

# --- Blocking stage: cargo-audit ----------------------------------------------
# RUSTSEC-2023-0071 (rsa 0.9.10, Marvin Attack timing side-channel): a dev-only
# dependency of modules/accounts (mints RSA-signed test JWTs for the OIDC verifier's
# fixtures), never linked into a shipped binary. Upstream: "No fixed upgrade is
# available!" as of this writing -- accepted risk, revisit when a fix ships.
CARGO_AUDIT_IGNORE=(RUSTSEC-2023-0071)

cargo_audit_stage() {
    local log="$VERIFY_DIR/cargo-audit.log"
    echo "== cargo-audit =="
    if ! ensure_tool cargo-audit cargo install cargo-audit --locked --version 0.22.2; then
        echo "  SKIP (cargo-audit unavailable)"
        echo "cargo-audit unavailable (missing and --no-install, or install failed)" >"$log"
        add_result cargo-audit SKIP true
        return
    fi
    local ignore_args=() id
    for id in "${CARGO_AUDIT_IGNORE[@]}"; do ignore_args+=(--ignore "$id"); done
    if cargo audit "${ignore_args[@]}" >"$log" 2>&1; then
        echo "  PASS"
        add_result cargo-audit PASS true
    elif grep -qiE "error loading advisory database|failed to fetch|could not fetch|couldn'?t resolve host|network is unreachable|failed to clone|unable to connect" "$log"; then
        echo "  SKIP (advisory DB fetch failed -- offline?)"
        add_result cargo-audit SKIP true
    else
        echo "  FAIL (see run/verify/cargo-audit.log)"
        add_result cargo-audit FAIL true
    fi
}

# --- Advisory stage: public-api (committed-snapshot baseline gate) ------------
# Diffs each contract crate's current public API (`cargo +nightly public-api -s`, a
# stable sorted plain-text item list) against a COMMITTED snapshot under
# docs/reference/public-api-baseline/<crate>.txt. ANY difference FAILs: removed lines
# are flagged BREAKING (a symbol vanished or a signature changed -- breaks a consumer),
# added lines ADDITIVE. The operator reviews the printed diff and, if intentional
# (additive or a versioned new contract), re-blesses via `--bless-public-api`. Unlike
# the old HEAD-worktree diff (which guarded only uncommitted changes and so caught
# nothing in this commit-straight-to-master repo), a committed snapshot catches
# committed breaks. cargo-public-api is version-pinned (rustdoc-JSON output is
# version-sensitive) and the pin is recorded in each snapshot's header. RESIDUAL RISK:
# rustdoc-JSON formatting can still drift across the *nightly toolchain itself* (the
# nightly date is deliberately not pinned -- a pinned nightly bit-rots); such a drift
# shows as a formatting-only diff, re-blessed after confirming no symbol changes. This
# stays advisory (blocking only under --strict).

# bless_public_api -- regenerate every committed snapshot from the current API and exit.
# The first-run case (baseline dir absent) is fine: the dir is created here.
bless_public_api() {
    echo "== public-api bless =="
    if ! ensure_public_api_tooling; then
        echo "  cannot bless: nightly toolchain / cargo-public-api unavailable" >&2
        return 1
    fi
    mkdir -p "$PUBLIC_API_BASELINE_DIR"
    local pver header pkg snap out rc=0
    pver="$(cargo public-api --version 2>/dev/null | awk '{print $2}')"
    header="# cargo-public-api $pver -- regenerate via ./verify.sh --bless-public-api"
    for pkg in $(public_api_crates); do
        snap="$PUBLIC_API_BASELINE_DIR/$pkg.txt"
        if out="$(cargo +nightly public-api -p "$pkg" -s --color=never 2>/dev/null)"; then
            { echo "$header"; printf '%s\n' "$out"; } >"$snap"
            echo "  blessed $pkg"
        else
            echo "  $pkg: cargo public-api FAILED" >&2
            rc=1
        fi
    done
    return "$rc"
}
ensure_public_api_tooling() {
    if ! rustup toolchain list 2>/dev/null | grep -q '^nightly'; then
        if [ "$INSTALL" -eq 0 ]; then return 1; fi
        echo "installing nightly toolchain (for rustdoc JSON) ..."
        rustup toolchain install nightly --profile minimal >/dev/null 2>&1
    fi
    rustup toolchain list 2>/dev/null | grep -q '^nightly' || return 1
    # Pin cargo-public-api (cargo-audit precedent, verify.sh:156): rustdoc-JSON output
    # is version-sensitive, so an unpinned bump would spuriously diff every snapshot.
    ensure_tool cargo-public-api cargo +nightly install cargo-public-api --locked --version 0.52.0
}

# orphan_baseline_findings -- Step 6b: a deleted contract crate leaves its committed
# docs/reference/public-api-baseline/<crate>.txt behind forever, since public_api_stage
# only ITERATES the live crate list (never the baseline dir) -- a snapshot for a crate
# that no longer exists would silently never be checked or cleaned up again. Diffs the
# baseline dir's file stems against the live `public_api_crates()` set; prints one
# finding per orphan naming the file. Takes the live-crate list as $1 (newline-separated)
# so callers compute it once.
orphan_baseline_findings() {
    local live="$1" f stem found=0 live_csv
    live_csv="$(tr '\n' ',' <<<"$live" | sed 's/,$//')"
    [ -d "$PUBLIC_API_BASELINE_DIR" ] || return 0
    for f in "$PUBLIC_API_BASELINE_DIR"/*.txt; do
        [ -f "$f" ] || continue
        stem="$(basename "$f" .txt)"
        if ! grep -qxF "$stem" <<<"$live"; then
            echo "  ORPHAN baseline $f -- no live crate named \"$stem\" (public_api_crates()" \
                "is: $live_csv) -- delete this snapshot, it belongs to a removed contract crate"
            found=1
        fi
    done
    return "$found"
}

public_api_stage() {
    local log="$VERIFY_DIR/public-api.log"; : >"$log"
    echo "== public-api =="
    if ! ensure_public_api_tooling; then
        echo "  SKIP (nightly toolchain / cargo-public-api unavailable)"
        echo "nightly toolchain or cargo-public-api unavailable" >"$log"
        add_result public-api SKIP false
        return
    fi
    local diff=0 toolfail=0 pkg snap expected cur diffout
    local live_crates; live_crates="$(public_api_crates)"
    if ! orphan_baseline_findings "$live_crates" | tee -a "$log"; then
        diff=1
    fi
    for pkg in $(public_api_crates); do
        snap="$PUBLIC_API_BASELINE_DIR/$pkg.txt"
        cur="$VERIFY_DIR/public-api-cur-$pkg.txt"
        expected="$VERIFY_DIR/public-api-base-$pkg.txt"
        diffout="$VERIFY_DIR/public-api-diff-$pkg.txt"
        # Tool errors FAIL the stage -- no `|| true` swallowing a crash into an empty diff.
        if ! cargo +nightly public-api -p "$pkg" -s --color=never >"$cur" 2>>"$log"; then
            echo "  $pkg: cargo public-api FAILED (see run/verify/public-api.log)" | tee -a "$log"
            toolfail=1
            continue
        fi
        if [ ! -f "$snap" ]; then
            echo "  $pkg: MISSING baseline snapshot -- run ./verify.sh --bless-public-api" | tee -a "$log"
            diff=1
            continue
        fi
        # Strip the pinned-version header before comparing against live output.
        grep -v '^# cargo-public-api' "$snap" >"$expected"
        if diff -u "$expected" "$cur" >"$diffout"; then
            echo "  $pkg: ok" >>"$log"
        else
            echo "  $pkg: DIFFERS from committed baseline (see run/verify/public-api-diff-$pkg.txt)" | tee -a "$log"
            # Removed lines (present in baseline, gone now) = BREAKING; added = ADDITIVE.
            grep -E '^-[^-]' "$diffout" | sed 's/^-/  BREAKING -/' | tee -a "$log"
            grep -E '^\+[^+]' "$diffout" | sed 's/^+/  ADDITIVE +/' | tee -a "$log"
            diff=1
        fi
    done
    if [ "$toolfail" -eq 1 ]; then
        echo "  FAIL (cargo public-api errored, see run/verify/public-api.log)"
        add_result public-api FAIL false
    elif [ "$diff" -eq 0 ]; then
        echo "  PASS"
        add_result public-api PASS false
    else
        echo "  FAIL: a crate differs from its committed baseline -- review the diff; if"
        echo "        intentional (additive or a versioned new contract), regenerate via"
        echo "        ./verify.sh --bless-public-api (or -BlessPublicApi). If only formatting"
        echo "        changed (toolchain drift), re-bless after confirming no symbol changes."
        add_result public-api FAIL false
    fi
}

# --- Advisory stage: fuzz (cargo-fuzz, core/edge/fuzz/) ----------------------
# core/edge/fuzz/fuzz_targets/{frame_decode,wire_decode}.rs port the corpus Go's
# edge/fuzz_test.go exercised (readFrame never panics on arbitrary/truncated/
# oversized input; frame_bytes/read_frame round-trip; the JSON codec never panics
# decoding arbitrary bytes into the wire envelope). cargo-fuzz needs nightly +
# libFuzzer sanitizer coverage instrumentation; as of this writing that runtime is
# not resolvable on this Windows machine (the built binary exits
# STATUS_DLL_NOT_FOUND) even though the targets build/typecheck fine -- this stage
# detects that and SKIPs with a clear note rather than reporting a false FAIL. The
# targets are committed so Linux/CI can run them for real.
FUZZ_TARGETS=(frame_decode wire_decode)

fuzz_stage() {
    local log="$VERIFY_DIR/fuzz.log"; : >"$log"
    echo "== fuzz =="
    if ! ensure_tool cargo-fuzz cargo install cargo-fuzz --locked; then
        echo "  SKIP (cargo-fuzz unavailable)"
        echo "cargo-fuzz unavailable" >"$log"
        add_result fuzz SKIP false
        return
    fi
    local ran=0 anyfail=0 platform_blocked=0 t
    for t in "${FUZZ_TARGETS[@]}"; do
        echo "--- $t ---" >>"$log"
        if ( cd core/edge && cargo +nightly fuzz run "$t" -- -max_total_time=10 -runs=100000 ) >>"$log" 2>&1; then
            echo "  $t: ok"
            ran=1
        elif grep -qiE "0xc0000135|DLL_NOT_FOUND|status_dll_not_found|is not installed" "$log"; then
            echo "  $t: SKIP (cannot execute the libFuzzer binary on this platform)"
            platform_blocked=1
        else
            echo "  $t: FAIL"
            anyfail=1
        fi
    done
    if [ "$anyfail" -eq 1 ]; then
        echo "  FAIL (see run/verify/fuzz.log)"
        add_result fuzz FAIL false
    elif [ "$ran" -eq 0 ] && [ "$platform_blocked" -eq 1 ]; then
        echo "  SKIP (fuzz targets present but cannot execute on this platform)"
        add_result fuzz SKIP false
    else
        echo "  PASS"
        add_result fuzz PASS false
    fi
}

# --- Slow stage: cargo-mutants mutation testing -------------------------------
# The pure foundation crates -- the split-proof and unit/proptest suites are the
# tests this is meant to grade. Very slow (each surviving/killed mutant is a full
# build+test cycle over ~268 mutants as of this writing); only run under --slow.
mutants_stage() {
    local log="$VERIFY_DIR/mutants.log"; : >"$log"
    echo "== mutants =="
    if ! ensure_tool cargo-mutants cargo install cargo-mutants --locked; then
        echo "  SKIP (cargo-mutants unavailable)"
        add_result mutants SKIP false
        return
    fi
    if cargo mutants -p edge -p gateway -p asyncevents -p registry -p bus --timeout 300 >"$log" 2>&1; then
        echo "  PASS"
        add_result mutants PASS false
    else
        echo "  FAIL (see run/verify/mutants.log)"
        add_result mutants FAIL false
    fi
}

# --- Blocking stage: codegen-fresh (generated C# client drift) ---------------
# tools/csharp-client-gen scrapes the player-reachable api/*api crates and emits
# clients/csharp/Generated/*.cs deterministically. If the working tree's generated
# files disagree with a fresh run, someone changed a contract (or the generator)
# without regenerating -- FAIL. Pure Rust + git, no dotnet/QUIC, so it runs on every
# machine regardless of msquic availability (unlike the csharp-client stage below).
codegen_fresh() {
    cargo run -q -p csharp-client-gen -- --out clients/csharp/Generated \
        && git diff --exit-code -- clients/csharp/Generated
}

# --- Advisory stage: csharp-client (external C# QUIC client, SKIP-aware) -----
# Builds the hand-written C# transport/CLI (clients/csharp, gbclient) and drives it
# against a self-contained monolith (NOT split-proof's processes -- those are already
# torn down by the time this stage runs) over pure QUIC, proving an EXTERNAL client
# (no Rust linkage) can dial the player plane, get Unauthorized/NotFound where
# expected, and complete a full register->create->list flow through the generated
# typed client. SKIPs (not FAILs) when dotnet is absent or when the first scenario's
# raw exit code is 3 (QuicConnection.IsSupported false -- msquic missing, the
# documented Linux/CI case; see docs/reference/csharp-client.md).
CSHARP_PORT=8099
CSHARP_PLAYER_PORT=9100
CSHARP_DEFAULT_DSN="postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable"

csharp_kill_stragglers() {
    if command -v taskkill >/dev/null 2>&1; then
        taskkill //F //IM server.exe >/dev/null 2>&1 || true
    fi
    pkill -f "target/debug/server" 2>/dev/null || true
}

csharp_stage() {
    local log="$VERIFY_DIR/csharp-client.log"; : >"$log"
    echo "== csharp-client =="
    if ! command -v dotnet >/dev/null 2>&1; then
        echo "  SKIP (dotnet unavailable)"
        echo "dotnet unavailable" >"$log"
        add_result csharp-client SKIP false
        return
    fi

    echo "--- dotnet build clients/csharp -c Release ---" >>"$log"
    if ! dotnet build clients/csharp -c Release >>"$log" 2>&1; then
        echo "  FAIL (dotnet build, see run/verify/csharp-client.log)"
        add_result csharp-client FAIL false
        return
    fi

    echo "--- cargo build -p server ---" >>"$log"
    if ! cargo build -p server >>"$log" 2>&1; then
        echo "  FAIL (cargo build -p server, see run/verify/csharp-client.log)"
        add_result csharp-client FAIL false
        return
    fi

    local dsn="${DATABASE_URL:-$CSHARP_DEFAULT_DSN}"
    local exe=""
    [ -f "target/debug/server.exe" ] && exe=".exe"

    csharp_kill_stragglers
    echo "--- starting self-contained monolith on :$CSHARP_PORT, player QUIC :$CSHARP_PLAYER_PORT (ephemeral CA -> --insecure, APIKEYS_DEV_SEED=1, dev flags on) ---" >>"$log"
    # Dev conveniences are now explicit opt-ins (fail-closed defaults): the gbclient flow
    # does register->create->list, so enable ACCOUNTS_DEV_AUTH (+ INVENTORY_DEV_GRANT for
    # symmetry). The admin module now boots with ZERO admin users (a warned no-op, session
    # auth), so no ADMIN_USER/ADMIN_PASS is needed -- this flow never touches /admin.
    env PORT=":$CSHARP_PORT" DATABASE_URL="$dsn" PLAYER_EDGE_ADDR=":$CSHARP_PLAYER_PORT" \
        APIKEYS_DEV_SEED=1 ACCOUNTS_DEV_AUTH=1 INVENTORY_DEV_GRANT=1 \
        "target/debug/server$exe" >>"$log" 2>&1 &
    local pid=$!

    local tries=60 healthy=0
    while [ "$tries" -gt 0 ]; do
        if curl -fsS -o /dev/null "http://localhost:$CSHARP_PORT/healthz" 2>/dev/null; then healthy=1; break; fi
        tries=$((tries - 1)); sleep 0.5
    done
    if [ "$healthy" -ne 1 ]; then
        echo "  FAIL (monolith never became healthy, see run/verify/csharp-client.log)"
        kill "$pid" 2>/dev/null || true
        csharp_kill_stragglers
        add_result csharp-client FAIL false
        return
    fi

    gbclient() {
        dotnet run --project clients/csharp -c Release --no-build -- "$@"
    }

    local status=PASS

    echo "--- [C1] QUIC probe: raw --insecure --api-key dev-key-client leaderboard.topScores ---" >>"$log"
    local c1_out c1_rc
    c1_out="$(gbclient raw --addr "127.0.0.1:$CSHARP_PLAYER_PORT" --insecure --api-key dev-key-client leaderboard.topScores 2>>"$log")"
    c1_rc=$?
    echo "    -> rc=$c1_rc  $c1_out" >>"$log"
    if [ "$c1_rc" -eq 3 ]; then
        echo "  SKIP (QUIC/msquic unsupported on this platform -- QuicConnection.IsSupported false)"
        kill "$pid" 2>/dev/null || true
        csharp_kill_stragglers
        add_result csharp-client SKIP false
        return
    fi
    if [ "$c1_rc" -ne 0 ]; then
        echo "    C1 FAIL: expected exit 0, got rc=$c1_rc" >>"$log"
        status=FAIL
    fi

    echo "--- [C2] raw --insecure --api-key dev-key-client characters.create, NO token -> exit 1 + Unauthorized ---" >>"$log"
    local c2_out c2_rc
    c2_out="$(gbclient raw --addr "127.0.0.1:$CSHARP_PLAYER_PORT" --insecure --api-key dev-key-client characters.create '{"name":"x","class":""}' 2>>"$log")"
    c2_rc=$?
    echo "    -> rc=$c2_rc  $c2_out" >>"$log"
    if [ "$c2_rc" -ne 1 ] || ! echo "$c2_out" | grep -q 'Unauthorized'; then
        echo "    C2 FAIL: expected exit 1 + Unauthorized, got rc=$c2_rc $c2_out" >>"$log"
        status=FAIL
    fi

    echo "--- [C3] raw --insecure --api-key dev-key-client --token bogus characters.ownerOf -> exit 1 + NotFound ---" >>"$log"
    local c3_out c3_rc
    c3_out="$(gbclient raw --addr "127.0.0.1:$CSHARP_PLAYER_PORT" --insecure --api-key dev-key-client --token bogus characters.ownerOf '{"character_id":"z"}' 2>>"$log")"
    c3_rc=$?
    echo "    -> rc=$c3_rc  $c3_out" >>"$log"
    if [ "$c3_rc" -ne 1 ] || ! echo "$c3_out" | grep -q 'NotFound'; then
        echo "    C3 FAIL: expected exit 1 + NotFound, got rc=$c3_rc $c3_out" >>"$log"
        status=FAIL
    fi

    echo "--- [C4] flow --insecure --api-key dev-key-client (typed client: register -> create -> list over pure QUIC) ---" >>"$log"
    local c4_rc
    gbclient flow --addr "127.0.0.1:$CSHARP_PLAYER_PORT" --insecure --api-key dev-key-client >>"$log" 2>&1
    c4_rc=$?
    echo "    -> rc=$c4_rc" >>"$log"
    if [ "$c4_rc" -ne 0 ]; then
        echo "    C4 FAIL: expected exit 0, got rc=$c4_rc" >>"$log"
        status=FAIL
    fi

    # ReportId is match.report's REQUIRED idempotency key; per-run-unique (nanos) because
    # match.matches persists across verify runs and a constant id would dedup C6's insert.
    local csharp_rid
    csharp_rid="$(date +%s%N)"

    echo "--- [C5] raw --insecure --api-key dev-key-client match.report -> exit 1 + Forbidden (client policy lacks match.report) ---" >>"$log"
    local c5_out c5_rc
    c5_out="$(gbclient raw --addr "127.0.0.1:$CSHARP_PLAYER_PORT" --insecure --api-key dev-key-client match.report "{\"ReportId\":\"c5-$csharp_rid\",\"Winner\":\"c5-winner\",\"Loser\":\"c5-loser\"}" 2>>"$log")"
    c5_rc=$?
    echo "    -> rc=$c5_rc  $c5_out" >>"$log"
    if [ "$c5_rc" -ne 1 ] || ! echo "$c5_out" | grep -q 'Forbidden'; then
        echo "    C5 FAIL: expected exit 1 + Forbidden, got rc=$c5_rc $c5_out" >>"$log"
        status=FAIL
    fi

    echo "--- [C6] raw --insecure --api-key dev-key-server match.report -> exit 0 (full policy allows it) ---" >>"$log"
    local c6_out c6_rc
    c6_out="$(gbclient raw --addr "127.0.0.1:$CSHARP_PLAYER_PORT" --insecure --api-key dev-key-server match.report "{\"ReportId\":\"c6-$csharp_rid\",\"Winner\":\"c6-winner\",\"Loser\":\"c6-loser\"}" 2>>"$log")"
    c6_rc=$?
    echo "    -> rc=$c6_rc  $c6_out" >>"$log"
    if [ "$c6_rc" -ne 0 ]; then
        echo "    C6 FAIL: expected exit 0, got rc=$c6_rc $c6_out" >>"$log"
        status=FAIL
    fi

    kill "$pid" 2>/dev/null || true
    csharp_kill_stragglers

    if [ "$status" = "PASS" ]; then
        echo "  PASS"
        add_result csharp-client PASS false
    else
        echo "  FAIL (see run/verify/csharp-client.log)"
        add_result csharp-client FAIL false
    fi
}

# --- Advisory stage: topiccheck (defined-vs-subscribed topic drift) -----------
# The Rust redesign of Go's whole-program topiccheck: `tools/topiccheck` builds the
# MONOLITH module set with a recording bus transport, runs the register+init lifecycle
# phases, and diffs the topics actually subscribed against the `bus::define`d ones.
# `--strict` makes it exit non-zero on any unsubscribed topic, so this stage FAILs on
# drift; advisory by default, blocking only under the umbrella `--strict`.
topiccheck_stage() {
    simple_stage topiccheck false cargo run -q -p topiccheck -- --strict
}

# --- Run -----------------------------------------------------------------------
if [ "$BLESS_PUBLIC_API" -eq 1 ]; then
    bless_public_api
    exit $?
fi
if [ "$BLESS_CONTRACT_GOLDEN" -eq 1 ]; then
    cargo run -q -p topiccheck -- contract-golden --bless
    exit $?
fi

simple_stage build   true cargo build --workspace
simple_stage clippy  true cargo clippy --workspace --all-targets -- -D warnings
simple_stage test    true cargo test --workspace
cargo_audit_stage
simple_stage fortress    true fortress
simple_stage routecheck  true cargo run -q -p routecheck
simple_stage codegen-fresh true codegen_fresh
simple_stage contract-golden true cargo run -q -p topiccheck -- contract-golden
simple_stage split-proof true cargo run -q -p splitproof

if [ "$RUN_ADVISORY" -eq 1 ]; then
    public_api_stage
    fuzz_stage
    csharp_stage
    topiccheck_stage
fi
if [ "$RUN_SLOW" -eq 1 ]; then
    mutants_stage
fi

# --- Summary ---------------------------------------------------------------
echo ""
echo "=== verify summary ==="
printf "%-16s | %-6s | %-8s\n" "Stage" "Status" "Blocking"
printf "%-16s-+-%-6s-+-%-8s\n" "----------------" "------" "--------"
fail=0
for i in "${!STAGE_NAMES[@]}"; do
    n="${STAGE_NAMES[$i]}"; s="${STAGE_STATUS[$i]}"; b="${STAGE_BLOCKING[$i]}"
    printf "%-16s | %-6s | %-8s\n" "$n" "$s" "$b"
    if [ "$s" = "FAIL" ] && { [ "$b" = "true" ] || [ "$STRICT" -eq 1 ]; }; then fail=1; fi
done
echo ""
if [ "$fail" -eq 0 ]; then echo "VERIFY: OK"; else echo "VERIFY: FAIL"; fi
exit "$fail"
