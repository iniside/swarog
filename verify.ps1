# verify.ps1 -- the umbrella verification gate for the rust-sketch (Step 12).
#
# Runs, in order, keeping going after a failure so the summary is complete:
#   1. cargo build            (whole workspace)
#   2. cargo clippy           (--all-targets, -D warnings: any lint FAILS)
#   3. cargo test             (whole workspace: unit + rpc-macro edge round-trip)
#   4. fortress               (build every cmd/<name>-svc + archcheck dependency law)
#   5. split proof            (.\split-proof.ps1 -- the FOUR-PROCESS topology proof)
#
# Prints a PASS/FAIL summary and exits non-zero if ANY stage failed. The split proof
# is the point: it exercises the SPLIT microservices (A=characters-svc, B=inventory-
# svc, C=config-svc, D=accounts-svc, G=gateway-svc) over real HTTP/QUIC, not the monolith. The fortress
# stage (Step 5) enforces the dependency law via archcheck.
#
# ASCII only -- PowerShell 5.1 chokes on em-dashes.

[CmdletBinding()]
param()
Set-Location -Path $PSScriptRoot

$names   = @()
$results = @()

function Run-Stage([string]$Name, [scriptblock]$Action) {
    Write-Host ''
    Write-Host ">>> $Name"
    & $Action
    $ok = ($LASTEXITCODE -eq 0)
    $script:names   += $Name
    $script:results += ($(if ($ok) { 'PASS' } else { 'FAIL' }))
}

Run-Stage 'build'       { cargo build --workspace }
Run-Stage 'clippy'      { cargo clippy --workspace --all-targets -- -D warnings }
Run-Stage 'test'        { cargo test --workspace }
Run-Stage 'fortress'    { cargo build -p server -p characters-svc -p inventory-svc -p gateway-svc -p config-svc -p accounts-svc; if ($LASTEXITCODE -eq 0) { cargo run -q -p archcheck } }
Run-Stage 'split-proof' { & (Join-Path $PSScriptRoot 'split-proof.ps1') }

Write-Host ''
Write-Host '==================== VERIFY SUMMARY ===================='
$overall = 0
for ($i = 0; $i -lt $names.Count; $i++) {
    '  {0,-6} {1}' -f $results[$i], $names[$i] | Write-Host
    if ($results[$i] -eq 'FAIL') { $overall = 1 }
}
Write-Host '======================================================='
if ($overall -eq 0) { Write-Host 'VERIFY: PASS' } else { Write-Host 'VERIFY: FAIL' }
exit $overall
