#Requires -Version 7.0
<#
.SYNOPSIS
    Build ONCE, deploy the jvm-quarkus-sketch either as a single-process monolith or as the
    microservices split (process A = characters/accounts, process B = inventory/admin).

.DESCRIPTION
    Per-service split is now PACKAGING, not profiles: each topology is its OWN fast-jar that links only
    its own modules (mirroring the Go backend's cmd/<svc> entrypoints).
      * monolith      = app/build/quarkus-app/quarkus-run.jar                 (all impls, local producers, roles=all)
      * characters-service/build/quarkus-app/quarkus-run.jar   = split process A (accounts+characters, edge QUIC server)
      * inventory-service/build/quarkus-app/quarkus-run.jar    = split process B (inventory+admin, characters-client remote producer)
    Each service jar carries its OWN baked-in application.properties (the old %characters / %inventory
    profiles). Runtime coordinates (ports + INVENTORY_ADDR/CHARACTERS_ADDR/CHARACTERS_EDGE_ADDR/
    EDGE_CERT_THUMBPRINT) are still env vars fed to `java -jar`. Async events are broker-less HTTP
    fanout (the characters relay POSTs to the inventory sink). No broker.

.PARAMETER Mode
    'monolith' (default) = 1 JVM (app jar), roles=all, port 8090.
    'microservices'      = 2 JVMs per $topology below (A=characters-service on 8080 + QUIC 9100,
                           B=inventory-service on 8081). No broker.

.PARAMETER SkipBuild   Reuse the existing quarkus-run.jar (skip `gradlew quarkusBuild`).
.PARAMETER SkipInfra   Do not touch docker compose (assume Postgres already up).
.PARAMETER Teardown    Stop everything launched by a previous run (from run/pids.json) and `compose down`.
.PARAMETER WithPostgres  Also start the compose `postgres` service. Opt-in: the sketch normally assumes a
                         LOCAL Postgres already listening on 5432 (its dev DB); starting the compose one
                         would clash on that port. Use this only on a machine without a local 5432.
.PARAMETER DatabaseUrl JDBC URL passed as DATABASE_URL to every JVM. Defaults to the local dev DB.

.EXAMPLE
    ./install.ps1                         # monolith on localhost:8090
    ./install.ps1 -Mode microservices     # A=8090 (characters) + B=8091 (inventory/admin), no broker
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
$compose = Join-Path $root 'infra/docker-compose.yml'

# Per-mode fast-jars. The monolith is app; each split process is its OWN service jar linking only its
# own modules (proven by :inventory-service:dependencies excluding the characters/accounts impl).
$monolithJar = 'app/build/quarkus-app/quarkus-run.jar'
$jar = Join-Path $root $monolithJar   # readiness/existence check for monolith mode

# --- Topology: process -> its OWN fast-jar (split mode only). Each service jar bakes in its roles +
#     Stork/admin-data/edge config; here we only supply the runtime coordinates (ports +
#     INVENTORY_ADDR/CHARACTERS_ADDR/CHARACTERS_EDGE_ADDR/EDGE_CERT_THUMBPRINT). No QUARKUS_PROFILE. ---
$topology = @(
    @{ name = 'characters'; jar = 'characters-service/build/quarkus-app/quarkus-run.jar'; httpPort = 8080 }  # A: edge QUIC server (characters.ownerOf, :9100) + outbox HTTP fanout -> B + admin-data REST
    @{ name = 'inventory';  jar = 'inventory-service/build/quarkus-app/quarkus-run.jar';  httpPort = 8081 }  # B: event-sink consumer + edge QUIC client -> A (:9100) + admin fan-out
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

# Launch one Quarkus JVM. PowerShell 7 has no per-invocation env dict for Start-Process, so we set the
# process-scoped $env:* keys right before launching (they are inherited by the child), then reset them.
# stdout/stderr are redirected to run/<logName>.*.log. Returns the started Process object.
function Start-Jvm {
    param(
        [hashtable]$EnvHash,
        [string]$LogName,
        [string]$JavaExe,
        [string]$JarPath   # per-process fast-jar: monolith=app, split=characters-service/inventory-service
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
            -ArgumentList '--enable-native-access=ALL-UNNAMED', '-jar', $JarPath `
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
        # Only Postgres remains in compose (broker-less); `down` stops it if -WithPostgres started it.
        docker compose -f $compose down 2>&1 | Out-Host
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

# --- Build phase (per-mode: monolith builds `app`; split builds the TWO service jars) --------------
if (-not $SkipBuild) {
    $env:JAVA_HOME = Split-Path (Split-Path $javaExe -Parent) -Parent
    if ($Mode -eq 'monolith') {
        Write-Host "-- Building monolith (gradlew :app:quarkusBuild) --" -ForegroundColor Cyan
        & (Join-Path $root 'gradlew.bat') ':app:quarkusBuild'
        if ($LASTEXITCODE -ne 0) { throw "gradlew :app:quarkusBuild failed (exit $LASTEXITCODE)." }
    }
    else {
        Write-Host "-- Building services (gradlew :characters-service:quarkusBuild :inventory-service:quarkusBuild) --" -ForegroundColor Cyan
        & (Join-Path $root 'gradlew.bat') ':characters-service:quarkusBuild' ':inventory-service:quarkusBuild'
        if ($LASTEXITCODE -ne 0) { throw "gradlew service quarkusBuild failed (exit $LASTEXITCODE)." }
    }
}
# Verify the jar(s) the chosen mode needs actually exist.
if ($Mode -eq 'monolith') {
    if (-not (Test-Path $jar)) { throw "Runnable jar not found at $jar. Run without -SkipBuild first." }
}
else {
    foreach ($spec in $topology) {
        $svcJar = Join-Path $root $spec.jar
        if (-not (Test-Path $svcJar)) { throw "Runnable jar not found at $svcJar. Run without -SkipBuild first." }
    }
}

# --- Infra phase -----------------------------------------------------------------------------------
# The sketch assumes a LOCAL Postgres on 5432 (its dev DB). -WithPostgres opts into the compose one
# (for machines lacking a local 5432; it would otherwise clash on that port).
if (-not $SkipInfra) {
    # No broker in either mode — async is broker-less HTTP fanout. The only backing service is
    # Postgres, which the sketch assumes is already local on 5432; -WithPostgres opts into the
    # compose one (for machines lacking a local 5432).
    if ($WithPostgres) {
        Write-Host "-- Infra: Postgres (compose) --" -ForegroundColor Cyan
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
    Write-Host "-- Launching monolith (app jar, roles=all, port 8090) --" -ForegroundColor Cyan
    # The app jar bakes roles=all: local PlayerCharacters producer, in-process fanout, local admin.
    $proc = Start-Jvm -JavaExe $javaExe -LogName 'monolith' -JarPath $monolithJar -EnvHash @{
        DATABASE_URL      = $DatabaseUrl
        QUARKUS_HTTP_PORT = '8090'
    }
    $launched += @{ name = 'monolith'; Process = $proc; httpPort = 8090 }
}
else {
    Write-Host "-- Launching microservices split --" -ForegroundColor Cyan
    # The edge QUIC server (process A, characters.ownerOf on :9100) needs a CurrentUser-store cert;
    # ensure-cert.ps1 is idempotent (reuses "GameBackend-Edge" if present) and prints ONLY the
    # thumbprint as its last stdout line (status goes to Write-Host).
    Write-Host "-- Ensuring edge QUIC cert --" -ForegroundColor Cyan
    $thumb = (& (Join-Path $root 'scripts/ensure-cert.ps1') | Select-Object -Last 1)
    Write-Host "  cert thumbprint: $thumb"

    foreach ($spec in $topology) {
        # No QUARKUS_PROFILE — each service jar bakes in its own roles/config. Only runtime coordinates here.
        $envHash = @{
            QUARKUS_HTTP_PORT = "$($spec.httpPort)"
            DATABASE_URL      = $DatabaseUrl
        }
        # Process A (characters-service) POSTs its outbox events to process B's inventory sink;
        # INVENTORY_ADDR redirects the baked events.subscribers.* URLs from the default to B (:8081). It
        # also runs the edge QUIC server for characters.ownerOf on :9100, secured by EDGE_CERT_THUMBPRINT.
        if ($spec.name -eq 'characters') {
            $envHash['INVENTORY_ADDR'] = 'localhost:8081'
            $envHash['EDGE_CERT_THUMBPRINT'] = $thumb
        }
        # Process B (inventory-service) reaches process A over Stork for admin fan-out REST
        # (CHARACTERS_ADDR feeds the static Stork address-list) AND dials A's edge QUIC server directly
        # for the sync characters.ownerOf capability (CHARACTERS_EDGE_ADDR, :9100).
        if ($spec.name -eq 'inventory') {
            $envHash['CHARACTERS_ADDR'] = 'localhost:8080'
            $envHash['CHARACTERS_EDGE_ADDR'] = 'localhost:9100'
        }

        $proc = Start-Jvm -JavaExe $javaExe -LogName $spec.name -JarPath $spec.jar -EnvHash $envHash
        $launched += @{ name = $spec.name; Process = $proc; httpPort = $spec.httpPort }
        Write-Host "  launched $($spec.name) (jar=$($spec.jar), port=$($spec.httpPort), pid=$($proc.Id))"
    }
}

# Persist PIDs so a later `-Teardown` from a fresh shell can stop these processes.
$pidRecords = $launched | ForEach-Object { @{ name = $_.name; pid = $_.Process.Id; httpPort = $_.httpPort } }
$pidRecords | ConvertTo-Json -AsArray | Set-Content -Path $pidsFile -Encoding utf8

# --- Readiness phase -------------------------------------------------------------------------------
# In split mode process B depends on A (edge QUIC/admin), so poll A first — the $launched order already
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
    Write-Host "  Process A (characters-service): http://localhost:8080   (POST /characters, edge QUIC ownerOf server :9100, /admin-data/characters, outbox POSTs to B)"
    Write-Host "  Process B (inventory-service):  http://localhost:8081   (/events sink, edge QUIC ownerOf client -> A:9100, /admin fans out to A)"
    Write-Host "  Drive the flow: POST http://localhost:8080/characters?name=Aria  -> A POSTs the event to B's sink, which grants a starter"
}
Write-Host "  Logs:       run/*.out.log / run/*.err.log"
Write-Host "  Tear down:  ./install.ps1 -Teardown"
