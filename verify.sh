#!/usr/bin/env bash
# verify.sh -- the umbrella verification gate for the rust-sketch (Steps 12, 14a).
#
# Usage:
#   ./verify.sh                # --fast: blocking stages only (default)
#   ./verify.sh --fast         # same as default
#   ./verify.sh --all          # + advisory: public-api, fuzz
#   ./verify.sh --slow         # + cargo-mutants mutation testing (very slow)
#   ./verify.sh --all --strict # advisory failures ALSO flip the exit code
#   ./verify.sh --all --no-install  # never auto-install a missing CLI (it SKIPs)
#
# BLOCKING (always runs):
#   1. build       cargo build --workspace
#   2. clippy      cargo clippy --workspace --all-targets -- -D warnings
#   3. test        cargo test --workspace (unit + proptest properties, see
#                  core/outbox/src/tests.rs, core/edge/src/{frame,codec,server}_tests.rs)
#   4. cargo-audit cargo audit against the RustSec advisory DB (auto-installs
#                  cargo-audit; SKIPs if the advisory DB fetch fails offline)
#   5. fortress    every domain module builds as its own -svc + archcheck dependency law
#   6. split-proof ./split-proof.sh -- the eleven-process topology proof
#
# ADVISORY (--all):
#   7. public-api  cargo-public-api additive-only diff of the api/*api and
#                  api/*events contract crates vs HEAD (apidiff parity; needs a
#                  nightly toolchain for rustdoc JSON -- auto-installed, SKIPs
#                  cleanly if unavailable)
#   8. fuzz        cargo-fuzz targets in core/edge/fuzz/ (frame_decode, wire_decode),
#                  10s each. SKIPs if cargo-fuzz can't execute on this platform
#                  (Windows lacks the libFuzzer sanitizer runtime as of this writing --
#                  the targets still build/check and are exercised for real on Linux/CI)
#
# SLOW (--slow):
#   9. mutants     cargo-mutants over the pure foundation crates (edge, gateway,
#                  outbox, registry, bus)
#
# topiccheck (the linkme-based bus.Define/on-subscriber-coverage equivalent) is a
# DELIBERATE placeholder -- landing in a follow-up step once the module set it
# audits is final. See the marked slot below.
#
# Prints a PASS/FAIL/SKIP summary and exits non-zero iff a BLOCKING stage failed (or
# ANY stage failed under --strict). Deliberately NOT `set -e` in the run phase: a
# failing stage must not abort the runner.
#
# ASCII only (no em-dashes) so the sibling verify.ps1 stays byte-parallel and
# PowerShell 5.1 never chokes.
set -uo pipefail
cd "$(dirname "$0")"

# --- Flags -------------------------------------------------------------------
LEVEL="fast"
STRICT=0
INSTALL=1
for arg in "$@"; do
    case "$arg" in
        --fast) LEVEL="fast" ;;
        --all) LEVEL="all" ;;
        --slow) LEVEL="slow" ;;
        --strict) STRICT=1 ;;
        --no-install) INSTALL=0 ;;
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

# api/*api and api/*events contract crates (the additive-only guard's scope) --
# mirrors experiments/go-sketch/verify.sh's CONTRACT_PKGS, one-to-one by domain.
PUBLIC_API_CRATES=(
    accountsevents
    accountsapi
    charactersevents
    charactersapi
    inventoryapi
    matchevents
    schedulerevents
    adminapi
)

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
    cargo build -p server -p characters-svc -p inventory-svc -p gateway-svc -p config-svc -p accounts-svc -p admin-svc -p audit-svc -p scheduler-svc -p match-svc -p rating-svc -p leaderboard-svc \
        && cargo run -q -p archcheck
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

# --- Advisory stage: public-api (apidiff parity, additive-only guard) --------
# Snapshots each contract crate's public API at HEAD (via a detached git worktree --
# same technique as Go's apidiff stage) and diffs it against the current working
# tree. `cargo public-api -s` prints a stable, sorted plain-text item list; any line
# REMOVED from the base (present at HEAD, gone now) means a symbol vanished or its
# signature changed -- both break a consumer, so that's an INCOMPATIBLE finding.
# Pure additions (`+`-only lines) are fine. Needs a nightly toolchain for rustdoc
# JSON; cargo-public-api itself shells out to it, no `+nightly` needed on our end.
ensure_public_api_tooling() {
    if ! rustup toolchain list 2>/dev/null | grep -q '^nightly'; then
        if [ "$INSTALL" -eq 0 ]; then return 1; fi
        echo "installing nightly toolchain (for rustdoc JSON) ..."
        rustup toolchain install nightly --profile minimal >/dev/null 2>&1
    fi
    rustup toolchain list 2>/dev/null | grep -q '^nightly' || return 1
    ensure_tool cargo-public-api cargo +nightly install cargo-public-api --locked
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
    local wt; wt="$(mktemp -d)"; rmdir "$wt"
    if ! git worktree add --detach "$wt" HEAD >>"$log" 2>&1; then
        echo "  FAIL (git worktree add failed, see run/verify/public-api.log)"
        add_result public-api FAIL false
        return
    fi
    local incompat=0 pkg base cur diffout
    for pkg in "${PUBLIC_API_CRATES[@]}"; do
        base="$VERIFY_DIR/public-api-base-$pkg.txt"
        cur="$VERIFY_DIR/public-api-cur-$pkg.txt"
        diffout="$VERIFY_DIR/public-api-diff-$pkg.txt"
        ( cd "$wt" && cargo public-api -p "$pkg" -s --color=never ) >"$base" 2>>"$log" || true
        cargo public-api -p "$pkg" -s --color=never >"$cur" 2>>"$log" || true
        diff -u "$base" "$cur" >"$diffout" || true
        # A "-" line NOT starting with "---" (the unified-diff file header) is a
        # symbol removed or changed since HEAD -- the incompatible case.
        if grep -qE '^-[^-]' "$diffout"; then
            echo "  $pkg: INCOMPATIBLE (see run/verify/public-api-diff-$pkg.txt)" | tee -a "$log"
            incompat=1
        else
            echo "  $pkg: ok" >>"$log"
        fi
    done
    git worktree remove --force "$wt" >>"$log" 2>&1 || true
    if [ "$incompat" -eq 0 ]; then
        echo "  PASS"
        add_result public-api PASS false
    else
        echo "  FAIL (incompatible API changes, see run/verify/public-api-diff-*.txt)"
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
    if cargo mutants -p edge -p gateway -p outbox -p registry -p bus --timeout 300 >"$log" 2>&1; then
        echo "  PASS"
        add_result mutants PASS false
    else
        echo "  FAIL (see run/verify/mutants.log)"
        add_result mutants FAIL false
    fi
}

# topiccheck (linkme-based bus.Define/on-subscriber-coverage equivalent): landing in
# a follow-up step (Step 14, part B) once the module set it audits is final. This
# comment marks where its stage call slots in, alongside the other advisory stages.

# --- Run -----------------------------------------------------------------------
simple_stage build   true cargo build --workspace
simple_stage clippy  true cargo clippy --workspace --all-targets -- -D warnings
simple_stage test    true cargo test --workspace
cargo_audit_stage
simple_stage fortress    true fortress
simple_stage split-proof true bash ./split-proof.sh

if [ "$RUN_ADVISORY" -eq 1 ]; then
    public_api_stage
    fuzz_stage
    # <-- topiccheck_stage slots in here once it lands (Step 14, part B) -->
fi
if [ "$RUN_SLOW" -eq 1 ]; then
    mutants_stage
fi

# --- Summary ---------------------------------------------------------------
echo ""
echo "=== verify summary ==="
printf "%-14s | %-6s | %-8s\n" "Stage" "Status" "Blocking"
printf "%-14s-+-%-6s-+-%-8s\n" "--------------" "------" "--------"
fail=0
for i in "${!STAGE_NAMES[@]}"; do
    n="${STAGE_NAMES[$i]}"; s="${STAGE_STATUS[$i]}"; b="${STAGE_BLOCKING[$i]}"
    printf "%-14s | %-6s | %-8s\n" "$n" "$s" "$b"
    if [ "$s" = "FAIL" ] && { [ "$b" = "true" ] || [ "$STRICT" -eq 1 ]; }; then fail=1; fi
done
echo ""
if [ "$fail" -eq 0 ]; then echo "VERIFY: OK"; else echo "VERIFY: FAIL"; fi
exit "$fail"
