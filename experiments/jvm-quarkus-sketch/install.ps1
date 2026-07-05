#Requires -Version 7.0
<#
.SYNOPSIS
    Build ONCE, deploy the jvm-quarkus-sketch either as a single-process monolith or as the
    microservices split (process A = characters/accounts, process B = inventory/admin).

.DESCRIPTION
    The whole point of the sketch: ONE artifact (app/build/quarkus-app/quarkus-run.jar), two
    topologies chosen purely by environment at launch. The monolith runs with NO Quarkus profile
    (base config, roles=all); each split process runs QUARKUS_PROFILE=<role> (Step 7 profiles),
    which flips channel ends between internal and Kafka and points Stork at process A.

    Nothing here rebuilds per mode. All topology knobs are env vars fed to `java -jar`.

.PARAMETER Mode
    'monolith' (default) = 1 JVM, roles=all, port 8090.
    'microservices'      = 2 JVMs per $topology below (A on 8090, B on 8091) + Redpanda broker.

.PARAMETER SkipBuild   Reuse the existing quarkus-run.jar (skip `gradlew quarkusBuild`).
.PARAMETER SkipInfra   Do not touch docker compose (assume Postgres / Redpanda already up).
.PARAMETER Teardown    Stop everything launched by a previous run (from run/pids.json) and `compose down`.
.PARAMETER WithPostgres  Also start the compose `postgres` service. Opt-in: the sketch normally assumes a
                         LOCAL Postgres already listening on 5432 (its dev DB); starting the compose one
                         would clash on that port. Use this only on a machine without a local 5432.
.PARAMETER DatabaseUrl JDBC URL passed as DATABASE_URL to every JVM. Defaults to the local dev DB.

.EXAMPLE
    ./install.ps1                         # monolith on localhost:8090
    ./install.ps1 -Mode microservices     # A=8090 (characters) + B=8091 (inventory/admin) + Redpanda
    ./install.ps1 -Teardown               # stop whatever a prior run started
#>
[CmdletBinding()]
param(
    [ValidateSet('monolith', 'microservices')]
    [string]$Mode = 'monolith',
    [switch]$SkipBuild,
    [switch]$SkipInfra,
    [switch]$Teardown,
    [switch]$WithPostgres,
    [string]$DatabaseUrl = 'jdbc:postgresql://localhost:5432/jvmsketch'
)

$ErrorActionPreference = 'Stop'

# Anchor every path to this script's directory so the script works from any CWD.
$root = $PSScriptRoot
$runDir = Join-Path $root 'run'
$pidsFile = Join-Path $runDir 'pids.json'
$jar = Join-Path $root 'app/build/quarkus-app/quarkus-run.jar'
$compose = Join-Path $root 'infra/docker-compose.yml'

# The runnable artifact is identical for both modes — env alone selects the topology.
$jarArg = 'app/build/quarkus-app/quarkus-run.jar'

# --- Topology: role -> process (split mode only). Matches the Step 7 %<profile> config and the
#     plan's "Mapa role->proces" table. The profile sets `roles` + which channel ends are Kafka;
#     here we only supply the runtime coordinates (port + Stork target for process A). ---------------
$topology = @(
    @{ name = 'characters'; profile = 'characters'; httpPort = 8090 }  # A: gRPC server + Kafka producer + admin-data REST
    @{ name = 'inventory';  profile = 'inventory';  httpPort = 8091 }  # B: Kafka consumer + gRPC client -> A + admin fan-out
)

# ---------------------------------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------------------------------

# Locate a JDK 26 (ported from run.cmd's batch discovery): prefer JAVA_HOME, else scan the IntelliJ
# .jdks download dir for a *26* install that has bin\java.exe. Fails loudly if none is found.
function Resolve-JavaExe {
    if ($env:JAVA_HOME) {
        $candidate = Join-Path $env:JAVA_HOME 'bin/java.exe'
        if (Test-Path $candidate) { return $candidate }
    }
    $jdksRoot = Join-Path $env:USERPROFILE '.jdks'
    if (Test-Path $jdksRoot) {
        $match = Get-ChildItem -Path $jdksRoot -Directory -Filter '*26*' |
            Where-Object { Test-Path (Join-Path $_.FullName 'bin/java.exe') } |
            Select-Object -First 1
        if ($match) { return (Join-Path $match.FullName 'bin/java.exe') }
    }
    throw "No JDK 26 found. Set JAVA_HOME, or install one (winget install EclipseAdoptium.Temurin.26.JDK)."
}

# Block until a TCP port accepts a connection, or $timeoutSec elapses. Used to gate JVM launch on the
# broker being reachable (Redpanda on 9092). Returns $true on success, $false on timeout.
function Wait-ForTcp {
    param(
        [string]$TargetHost,
        [int]$Port,
        [int]$TimeoutSec = 60
    )
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        $client = [System.Net.Sockets.TcpClient]::new()
        try {
            $client.Connect($TargetHost, $Port)
            if ($client.Connected) { return $true }
        }
        catch {
            # Not up yet — swallow and retry after a short pause.
        }
        finally {
            $client.Dispose()
        }
        Start-Sleep -Seconds 1
    }
    return $false
}

# Launch one Quarkus JVM. PowerShell 7 has no per-invocation env dict for Start-Process, so we set the
# process-scoped $env:* keys right before launching (they are inherited by the child), then reset them.
# stdout/stderr are redirected to run/<logName>.*.log. Returns the started Process object.
function Start-Jvm {
    param(
        [hashtable]$EnvHash,
        [string]$LogName,
        [string]$JavaExe
    )
    if (-not (Test-Path $runDir)) { New-Item -ItemType Directory -Path $runDir | Out-Null }

    # Snapshot then set, so we can restore the parent shell's env afterwards.
    $saved = @{}
    foreach ($key in $EnvHash.Keys) {
        $saved[$key] = [System.Environment]::GetEnvironmentVariable($key, 'Process')
        Set-Item -Path "Env:$key" -Value $EnvHash[$key]
    }
    try {
        $outLog = Join-Path $runDir "$LogName.out.log"
        $errLog = Join-Path $runDir "$LogName.err.log"
        return Start-Process -FilePath $JavaExe `
            -ArgumentList '-jar', $jarArg `
            -WorkingDirectory $root `
            -PassThru `
            -RedirectStandardOutput $outLog `
            -RedirectStandardError $errLog
    }
    finally {
        # Restore: null means the key was unset before, so remove it again.
        foreach ($key in $saved.Keys) {
            if ($null -eq $saved[$key]) { Remove-Item -Path "Env:$key" -ErrorAction SilentlyContinue }
            else { Set-Item -Path "Env:$key" -Value $saved[$key] }
        }
    }
}

# Poll GET http://localhost:<port>/q/health/ready until 200 or timeout. Returns $true when ready.
function Wait-ForReady {
    param(
        [int]$Port,
        [int]$TimeoutSec = 60,
        [int]$IntervalSec = 2
    )
    $url = "http://localhost:$Port/q/health/ready"
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        try {
            $resp = Invoke-WebRequest -Uri $url -UseBasicParsing -TimeoutSec 5
            if ($resp.StatusCode -eq 200) { return $true }
        }
        catch {
            # Not ready / connection refused — retry.
        }
        Start-Sleep -Seconds $IntervalSec
    }
    return $false
}

function Read-Pids {
    if (Test-Path $pidsFile) {
        return @(Get-Content $pidsFile -Raw | ConvertFrom-Json)
    }
    return @()
}

# ---------------------------------------------------------------------------------------------------
# Teardown branch — stop prior processes, bring infra down, delete the pid file, and return.
# ---------------------------------------------------------------------------------------------------
if ($Teardown) {
    Write-Host "== Teardown ==" -ForegroundColor Cyan
    foreach ($entry in (Read-Pids)) {
        $procId = [int]$entry.pid
        try {
            Stop-Process -Id $procId -Force -ErrorAction Stop
            Write-Host "  stopped $($entry.name) (pid $procId)"
        }
        catch {
            Write-Host "  $($entry.name) (pid $procId) already gone"
        }
    }
    if (Test-Path $compose) {
        # `down` with the profile so the profiled `redpanda` service is included; harmless in monolith.
        docker compose -f $compose --profile microservices down 2>&1 | Out-Host
    }
    if (Test-Path $pidsFile) { Remove-Item $pidsFile -Force }
    Write-Host "Teardown complete." -ForegroundColor Green
    return
}

# ---------------------------------------------------------------------------------------------------
# Main deploy flow. Track launched processes so a mid-launch failure doesn't orphan JVMs (trap below).
# ---------------------------------------------------------------------------------------------------
$launched = @()

trap {
    Write-Host "`n[!] Error during launch: $_" -ForegroundColor Red
    foreach ($p in $launched) {
        try { Stop-Process -Id $p.Process.Id -Force -ErrorAction Stop; Write-Host "  cleaned up pid $($p.Process.Id)" }
        catch { }
    }
    break
}

Write-Host "== jvm-quarkus-sketch install ($Mode) ==" -ForegroundColor Cyan
$javaExe = Resolve-JavaExe
Write-Host "Using JDK: $javaExe"

# --- Build phase (one artifact for BOTH modes) -----------------------------------------------------
if (-not $SkipBuild) {
    Write-Host "-- Building (gradlew quarkusBuild) --" -ForegroundColor Cyan
    $env:JAVA_HOME = Split-Path (Split-Path $javaExe -Parent) -Parent
    & (Join-Path $root 'gradlew.bat') quarkusBuild
    if ($LASTEXITCODE -ne 0) { throw "gradlew quarkusBuild failed (exit $LASTEXITCODE)." }
}
if (-not (Test-Path $jar)) {
    throw "Runnable jar not found at $jar. Run without -SkipBuild first."
}

# --- Infra phase -----------------------------------------------------------------------------------
# The sketch assumes a LOCAL Postgres on 5432 (its dev DB). -WithPostgres opts into the compose one
# (for machines lacking a local 5432; it would otherwise clash on that port).
if (-not $SkipInfra) {
    if ($Mode -eq 'microservices') {
        Write-Host "-- Infra: Redpanda broker (+Postgres if -WithPostgres) --" -ForegroundColor Cyan
        $svc = @('redpanda')
        if ($WithPostgres) { $svc += 'postgres' }
        docker compose -f $compose --profile microservices up -d @svc
        if ($LASTEXITCODE -ne 0) { throw "docker compose up failed (exit $LASTEXITCODE)." }
        Write-Host "  waiting for Redpanda (localhost:9092)..."
        if (-not (Wait-ForTcp -TargetHost 'localhost' -Port 9092 -TimeoutSec 60)) {
            throw "Redpanda did not come up on localhost:9092 within 60s."
        }
        Write-Host "  Redpanda is up." -ForegroundColor Green
    }
    elseif ($WithPostgres) {
        Write-Host "-- Infra: Postgres (monolith) --" -ForegroundColor Cyan
        docker compose -f $compose up -d postgres
        if ($LASTEXITCODE -ne 0) { throw "docker compose up postgres failed (exit $LASTEXITCODE)." }
    }
    else {
        Write-Host "-- Infra: assuming local Postgres at $DatabaseUrl (use -WithPostgres to start the compose one) --"
    }
}

# --- Launch phase ----------------------------------------------------------------------------------
if (-not (Test-Path $runDir)) { New-Item -ItemType Directory -Path $runDir | Out-Null }

if ($Mode -eq 'monolith') {
    Write-Host "-- Launching monolith (roles=all, port 8090) --" -ForegroundColor Cyan
    # No QUARKUS_PROFILE => base config: roles=all, internal channels, local PlayerCharacters branch.
    $proc = Start-Jvm -JavaExe $javaExe -LogName 'monolith' -EnvHash @{
        DATABASE_URL      = $DatabaseUrl
        QUARKUS_HTTP_PORT = '8090'
    }
    $launched += @{ name = 'monolith'; Process = $proc; httpPort = 8090 }
}
else {
    Write-Host "-- Launching microservices split --" -ForegroundColor Cyan
    foreach ($spec in $topology) {
        $envHash = @{
            QUARKUS_PROFILE         = $spec.profile
            QUARKUS_HTTP_PORT       = "$($spec.httpPort)"
            DATABASE_URL            = $DatabaseUrl
            KAFKA_BOOTSTRAP_SERVERS = 'localhost:9092'
        }
        # Process B (inventory) reaches process A (characters) over Stork for gRPC ownerOf AND admin
        # fan-out REST; CHARACTERS_ADDR feeds the static Stork address-list (%inventory profile).
        if ($spec.name -eq 'inventory') { $envHash['CHARACTERS_ADDR'] = 'localhost:8090' }

        $proc = Start-Jvm -JavaExe $javaExe -LogName $spec.name -EnvHash $envHash
        $launched += @{ name = $spec.name; Process = $proc; httpPort = $spec.httpPort }
        Write-Host "  launched $($spec.name) (profile=$($spec.profile), port=$($spec.httpPort), pid=$($proc.Id))"
    }
}

# Persist PIDs so a later `-Teardown` from a fresh shell can stop these processes.
$pidRecords = $launched | ForEach-Object { @{ name = $_.name; pid = $_.Process.Id; httpPort = $_.httpPort } }
$pidRecords | ConvertTo-Json -AsArray | Set-Content -Path $pidsFile -Encoding utf8

# --- Readiness phase -------------------------------------------------------------------------------
# In split mode process B depends on A (gRPC/admin), so poll A first — the $launched order already
# has A ('characters') before B ('inventory'), matching $topology.
Write-Host "-- Readiness (/q/health/ready) --" -ForegroundColor Cyan
foreach ($entry in $launched) {
    Write-Host "  waiting for $($entry.name) on :$($entry.httpPort)..."
    if (Wait-ForReady -Port $entry.httpPort -TimeoutSec 60 -IntervalSec 2) {
        Write-Host "  $($entry.name) UP (http://localhost:$($entry.httpPort))" -ForegroundColor Green
    }
    else {
        Write-Host "  $($entry.name) TIMED OUT — see run/$($entry.name).err.log" -ForegroundColor Yellow
    }
}

# --- Summary ---------------------------------------------------------------------------------------
Write-Host ""
Write-Host "== Ready ($Mode) ==" -ForegroundColor Green
if ($Mode -eq 'monolith') {
    Write-Host "  Admin:      http://localhost:8090/admin"
    Write-Host "  Characters: POST http://localhost:8090/characters?name=Aria"
}
else {
    Write-Host "  Process A (characters/accounts): http://localhost:8090   (POST /characters, gRPC ownerOf, /admin-data/characters)"
    Write-Host "  Process B (inventory/admin):     http://localhost:8091   (/admin fans out to A)"
    Write-Host "  Drive the flow: POST http://localhost:8090/characters?name=Aria  -> inventory in B grants a starter"
}
Write-Host "  Logs:       run/*.out.log / run/*.err.log"
Write-Host "  Tear down:  ./install.ps1 -Teardown"
