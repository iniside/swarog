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
#   .\verify.ps1 -BlessPublicApi # regenerate the committed public-api snapshots and exit
#
# ASCII only -- PowerShell 5.1 chokes on em-dashes.

param(
    [switch]$Fast,
    [switch]$All,
    [switch]$Slow,
    [switch]$Strict,
    [switch]$NoInstall,
    [switch]$BlessPublicApi
)

# Deliberately NOT 'Stop' during the run phase: a failing stage must not abort
# the runner. Each stage records PASS/FAIL/SKIP; the summary decides the exit code.
$ErrorActionPreference = 'Continue'

$root = $PSScriptRoot
Set-Location $root
$runDir = Join-Path $root 'run'
$verifyDir = Join-Path $runDir 'verify'
New-Item -ItemType Directory -Force -Path $verifyDir | Out-Null

# --- Live log tee: every invocation writes its full console output to a timestamped
# log file (in addition to the console), with the log path printed FIRST so a human or
# an agent can tail it live. PS7 supports nested transcripts, which matters because this
# script invokes split-proof.ps1 as a child stage.
$logsDir = Join-Path $root 'run/logs'
New-Item -ItemType Directory -Force -Path $logsDir | Out-Null
$LogPath = Join-Path $logsDir "verify-$(Get-Date -Format 'yyyyMMdd-HHmmss').log"
Write-Host "[log] $LogPath"
Start-Transcript -Path $LogPath | Out-Null

# Exit-WithLog CODE -- stops the transcript before exiting so every exit path (normal
# completion, --bless-public-api's early exits) closes the log file cleanly.
function Exit-WithLog {
    param([int]$Code)
    Stop-Transcript | Out-Null
    exit $Code
}

$RunAdvisory = $All.IsPresent -or $Slow.IsPresent
$RunSlow = $Slow.IsPresent
$Install = -not $NoInstall.IsPresent
$StrictOn = $Strict.IsPresent

# Directory holding the committed public-api snapshots (the trusted baseline).
$publicApiBaselineDir = Join-Path $root 'docs/reference/public-api-baseline'

# Get-PublicApiCrates -- the public-api gate's scope, DERIVED from the filesystem: the
# `name = "..."` of every api/*/api/Cargo.toml and api/*/events/Cargo.toml (twin of
# verify.sh's public_api_crates). A new domain joins the gate automatically; rpc crates
# stay out by construction.
function Get-PublicApiCrates {
    $names = @()
    $apiRoot = Join-Path $root 'api'
    foreach ($sub in @('api', 'events')) {
        Get-ChildItem -Path $apiRoot -Directory -ErrorAction SilentlyContinue | Sort-Object Name | ForEach-Object {
            $toml = Join-Path (Join-Path $_.FullName $sub) 'Cargo.toml'
            if (Test-Path $toml) {
                $m = Select-String -Path $toml -Pattern '^name = "(.*)"' | Select-Object -First 1
                if ($m) { $names += $m.Matches[0].Groups[1].Value }
            }
        }
    }
    $names
}

# Get-FortressCrates -- the fortress stage's build scope, DERIVED from the filesystem
# (twin of Get-PublicApiCrates): the `name = "..."` of every cmd/*-svc/Cargo.toml, plus
# the monolith `server`. A new svc crate joins the fortress build automatically; the
# module-set-membership drift itself is guarded separately by
# checkmodules::split_fleet_matches_cmd_dirs (tools/checkmodules).
function Get-FortressCrates {
    $names = @('server')
    $cmdRoot = Join-Path $root 'cmd'
    Get-ChildItem -Path $cmdRoot -Directory -Filter '*-svc' -ErrorAction SilentlyContinue | Sort-Object Name | ForEach-Object {
        $toml = Join-Path $_.FullName 'Cargo.toml'
        if (Test-Path $toml) {
            $m = Select-String -Path $toml -Pattern '^name = "(.*)"' | Select-Object -First 1
            if ($m) { $names += $m.Matches[0].Groups[1].Value }
        }
    }
    $names
}

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
    $pkgArgs = (Get-FortressCrates) | ForEach-Object { '-p', $_ }
    & cargo build @pkgArgs *> $log
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

# --- Advisory stage: public-api (committed-snapshot baseline gate) -----------
# Twin of verify.sh's public_api_stage: diffs each contract crate's current public API
# against a COMMITTED snapshot under docs/reference/public-api-baseline/<crate>.txt. ANY
# difference FAILs (removed lines flagged BREAKING, added ADDITIVE); re-bless intentional
# changes via -BlessPublicApi. cargo-public-api is version-pinned and the pin recorded in
# each snapshot header. RESIDUAL RISK: rustdoc-JSON formatting can drift across the
# nightly toolchain itself (nightly date not pinned) -- a formatting-only diff, re-blessed
# after confirming no symbol changes. Advisory (blocking only under -Strict).
function Ensure-PublicApiTooling {
    $hasNightly = (& rustup toolchain list 2>$null) -match '^nightly'
    if (-not $hasNightly) {
        if (-not $Install) { return $false }
        Write-Host "installing nightly toolchain (for rustdoc JSON) ..." -ForegroundColor Yellow
        & rustup toolchain install nightly --profile minimal *> $null
        $hasNightly = (& rustup toolchain list 2>$null) -match '^nightly'
    }
    if (-not $hasNightly) { return $false }
    # Pin cargo-public-api (cargo-audit precedent): rustdoc-JSON output is
    # version-sensitive, so an unpinned bump would spuriously diff every snapshot.
    return (Ensure-Tool 'cargo-public-api' 'cargo' @('+nightly', 'install', 'cargo-public-api', '--locked', '--version', '0.52.0'))
}

# Invoke-BlessPublicApi -- regenerate every committed snapshot and exit. First-run
# (baseline dir absent) is fine: the dir is created here.
function Invoke-BlessPublicApi {
    Write-Host "== public-api bless ==" -ForegroundColor Cyan
    if (-not (Ensure-PublicApiTooling)) {
        Write-Host "  cannot bless: nightly toolchain / cargo-public-api unavailable" -ForegroundColor Red
        Exit-WithLog 1
    }
    New-Item -ItemType Directory -Force -Path $publicApiBaselineDir | Out-Null
    $pver = ((& cargo public-api --version | Select-Object -First 1) -split '\s+')[1]
    $header = "# cargo-public-api $pver -- regenerate via .\verify.ps1 -BlessPublicApi"
    $fail = $false
    foreach ($pkg in (Get-PublicApiCrates)) {
        $snap = Join-Path $publicApiBaselineDir "$pkg.txt"
        $out = & cargo +nightly public-api -p $pkg -s --color=never
        if ($LASTEXITCODE -ne 0) {
            Write-Host "  ${pkg}: cargo public-api FAILED" -ForegroundColor Red
            $fail = $true
            continue
        }
        (@($header) + $out) | Set-Content -Path $snap -Encoding utf8
        Write-Host "  blessed $pkg"
    }
    if ($fail) { Exit-WithLog 1 } else { Exit-WithLog 0 }
}

# Get-OrphanBaselineFindings -- Step 6b (twin of verify.sh's orphan_baseline_findings): a
# deleted contract crate leaves its committed docs/reference/public-api-baseline/<crate>.txt
# behind forever, since Invoke-PublicApiStage only ITERATES the live crate list (never the
# baseline dir) -- a snapshot for a crate that no longer exists would silently never be
# checked or cleaned up again. Diffs the baseline dir's file stems against the live
# Get-PublicApiCrates set; returns one finding string per orphan naming the file.
function Get-OrphanBaselineFindings {
    param([string[]]$LiveCrates)
    $findings = @()
    if (-not (Test-Path $publicApiBaselineDir)) { return $findings }
    $liveSet = [System.Collections.Generic.HashSet[string]]::new([string[]]$LiveCrates)
    Get-ChildItem -Path $publicApiBaselineDir -Filter '*.txt' -ErrorAction SilentlyContinue | Sort-Object Name | ForEach-Object {
        $stem = $_.BaseName
        if (-not $liveSet.Contains($stem)) {
            $findings += "  ORPHAN baseline $($_.FullName) -- no live crate named `"$stem`" (Get-PublicApiCrates is: $($LiveCrates -join ', ')) -- delete this snapshot, it belongs to a removed contract crate"
        }
    }
    $findings
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
    $diff = $false
    $toolfail = $false
    $liveCrates = @(Get-PublicApiCrates)
    $orphans = Get-OrphanBaselineFindings -LiveCrates $liveCrates
    if ($orphans) {
        $diff = $true
        foreach ($o in $orphans) {
            Write-Host $o -ForegroundColor Red
            $o | Out-File -Append $log
        }
    }
    foreach ($pkg in (Get-PublicApiCrates)) {
        $snap = Join-Path $publicApiBaselineDir "$pkg.txt"
        $cur = Join-Path $verifyDir "public-api-cur-$pkg.txt"
        $diffOut = Join-Path $verifyDir "public-api-diff-$pkg.txt"
        # Tool errors FAIL the stage -- capture stdout only; stderr goes to the log.
        & cargo +nightly public-api -p $pkg -s --color=never 1> $cur 2>> $log
        if ($LASTEXITCODE -ne 0) {
            Write-Host "  ${pkg}: cargo public-api FAILED (see run/verify/public-api.log)" -ForegroundColor Red
            $toolfail = $true
            continue
        }
        if (-not (Test-Path $snap)) {
            Write-Host "  ${pkg}: MISSING baseline snapshot -- run .\verify.ps1 -BlessPublicApi" -ForegroundColor Red
            $diff = $true
            continue
        }
        # Strip the pinned-version header before comparing against live output.
        $expected = @(Get-Content $snap | Where-Object { $_ -notmatch '^# cargo-public-api' })
        $curLines = @(Get-Content $cur -ErrorAction SilentlyContinue)
        $delta = Compare-Object $expected $curLines
        if ($delta) {
            $delta | ForEach-Object { "{0} {1}" -f $_.SideIndicator, $_.InputObject } | Out-File $diffOut
            Write-Host "  ${pkg}: DIFFERS from committed baseline (see run/verify/public-api-diff-$pkg.txt)" -ForegroundColor Red
            $delta | ForEach-Object {
                if ($_.SideIndicator -eq '<=') { Write-Host ("  BREAKING - " + $_.InputObject) -ForegroundColor Red }
                else { Write-Host ("  ADDITIVE + " + $_.InputObject) -ForegroundColor Yellow }
            }
            $diff = $true
        } else {
            "  ${pkg}: ok" | Out-File -Append $log
        }
    }
    if ($toolfail) {
        Write-Host "  FAIL (cargo public-api errored, see run/verify/public-api.log)" -ForegroundColor Red
        Add-Result 'public-api' 'FAIL' $false
    } elseif (-not $diff) {
        Write-Host "  PASS" -ForegroundColor Green
        Add-Result 'public-api' 'PASS' $false
    } else {
        Write-Host "  FAIL: a crate differs from its committed baseline -- review the diff; if" -ForegroundColor Red
        Write-Host "        intentional (additive or a versioned new contract), regenerate via" -ForegroundColor Red
        Write-Host "        .\verify.ps1 -BlessPublicApi (or --bless-public-api). If only formatting" -ForegroundColor Red
        Write-Host "        changed (toolchain drift), re-bless after confirming no symbol changes." -ForegroundColor Red
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
    & cargo mutants -p edge -p gateway -p asyncevents -p registry -p bus --timeout 300 *> $log
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
    "--- starting self-contained monolith on :$CSharpPort, player QUIC :$CSharpPlayerPort (ephemeral CA -> --insecure, APIKEYS_DEV_SEED=1, dev flags on) ---" | Out-File -Append $log
    # Dev conveniences are now explicit opt-ins (fail-closed defaults): the gbclient flow
    # does register->create->list, so enable ACCOUNTS_DEV_AUTH (+ INVENTORY_DEV_GRANT for
    # symmetry). The admin module now boots with ZERO admin users (a warned no-op, session
    # auth), so no ADMIN_USER/ADMIN_PASS is needed -- this flow never touches /admin.
    $proc = Start-Process -FilePath 'target\debug\server.exe' -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput (Join-Path $verifyDir 'csharp-client-server.out.log') `
        -RedirectStandardError (Join-Path $verifyDir 'csharp-client-server.err.log') `
        -Environment @{
            PORT = ":$CSharpPort"
            DATABASE_URL = $dsn
            PLAYER_EDGE_ADDR = ":$CSharpPlayerPort"
            APIKEYS_DEV_SEED = '1'
            ACCOUNTS_DEV_AUTH = '1'
            INVENTORY_DEV_GRANT = '1'
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

    "--- [C1] QUIC probe: raw --insecure --api-key dev-key-client leaderboard.topScores ---" | Out-File -Append $log
    $c1 = Invoke-GbClient @('raw', '--addr', "127.0.0.1:$CSharpPlayerPort", '--insecure', '--api-key', 'dev-key-client', 'leaderboard.topScores')
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

    "--- [C2] raw --insecure --api-key dev-key-client characters.create, NO token -> exit 1 + Unauthorized ---" | Out-File -Append $log
    $c2 = Invoke-GbClient @('raw', '--addr', "127.0.0.1:$CSharpPlayerPort", '--insecure', '--api-key', 'dev-key-client', 'characters.create', '{"name":"x","class":""}')
    "    -> rc=$($c2.Rc)  $($c2.Out)" | Out-File -Append $log
    if ($c2.Rc -ne 1 -or $c2.Out -notmatch 'Unauthorized') {
        "    C2 FAIL: expected exit 1 + Unauthorized, got rc=$($c2.Rc) $($c2.Out)" | Out-File -Append $log
        $status = 'FAIL'
    }

    "--- [C3] raw --insecure --api-key dev-key-client --token bogus characters.ownerOf -> exit 1 + NotFound ---" | Out-File -Append $log
    $c3 = Invoke-GbClient @('raw', '--addr', "127.0.0.1:$CSharpPlayerPort", '--insecure', '--api-key', 'dev-key-client', '--token', 'bogus', 'characters.ownerOf', '{"character_id":"z"}')
    "    -> rc=$($c3.Rc)  $($c3.Out)" | Out-File -Append $log
    if ($c3.Rc -ne 1 -or $c3.Out -notmatch 'NotFound') {
        "    C3 FAIL: expected exit 1 + NotFound, got rc=$($c3.Rc) $($c3.Out)" | Out-File -Append $log
        $status = 'FAIL'
    }

    "--- [C4] flow --insecure --api-key dev-key-client (typed client: register -> create -> list over pure QUIC) ---" | Out-File -Append $log
    $c4 = Invoke-GbClient @('flow', '--addr', "127.0.0.1:$CSharpPlayerPort", '--insecure', '--api-key', 'dev-key-client')
    "    -> rc=$($c4.Rc)  $($c4.Out)" | Out-File -Append $log
    if ($c4.Rc -ne 0) {
        "    C4 FAIL: expected exit 0, got rc=$($c4.Rc)" | Out-File -Append $log
        $status = 'FAIL'
    }

    # ReportId is match.report's REQUIRED idempotency key; per-run-unique (ticks) because
    # match.matches persists across verify runs and a constant id would dedup C6's insert.
    $csharpRid = [DateTime]::UtcNow.Ticks

    "--- [C5] raw --insecure --api-key dev-key-client match.report -> exit 1 + Forbidden (client policy lacks match.report) ---" | Out-File -Append $log
    $c5 = Invoke-GbClient @('raw', '--addr', "127.0.0.1:$CSharpPlayerPort", '--insecure', '--api-key', 'dev-key-client', 'match.report', "{`"ReportId`":`"c5-$csharpRid`",`"Winner`":`"c5-winner`",`"Loser`":`"c5-loser`"}")
    "    -> rc=$($c5.Rc)  $($c5.Out)" | Out-File -Append $log
    if ($c5.Rc -ne 1 -or $c5.Out -notmatch 'Forbidden') {
        "    C5 FAIL: expected exit 1 + Forbidden, got rc=$($c5.Rc) $($c5.Out)" | Out-File -Append $log
        $status = 'FAIL'
    }

    "--- [C6] raw --insecure --api-key dev-key-server match.report -> exit 0 (full policy allows it) ---" | Out-File -Append $log
    $c6 = Invoke-GbClient @('raw', '--addr', "127.0.0.1:$CSharpPlayerPort", '--insecure', '--api-key', 'dev-key-server', 'match.report', "{`"ReportId`":`"c6-$csharpRid`",`"Winner`":`"c6-winner`",`"Loser`":`"c6-loser`"}")
    "    -> rc=$($c6.Rc)  $($c6.Out)" | Out-File -Append $log
    if ($c6.Rc -ne 0) {
        "    C6 FAIL: expected exit 0, got rc=$($c6.Rc) $($c6.Out)" | Out-File -Append $log
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
if ($BlessPublicApi) {
    Invoke-BlessPublicApi
}

Invoke-SimpleStage 'build'   $true 'cargo' @('build', '--workspace')
Invoke-SimpleStage 'clippy'  $true 'cargo' @('clippy', '--workspace', '--all-targets', '--', '-D', 'warnings')
Invoke-SimpleStage 'test'    $true 'cargo' @('test', '--workspace')
Invoke-CargoAuditStage
Invoke-FortressStage
Invoke-CodegenFreshStage
Invoke-SimpleStage 'split-proof' $true 'cargo' @('run', '-q', '-p', 'splitproof')

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
Exit-WithLog $overall
