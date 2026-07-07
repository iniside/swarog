#!/usr/bin/env bash
# verify.sh - one-shot local verification gate. Runs every verification stage,
# keeps going after a failure, prints a summary table, and exits non-zero iff a
# BLOCKING stage failed (or ANY stage failed under --strict).
#
# Usage:
#   ./verify.sh                # --fast: blocking stages only (default)
#   ./verify.sh --fast         # same as default
#   ./verify.sh --all          # + advisory: test-race, fuzz, apidiff, topiccheck
#   ./verify.sh --slow         # + gremlins mutation testing (very slow)
#   ./verify.sh --all --strict # advisory failures ALSO flip the exit code
#   ./verify.sh --all --no-install  # never auto-install a missing CLI (it SKIPs)
#
# Behavioural twin of verify.ps1. Blocking stages: build, vet, golangci-lint,
# go-arch-lint, test, govulncheck. Advisory (--all): test-race, fuzz, apidiff,
# topiccheck. Slow (--slow): gremlins. Per-stage output goes to run/verify/<name>.log.
#
# Deliberately NOT `set -e` in the run phase: a failing stage must not abort the
# runner. Each stage records PASS/FAIL/SKIP and the summary decides the exit code.
set -uo pipefail
cd "$(dirname "$0")"

# --- Flags ------------------------------------------------------------------
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
RUN_GREMLINS=0
case "$LEVEL" in
    all)  RUN_ADVISORY=1 ;;
    slow) RUN_ADVISORY=1; RUN_GREMLINS=1 ;;
esac

RUN_DIR="run"
VERIFY_DIR="$RUN_DIR/verify"
mkdir -p "$VERIFY_DIR"

CONTRACT_PKGS=(
    gamebackend/modules/accounts/accountsevents
    gamebackend/modules/characters/charactersevents
    gamebackend/modules/match/matchevents
    gamebackend/modules/scheduler/schedulerevents
    gamebackend/modules/admin/adminapi
)

# --- Result accumulation ----------------------------------------------------
STAGE_NAMES=()
STAGE_STATUS=()
STAGE_BLOCKING=()

add_result() {
    STAGE_NAMES+=("$1")
    STAGE_STATUS+=("$2")
    STAGE_BLOCKING+=("$3")
}

# ensure_tool BINNAME INSTALLSPEC — returns 0 if the tool is available (installing
# it if missing and installs are enabled), 1 if it's unavailable (stage SKIPs).
ensure_tool() {
    local bin="$1" spec="$2"
    if command -v "$bin" >/dev/null 2>&1; then return 0; fi
    if [ "$INSTALL" -eq 0 ]; then return 1; fi
    echo "installing $spec ..."
    go install "$spec"
    hash -r
    command -v "$bin" >/dev/null 2>&1
}

# simple_stage NAME BLOCKING CMD... — runs CMD, logging to run/verify/NAME.log,
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

# --- Blocking stage: govulncheck (TEXT mode; exit 3 on vuln = FAIL) ----------
govulncheck_stage() {
    local log="$VERIFY_DIR/govulncheck.log"
    echo "== govulncheck =="
    if ! ensure_tool govulncheck golang.org/x/vuln/cmd/govulncheck@v1.5.0; then
        echo "  SKIP (govulncheck unavailable)"
        echo "govulncheck unavailable (missing and --no-install, or install failed)" >"$log"
        add_result govulncheck SKIP true
        return
    fi
    if govulncheck ./... >"$log" 2>&1; then
        echo "  PASS"; add_result govulncheck PASS true
    else
        echo "  FAIL (see run/verify/govulncheck.log)"; add_result govulncheck FAIL true
    fi
}

# --- Advisory stage: test-race (probe cgo+gcc first) ------------------------
race_stage() {
    local log="$VERIFY_DIR/test-race.log"
    echo "== test-race =="
    local cgo; cgo="$(go env CGO_ENABLED 2>/dev/null)"
    if [ "$cgo" = "1" ] && command -v gcc >/dev/null 2>&1; then
        if go test ./... -race >"$log" 2>&1; then
            echo "  PASS"; add_result test-race PASS false
        else
            echo "  FAIL (see run/verify/test-race.log)"; add_result test-race FAIL false
        fi
    else
        echo "  SKIP (no cgo/gcc)"
        echo "skipped: CGO_ENABLED=$cgo, gcc $(command -v gcc >/dev/null 2>&1 && echo present || echo absent)" >"$log"
        add_result test-race SKIP false
    fi
}

# --- Advisory stage: fuzz (discover every func Fuzz*, run each 10s) ----------
fuzz_stage() {
    local log="$VERIFY_DIR/fuzz.log"; : >"$log"
    echo "== fuzz =="
    local found=0 anyfail=0 files f dir fn
    files="$(grep -rlE 'func Fuzz[A-Za-z0-9_]+\(' --include='*_test.go' . 2>/dev/null || true)"
    for f in $files; do
        dir="$(dirname "$f")"
        for fn in $(grep -oE 'func Fuzz[A-Za-z0-9_]+' "$f" | sed 's/^func //'); do
            found=1
            echo "--- $dir $fn ---" >>"$log"
            if go test "$dir" -run '^$' -fuzz "^${fn}$" -fuzztime=10s >>"$log" 2>&1; then
                echo "  $fn: ok"
            else
                echo "  $fn: FAIL"
                anyfail=1
            fi
        done
    done
    if [ "$found" -eq 0 ]; then
        echo "  SKIP (no fuzz targets)"; add_result fuzz SKIP false; return
    fi
    if [ "$anyfail" -eq 0 ]; then
        echo "  PASS"; add_result fuzz PASS false
    else
        echo "  FAIL (see run/verify/fuzz.log)"; add_result fuzz FAIL false
    fi
}

# --- Advisory stage: apidiff (base = HEAD via a detached worktree) -----------
apidiff_stage() {
    local log="$VERIFY_DIR/apidiff.log"; : >"$log"
    echo "== apidiff =="
    if ! ensure_tool apidiff golang.org/x/exp/cmd/apidiff@latest; then
        echo "  SKIP (apidiff unavailable)"; echo "apidiff unavailable" >"$log"; add_result apidiff SKIP false; return
    fi
    local repo_win; repo_win="$(pwd -W 2>/dev/null || pwd)"
    local wt; wt="$(mktemp -d)"; rmdir "$wt"
    if ! git worktree add --detach "$wt" HEAD >>"$log" 2>&1; then
        echo "  FAIL (git worktree add failed, see run/verify/apidiff.log)"; add_result apidiff FAIL false; return
    fi
    local incompat=0 i=0 pkg snap out
    for pkg in "${CONTRACT_PKGS[@]}"; do
        i=$((i + 1))
        snap="$repo_win/$RUN_DIR/verify/apidiff-$i.api"
        # -w writes the BASE (worktree = HEAD) API snapshot from inside the worktree;
        # -incompatible then compares that base against the CURRENT tree from repo root.
        ( cd "$wt" && apidiff -w "$snap" "$pkg" ) >>"$log" 2>&1 || true
        out="$(apidiff -incompatible "$snap" "$pkg" 2>>"$log" || true)"
        if [ -n "$out" ]; then echo "$out" >>"$log"; incompat=1; fi
        rm -f "$snap"
    done
    # ALWAYS clean up the worktree, even if a comparison above errored.
    git worktree remove --force "$wt" >>"$log" 2>&1 || true
    if [ "$incompat" -eq 0 ]; then
        echo "  PASS"; add_result apidiff PASS false
    else
        echo "  FAIL (incompatible changes, see run/verify/apidiff.log)"; add_result apidiff FAIL false
    fi
}

# --- Advisory stage: topiccheck (--strict makes it able to FAIL) ------------
topiccheck_stage() {
    local log="$VERIFY_DIR/topiccheck.log"
    echo "== topiccheck =="
    local ok=0
    if [ "$STRICT" -eq 1 ]; then
        go run ./tools/topiccheck ./... --strict >"$log" 2>&1 && ok=1
    else
        go run ./tools/topiccheck ./... >"$log" 2>&1 && ok=1
    fi
    if [ "$ok" -eq 1 ]; then
        echo "  PASS"; add_result topiccheck PASS false
    else
        echo "  FAIL (see run/verify/topiccheck.log)"; add_result topiccheck FAIL false
    fi
}

# --- Slow stage: gremlins mutation testing ----------------------------------
gremlins_stage() {
    local log="$VERIFY_DIR/gremlins.log"; : >"$log"
    echo "== gremlins =="
    if ! ensure_tool gremlins github.com/go-gremlins/gremlins/cmd/gremlins@v0.6.0; then
        echo "  SKIP (gremlins unavailable)"; add_result gremlins SKIP false; return
    fi
    local anyfail=0 p
    for p in edge gateway outbox registry bus; do
        echo "--- ./$p/... ---" >>"$log"
        if ! gremlins unleash "./$p/..." >>"$log" 2>&1; then anyfail=1; fi
    done
    if [ "$anyfail" -eq 0 ]; then
        echo "  PASS"; add_result gremlins PASS false
    else
        echo "  FAIL (see run/verify/gremlins.log)"; add_result gremlins FAIL false
    fi
}

# --- Run --------------------------------------------------------------------
simple_stage build         true go build ./...
simple_stage vet           true go vet ./...
simple_stage golangci-lint true golangci-lint run ./...
simple_stage go-arch-lint  true go-arch-lint check
simple_stage test          true go test ./...
govulncheck_stage

if [ "$RUN_ADVISORY" -eq 1 ]; then
    race_stage
    fuzz_stage
    apidiff_stage
    topiccheck_stage
fi
if [ "$RUN_GREMLINS" -eq 1 ]; then
    gremlins_stage
fi

# --- Summary ----------------------------------------------------------------
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
if [ "$fail" -eq 0 ]; then echo "VERIFY OK"; else echo "VERIFY FAILED"; fi
exit "$fail"
