# verify.ps1 -- the umbrella verification gate for the rust-sketch (Steps 12, 14a).
# Behavioural twin of verify.sh -- see its header comment for the full stage list
# and rationale; kept in lockstep stage-for-stage.
#
# Usage:
#   .\verify.ps1                 # -Fast: blocking stages only (default)
#   .\verify.ps1 -Fast           # same as default
#   .\verify.ps1 -All            # + advisory: public-api, fuzz, csharp-client, topiccheck
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
        & cargo run -q -p requirecheck -- --strict *>> $log
    }
    if ($LASTEXITCODE -eq 0) {
        & cargo run -q -p topiccheck -- --durability-strict *>> $log
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

# --- Blocking stage: codegen-fresh (generated C# client drift) -------------
function Invoke-CodegenFreshStage {
    $log = Join-Path $verifyDir 'codegen-fresh.log'
    Write-Host "== codegen-fresh ==" -ForegroundColor Cyan
    & cargo run -q -p csharp-client-gen -- --out clients/csharp/Generated *> $log
    if ($LASTEXITCODE -eq 0) {
        & git diff --exit-code -- clients/csharp/Generated *>> $log
    }
    if ($LASTEXITCODE -eq 0) {
        Write-Host "  PASS" -ForegroundColor Green
        Add-Result 'codegen-fresh' 'PASS' $true
    } else {
        Write-Host "  FAIL (see run/verify/codegen-fresh.log)" -ForegroundColor Red
        Add-Result 'codegen-fresh' 'FAIL' $true
    }
}

# --- Advisory stage: csharp-client (external C# QUIC client, SKIP-aware) ----
# Builds the hand-written C# transport/CLI (clients\csharp, gbclient) and drives it
# against a self-contained monolith (NOT split-proof.ps1's processes -- those are
# already torn down by the time this stage runs) over pure QUIC. SKIPs (not FAILs)
# when dotnet is absent or the first scenario's raw exit code is 3
# (QuicConnection.IsSupported false -- msquic missing).
$CSharpPort = 8099
$CSharpPlayerPort = 9100
$CSharpDefaultDsn = 'postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable'

function Stop-CSharpStragglers {
    try { taskkill /F /IM server.exe *> $null } catch {}
}

function Invoke-GbClient {
    param([string[]]$CliArgs)
    $out = & dotnet run --project clients/csharp -c Release --no-build -- @CliArgs 2>$null
    $rc = $LASTEXITCODE
    return [pscustomobject]@{ Rc = $rc; Out = (($out | Out-String)).Trim() }
}

function Invoke-CSharpStage {
    $log = Join-Path $verifyDir 'csharp-client.log'
    '' | Out-File $log
    Write-Host "== csharp-client ==" -ForegroundColor Cyan
    if (-not (Get-Command dotnet -ErrorAction SilentlyContinue)) {
        Write-Host "  SKIP (dotnet unavailable)" -ForegroundColor Yellow
        'dotnet unavailable' | Out-File $log
        Add-Result 'csharp-client' 'SKIP' $false
        return
    }

    "--- dotnet build clients/csharp -c Release ---" | Out-File -Append $log
    & dotnet build clients/csharp -c Release *>> $log
    if ($LASTEXITCODE -ne 0) {
        Write-Host "  FAIL (dotnet build, see run/verify/csharp-client.log)" -ForegroundColor Red
        Add-Result 'csharp-client' 'FAIL' $false
        return
    }

    "--- cargo build -p server ---" | Out-File -Append $log
    & cargo build -p server *>> $log
    if ($LASTEXITCODE -ne 0) {
        Write-Host "  FAIL (cargo build -p server, see run/verify/csharp-client.log)" -ForegroundColor Red
        Add-Result 'csharp-client' 'FAIL' $false
        return
    }

    $dsn = if ($env:DATABASE_URL) { $env:DATABASE_URL } else { $CSharpDefaultDsn }

    Stop-CSharpStragglers
    "--- starting self-contained monolith on :$CSharpPort, player QUIC :$CSharpPlayerPort (ephemeral CA -> --insecure) ---" | Out-File -Append $log
    $proc = Start-Process -FilePath 'target\debug\server.exe' -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput (Join-Path $verifyDir 'csharp-client-server.out.log') `
        -RedirectStandardError (Join-Path $verifyDir 'csharp-client-server.err.log') `
        -Environment @{
            PORT = ":$CSharpPort"
            DATABASE_URL = $dsn
            PLAYER_EDGE_ADDR = ":$CSharpPlayerPort"
        }

    # curl.exe (not Invoke-WebRequest -- it hangs to its HttpClient timeout against this
    # server for reasons unrelated to server health) mirrors verify.sh's wait_healthy.
    $healthy = $false
    for ($i = 0; $i -lt 60; $i++) {
        & curl.exe -sf -o NUL "http://127.0.0.1:$CSharpPort/healthz" 2>$null
        if ($LASTEXITCODE -eq 0) { $healthy = $true; break }
        Start-Sleep -Milliseconds 500
    }
    if (-not $healthy) {
        Write-Host "  FAIL (monolith never became healthy, see run/verify/csharp-client.log)" -ForegroundColor Red
        try { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue } catch {}
        Stop-CSharpStragglers
        Add-Result 'csharp-client' 'FAIL' $false
        return
    }

    $status = 'PASS'

    "--- [C1] QUIC probe: raw --insecure leaderboard.topScores ---" | Out-File -Append $log
    $c1 = Invoke-GbClient @('raw', '--addr', "127.0.0.1:$CSharpPlayerPort", '--insecure', 'leaderboard.topScores')
    "    -> rc=$($c1.Rc)  $($c1.Out)" | Out-File -Append $log
    if ($c1.Rc -eq 3) {
        Write-Host "  SKIP (QUIC/msquic unsupported on this platform -- QuicConnection.IsSupported false)" -ForegroundColor Yellow
        try { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue } catch {}
        Stop-CSharpStragglers
        Add-Result 'csharp-client' 'SKIP' $false
        return
    }
    if ($c1.Rc -ne 0) {
        "    C1 FAIL: expected exit 0, got rc=$($c1.Rc)" | Out-File -Append $log
        $status = 'FAIL'
    }

    "--- [C2] raw --insecure characters.create, NO token -> exit 1 + Unauthorized ---" | Out-File -Append $log
    $c2 = Invoke-GbClient @('raw', '--addr', "127.0.0.1:$CSharpPlayerPort", '--insecure', 'characters.create', '{"name":"x","class":""}')
    "    -> rc=$($c2.Rc)  $($c2.Out)" | Out-File -Append $log
    if ($c2.Rc -ne 1 -or $c2.Out -notmatch 'Unauthorized') {
        "    C2 FAIL: expected exit 1 + Unauthorized, got rc=$($c2.Rc) $($c2.Out)" | Out-File -Append $log
        $status = 'FAIL'
    }

    "--- [C3] raw --insecure --token bogus characters.ownerOf -> exit 1 + NotFound ---" | Out-File -Append $log
    $c3 = Invoke-GbClient @('raw', '--addr', "127.0.0.1:$CSharpPlayerPort", '--insecure', '--token', 'bogus', 'characters.ownerOf', '{"character_id":"z"}')
    "    -> rc=$($c3.Rc)  $($c3.Out)" | Out-File -Append $log
    if ($c3.Rc -ne 1 -or $c3.Out -notmatch 'NotFound') {
        "    C3 FAIL: expected exit 1 + NotFound, got rc=$($c3.Rc) $($c3.Out)" | Out-File -Append $log
        $status = 'FAIL'
    }

    "--- [C4] flow --insecure (typed client: register -> create -> list over pure QUIC) ---" | Out-File -Append $log
    $c4 = Invoke-GbClient @('flow', '--addr', "127.0.0.1:$CSharpPlayerPort", '--insecure')
    "    -> rc=$($c4.Rc)  $($c4.Out)" | Out-File -Append $log
    if ($c4.Rc -ne 0) {
        "    C4 FAIL: expected exit 0, got rc=$($c4.Rc)" | Out-File -Append $log
        $status = 'FAIL'
    }

    try { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue } catch {}
    Stop-CSharpStragglers

    if ($status -eq 'PASS') {
        Write-Host "  PASS" -ForegroundColor Green
        Add-Result 'csharp-client' 'PASS' $false
    } else {
        Write-Host "  FAIL (see run/verify/csharp-client.log)" -ForegroundColor Red
        Add-Result 'csharp-client' 'FAIL' $false
    }
}

# --- Advisory stage: topiccheck (defined-vs-subscribed topic drift) ----------
# The Rust redesign of Go's whole-program topiccheck: tools/topiccheck builds the
# monolith module set with a recording bus transport and diffs subscribed vs
# bus::define'd topics. --strict exits non-zero on drift, so this FAILs then; advisory
# by default, blocking only under the umbrella -Strict.
function Invoke-TopiccheckStage {
    Invoke-SimpleStage 'topiccheck' $false 'cargo' @('run', '-q', '-p', 'topiccheck', '--', '--strict')
}

# --- Run ---------------------------------------------------------------------
Invoke-SimpleStage 'build'   $true 'cargo' @('build', '--workspace')
Invoke-SimpleStage 'clippy'  $true 'cargo' @('clippy', '--workspace', '--all-targets', '--', '-D', 'warnings')
Invoke-SimpleStage 'test'    $true 'cargo' @('test', '--workspace')
Invoke-CargoAuditStage
Invoke-FortressStage
Invoke-CodegenFreshStage
Invoke-SimpleStage 'split-proof' $true 'pwsh' @('-File', (Join-Path $root 'split-proof.ps1'))

if ($RunAdvisory) {
    Invoke-PublicApiStage
    Invoke-FuzzStage
    Invoke-CSharpStage
    Invoke-TopiccheckStage
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
