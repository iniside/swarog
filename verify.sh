#!/usr/bin/env bash
# verify.sh -- the umbrella verification gate for the rust-sketch (Step 12).
#
# Runs, in order, keeping going after a failure so the summary is complete:
#   1. cargo build            (whole workspace)
#   2. cargo clippy           (--all-targets, -D warnings: any lint FAILS)
#   3. cargo test             (whole workspace: unit + rpc-macro edge round-trip)
#   4. fortress               (build every cmd/<name>-svc + archcheck dependency law)
#   5. split proof            (./split-proof.sh -- the FOUR-PROCESS topology proof)
#
# Prints a PASS/FAIL summary and exits non-zero if ANY stage failed. The split proof
# is the point: it exercises the SPLIT microservices (A=characters-svc, B=inventory-
# svc, C=config-svc, D=accounts-svc, G=gateway-svc, E=admin-svc) over real HTTP/QUIC, not the monolith. The
# fortress stage (Step 5) enforces the dependency law: every domain module boots as
# its own -svc and no module imports another module's impl or a foreign <name>rpc.
#
# ASCII only (no em-dashes) so the sibling verify.ps1 stays byte-parallel and
# PowerShell 5.1 never chokes.
set -uo pipefail
cd "$(dirname "$0")"

names=()
results=()

run_stage() {
    local name="$1"; shift
    echo ""
    echo ">>> $name"
    if "$@"; then
        names+=("$name"); results+=("PASS")
    else
        names+=("$name"); results+=("FAIL")
    fi
}

# The fortress stage: every domain module must compile + boot as its own -svc binary,
# and archcheck enforces the dependency law (no module->module / module->foreign-rpc
# edge, no resurrected Option<edge::Server> under modules/).
fortress() {
    cargo build -p server -p characters-svc -p inventory-svc -p gateway-svc -p config-svc -p accounts-svc -p admin-svc \
        && cargo run -q -p archcheck
}

run_stage "build"       cargo build --workspace
run_stage "clippy"      cargo clippy --workspace --all-targets -- -D warnings
run_stage "test"        cargo test --workspace
run_stage "fortress"    fortress
run_stage "split-proof" bash ./split-proof.sh

echo ""
echo "==================== VERIFY SUMMARY ===================="
overall=0
for i in "${!names[@]}"; do
    printf "  %-6s %s\n" "${results[$i]}" "${names[$i]}"
    [ "${results[$i]}" = "FAIL" ] && overall=1
done
echo "======================================================="
if [ "$overall" -eq 0 ]; then
    echo "VERIFY: PASS"
else
    echo "VERIFY: FAIL"
fi
exit "$overall"
