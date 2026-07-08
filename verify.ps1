# verify.ps1 -- the umbrella verification gate for the rust-sketch (Steps 12, 14a).
# Behavioural twin of verify.sh -- see its header comment for the full stage list
# and rationale; kept in lockstep stage-for-stage.
#
# Usage:
#   .\verify.ps1                 # -Fast: blocking stages only (default)
#   .\verify.ps1 -Fast           # same as default
#   .\verify.ps1 -All            # + advisory: public-api, fuzz
#   .\verify.ps1 -Slow           # + cargo-mutants mutation testing (very slow)
#   .\verify.ps1 -All -Strict    # advisory failures ALSO flip the exit code
#   .\verify.ps1 -All -NoInstall # never auto-install a missing CLI (it SKIPs)
#
# ASCII only -- PowerShell 5.1 chokes on em-dashes.

param(
    [switch]$Fast,
    [switch]$All,
    [switch]$Slow,
    [switch]$Strict,
    [switch]$NoInstall
)

# Deliberately NOT 'Stop' during the run phase: a failing stage must not abort
# the runner. Each stage records PASS/FAIL/SKIP; the summary decides the exit code.
$ErrorActionPreference = 'Continue'

$root = $PSScriptRoot
Set-Location $root
$runDir = Join-Path $root 'run'
$verifyDir = Join-Path $runDir 'verify'
New-Item -ItemType Directory -Force -Path $verifyDir | Out-Null

$RunAdvisory = $All.IsPresent -or $Slow.IsPresent
$RunSlow = $Slow.IsPresent
$Install = -not $NoInstall.IsPresent
$StrictOn = $Strict.IsPresent

# api/*api and api/*events contract crates (additive-only guard scope) -- mirrors
# verify.sh's PUBLIC_API_CRATES, one-to-one by domain.
$publicApiCrates = @(
    'accountsevents',
    'accountsapi',
    'charactersevents',
    'charactersapi',
    'inventoryapi',
    'matchevents',
    'schedulerevents',
    'adminapi'
)

# RUSTSEC-2023-0071 (rsa 0.9.10, Marvin Attack timing side-channel): a dev-only
# dependency of modules/accounts (mints RSA-signed test JWTs for the OIDC verifier's
# fixtures), never linked into a shipped binary. Upstream: "No fixed upgrade is
# available!" as of this writing -- accepted risk, revisit when a fix ships.
$cargoAuditIgnore = @('RUSTSEC-2023-0071')

$fuzzTargets = @('frame_decode', 'wire_decode')

# --- Result accumulation ----------------------------------------------------
$script:results = @()
function Add-Result {
    param([string]$Name, [string]$Status, [bool]$Blocking)
    $script:results += [pscustomobject]@{ Name = $Name; Status = $Status; Blocking = $Blocking }
}

# Ensure-Tool BIN CMD ARGS -- $true if BIN is available (installing via CMD ARGS if
# missing and installs are enabled), $false if unavailable (stage SKIPs).
function Ensure-Tool {
    param([string]$Bin, [string]$Exe, [string[]]$InstallArgs)
    if (Get-Command $Bin -ErrorAction SilentlyContinue) { return $true }
    if (-not $Install) { return $false }
    Write-Host "installing $Bin ($Exe $InstallArgs) ..." -ForegroundColor Yellow
    & $Exe @InstallArgs *> $null
    return [bool](Get-Command $Bin -ErrorAction SilentlyContinue)
}

# Invoke-SimpleStage NAME BLOCKING EXE ARGS -- runs EXE, logging to
# run/verify/NAME.log, recording PASS on exit 0 else FAIL.
function Invoke-SimpleStage {
    param([string]$Name, [bool]$Blocking, [string]$Exe, [string[]]$Arguments)
    $log = Join-Path $verifyDir "$Name.log"
    Write-Host "== $Name ==" -ForegroundColor Cyan
    & $Exe @Arguments *> $log
    if ($LASTEXITCODE -eq 0) {
        Write-Host "  PASS" -ForegroundColor Green
        Add-Result $Name 'PASS' $Blocking
    } else {
        Write-Host "  FAIL (see run/verify/$Name.log)" -ForegroundColor Red
        Add-Result $Name 'FAIL' $Blocking
    }
}

# --- Blocking stage: fortress (Step 5) --------------------------------------
function Invoke-FortressStage {
    $log = Join-Path $verifyDir 'fortress.log'
    Write-Host "== fortress ==" -ForegroundColor Cyan
    & cargo build -p server -p characters-svc -p inventory-svc -p gateway-svc -p config-svc -p accounts-svc -p admin-svc -p audit-svc -p scheduler-svc -p match-svc -p rating-svc -p leaderboard-svc *> $log
    if ($LASTEXITCODE -eq 0) {
        & cargo run -q -p archcheck *>> $log
    }
    if ($LASTEXITCODE -eq 0) {
        Write-Host "  PASS" -ForegroundColor Green; Add-Result 'fortress' 'PASS' $true
    } else {
        Write-Host "  FAIL (see run/verify/fortress.log)" -ForegroundColor Red; Add-Result 'fortress' 'FAIL' $true
    }
}

# --- Blocking stage: cargo-audit ---------------------------------------------
function Invoke-CargoAuditStage {
    $log = Join-Path $verifyDir 'cargo-audit.log'
    Write-Host "== cargo-audit ==" -ForegroundColor Cyan
    if (-not (Ensure-Tool 'cargo-audit' 'cargo' @('install', 'cargo-audit', '--locked', '--version', '0.22.2'))) {
        Write-Host "  SKIP (cargo-audit unavailable)" -ForegroundColor Yellow
        'cargo-audit unavailable (missing and -NoInstall, or install failed)' | Out-File $log
        Add-Result 'cargo-audit' 'SKIP' $true
        return
    }
    $auditArgs = @()
    foreach ($id in $cargoAuditIgnore) { $auditArgs += @('--ignore', $id) }
    & cargo audit @auditArgs *> $log
    if ($LASTEXITCODE -eq 0) {
        Write-Host "  PASS" -ForegroundColor Green
        Add-Result 'cargo-audit' 'PASS' $true
        return
    }
    $logText = Get-Content $log -Raw
    if ($logText -match '(?i)error loading advisory database|failed to fetch|could not fetch|couldn.?t resolve host|network is unreachable|failed to clone|unable to connect') {
        Write-Host "  SKIP (advisory DB fetch failed -- offline?)" -ForegroundColor Yellow
        Add-Result 'cargo-audit' 'SKIP' $true
    } else {
        Write-Host "  FAIL (see run/verify/cargo-audit.log)" -ForegroundColor Red
        Add-Result 'cargo-audit' 'FAIL' $true
    }
}

# --- Advisory stage: public-api (apidiff parity, additive-only guard) -------
function Ensure-PublicApiTooling {
    $hasNightly = (& rustup toolchain list 2>$null) -match '^nightly'
    if (-not $hasNightly) {
        if (-not $Install) { return $false }
        Write-Host "installing nightly toolchain (for rustdoc JSON) ..." -ForegroundColor Yellow
        & rustup toolchain install nightly --profile minimal *> $null
        $hasNightly = (& rustup toolchain list 2>$null) -match '^nightly'
    }
    if (-not $hasNightly) { return $false }
    return (Ensure-Tool 'cargo-public-api' 'cargo' @('+nightly', 'install', 'cargo-public-api', '--locked'))
}

function Invoke-PublicApiStage {
    $log = Join-Path $verifyDir 'public-api.log'
    '' | Out-File $log
    Write-Host "== public-api ==" -ForegroundColor Cyan
    if (-not (Ensure-PublicApiTooling)) {
        Write-Host "  SKIP (nightly toolchain / cargo-public-api unavailable)" -ForegroundColor Yellow
        'nightly toolchain or cargo-public-api unavailable' | Out-File $log
        Add-Result 'public-api' 'SKIP' $false
        return
    }
    $wt = Join-Path ([System.IO.Path]::GetTempPath()) ("verify-public-api-" + [guid]::NewGuid().ToString('N'))
    & git worktree add --detach $wt HEAD *>> $log
    if ($LASTEXITCODE -ne 0) {
        Write-Host "  FAIL (git worktree add failed, see run/verify/public-api.log)" -ForegroundColor Red
        Add-Result 'public-api' 'FAIL' $false
        return
    }
    $incompat = $false
    try {
        foreach ($pkg in $publicApiCrates) {
            $base = Join-Path $verifyDir "public-api-base-$pkg.txt"
            $cur = Join-Path $verifyDir "public-api-cur-$pkg.txt"
            $diffOut = Join-Path $verifyDir "public-api-diff-$pkg.txt"
            Push-Location $wt
            & cargo public-api -p $pkg -s --color=never *> $base 2>> $log
            Pop-Location
            & cargo public-api -p $pkg -s --color=never *> $cur 2>> $log
            $baseLines = Get-Content $base -ErrorAction SilentlyContinue
            $curLines = Get-Content $cur -ErrorAction SilentlyContinue
            $diffText = Compare-Object $baseLines $curLines
            $diffText | ForEach-Object { "{0} {1}" -f $_.SideIndicator, $_.InputObject } | Out-File $diffOut
            # A line only present in the BASE (SideIndicator '<=', i.e. HEAD had it,
            # current doesn't) means a symbol vanished or its signature changed --
            # both break a consumer. Lines only in current ('=>') are pure additions.
            $removed = $diffText | Where-Object { $_.SideIndicator -eq '<=' }
            if ($removed) {
                Write-Host "  ${pkg}: INCOMPATIBLE (see run/verify/public-api-diff-$pkg.txt)" -ForegroundColor Red
                $incompat = $true
            } else {
                "  ${pkg}: ok" | Out-File -Append $log
            }
        }
    } finally {
        & git worktree remove --force $wt *>> $log
    }
    if (-not $incompat) {
        Write-Host "  PASS" -ForegroundColor Green
        Add-Result 'public-api' 'PASS' $false
    } else {
        Write-Host "  FAIL (incompatible API changes, see run/verify/public-api-diff-*.txt)" -ForegroundColor Red
        Add-Result 'public-api' 'FAIL' $false
    }
}

# --- Advisory stage: fuzz (cargo-fuzz, core/edge/fuzz/) ---------------------
function Invoke-FuzzStage {
    $log = Join-Path $verifyDir 'fuzz.log'
    '' | Out-File $log
    Write-Host "== fuzz ==" -ForegroundColor Cyan
    if (-not (Ensure-Tool 'cargo-fuzz' 'cargo' @('install', 'cargo-fuzz', '--locked'))) {
        Write-Host "  SKIP (cargo-fuzz unavailable)" -ForegroundColor Yellow
        'cargo-fuzz unavailable' | Out-File $log
        Add-Result 'fuzz' 'SKIP' $false
        return
    }
    $ran = $false
    $anyfail = $false
    $platformBlocked = $false
    foreach ($t in $fuzzTargets) {
        "--- $t ---" | Out-File -Append $log
        Push-Location (Join-Path $root 'core\edge')
        & cargo +nightly fuzz run $t -- -max_total_time=10 -runs=100000 *>> $log
        $ec = $LASTEXITCODE
        Pop-Location
        if ($ec -eq 0) {
            Write-Host "  $t`: ok"
            $ran = $true
        } else {
            $logText = Get-Content $log -Raw
            if ($logText -match '(?i)0xc0000135|DLL_NOT_FOUND|status_dll_not_found|is not installed') {
                Write-Host "  $t`: SKIP (cannot execute the libFuzzer binary on this platform)" -ForegroundColor Yellow
                $platformBlocked = $true
            } else {
                Write-Host "  $t`: FAIL" -ForegroundColor Red
                $anyfail = $true
            }
        }
    }
    if ($anyfail) {
        Write-Host "  FAIL (see run/verify/fuzz.log)" -ForegroundColor Red
        Add-Result 'fuzz' 'FAIL' $false
    } elseif (-not $ran -and $platformBlocked) {
        Write-Host "  SKIP (fuzz targets present but cannot execute on this platform)" -ForegroundColor Yellow
        Add-Result 'fuzz' 'SKIP' $false
    } else {
        Write-Host "  PASS" -ForegroundColor Green
        Add-Result 'fuzz' 'PASS' $false
    }
}

# --- Slow stage: cargo-mutants mutation testing ------------------------------
function Invoke-MutantsStage {
    $log = Join-Path $verifyDir 'mutants.log'
    '' | Out-File $log
    Write-Host "== mutants ==" -ForegroundColor Cyan
    if (-not (Ensure-Tool 'cargo-mutants' 'cargo' @('install', 'cargo-mutants', '--locked'))) {
        Write-Host "  SKIP (cargo-mutants unavailable)" -ForegroundColor Yellow
        Add-Result 'mutants' 'SKIP' $false
        return
    }
    & cargo mutants -p edge -p gateway -p outbox -p registry -p bus --timeout 300 *> $log
    if ($LASTEXITCODE -eq 0) {
        Write-Host "  PASS" -ForegroundColor Green
        Add-Result 'mutants' 'PASS' $false
    } else {
        Write-Host "  FAIL (see run/verify/mutants.log)" -ForegroundColor Red
        Add-Result 'mutants' 'FAIL' $false
    }
}

# topiccheck (linkme-based bus.Define/on-subscriber-coverage equivalent): landing in
# a follow-up step (Step 14, part B) once the module set it audits is final. This
# comment marks where its stage call slots in, alongside the other advisory stages.

# --- Run ---------------------------------------------------------------------
Invoke-SimpleStage 'build'   $true 'cargo' @('build', '--workspace')
Invoke-SimpleStage 'clippy'  $true 'cargo' @('clippy', '--workspace', '--all-targets', '--', '-D', 'warnings')
Invoke-SimpleStage 'test'    $true 'cargo' @('test', '--workspace')
Invoke-CargoAuditStage
Invoke-FortressStage
Invoke-SimpleStage 'split-proof' $true 'pwsh' @('-File', (Join-Path $root 'split-proof.ps1'))

if ($RunAdvisory) {
    Invoke-PublicApiStage
    Invoke-FuzzStage
    # <-- topiccheck stage slots in here once it lands (Step 14, part B) -->
}
if ($RunSlow) {
    Invoke-MutantsStage
}

# --- Summary ------------------------------------------------------------------
Write-Host ''
Write-Host '=== verify summary ===' -ForegroundColor Cyan
'{0,-14} | {1,-6} | {2,-8}' -f 'Stage', 'Status', 'Blocking' | Write-Host
'{0,-14}-+-{1,-6}-+-{2,-8}' -f ('-' * 14), ('-' * 6), ('-' * 8) | Write-Host
$overall = 0
foreach ($r in $script:results) {
    '{0,-14} | {1,-6} | {2,-8}' -f $r.Name, $r.Status, $r.Blocking | Write-Host
    if ($r.Status -eq 'FAIL' -and ($r.Blocking -or $StrictOn)) { $overall = 1 }
}
Write-Host ''
if ($overall -eq 0) { Write-Host 'VERIFY: OK' } else { Write-Host 'VERIFY: FAIL' }
exit $overall
