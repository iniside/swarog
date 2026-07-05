# run.ps1 - build the server once, then run it as a monolith or as the
# accounts+characters / inventory+admin microservices split.
#
# Usage:
#   .\run.ps1                              # monolith, default DB
#   .\run.ps1 -Mode microservices           # A (accounts,characters) + B (inventory,admin)
#   .\run.ps1 -DatabaseUrl "postgres://..." # override DATABASE_URL
#   .\run.ps1 -Teardown                     # stop whatever run.ps1 started last
#
# Assumes a local Postgres is already running (same assumption as run-dev.ps1).

param(
    [ValidateSet('monolith', 'microservices')]
    [string]$Mode = 'monolith',
    [switch]$Teardown,
    [string]$DatabaseUrl = ''
)

$ErrorActionPreference = 'Stop'

$root = $PSScriptRoot
$runDir = Join-Path $root 'run'
$pidsFile = Join-Path $runDir 'pids.json'
$binDir = Join-Path $root 'bin'
$binPath = Join-Path $binDir 'server.exe'

$defaultDSN = 'postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable'
if ([string]::IsNullOrWhiteSpace($DatabaseUrl)) {
    $DatabaseUrl = $defaultDSN
}

# --- Teardown ---------------------------------------------------------------
if ($Teardown) {
    if (-not (Test-Path $pidsFile)) {
        Write-Host "No run/pids.json found — nothing to tear down." -ForegroundColor Yellow
        return
    }
    $entries = Get-Content $pidsFile -Raw | ConvertFrom-Json
    foreach ($entry in $entries) {
        try {
            Stop-Process -Id $entry.pid -Force -ErrorAction Stop
            Write-Host ("Stopped {0} (pid {1})" -f $entry.name, $entry.pid) -ForegroundColor Green
        } catch {
            Write-Host ("{0} (pid {1}) was not running" -f $entry.name, $entry.pid) -ForegroundColor Yellow
        }
    }
    Remove-Item $pidsFile -Force
    Write-Host "Teardown complete." -ForegroundColor Green
    return
}

# --- Build once ---------------------------------------------------------------
New-Item -ItemType Directory -Force -Path $binDir | Out-Null
New-Item -ItemType Directory -Force -Path $runDir | Out-Null

Write-Host "Building ./cmd/server -> $binPath ..." -ForegroundColor Cyan
& go build -o $binPath ./cmd/server
if ($LASTEXITCODE -ne 0) {
    Write-Error "go build failed — aborting."
    exit 1
}
Write-Host "Build OK." -ForegroundColor Green

# --- Helpers ---------------------------------------------------------------

# Start-Server launches bin/server.exe with the given env hash, redirecting
# stdout/stderr to run/<logName>.{out,err}.log. Returns the Process object.
function Start-Server {
    param(
        [hashtable]$EnvHash,
        [string]$LogName
    )

    $outLog = Join-Path $runDir "$LogName.out.log"
    $errLog = Join-Path $runDir "$LogName.err.log"

    # Set process-scoped env vars, launch, then restore — the child process
    # inherits the modified environment at creation time.
    $saved = @{}
    foreach ($key in $EnvHash.Keys) {
        $saved[$key] = [Environment]::GetEnvironmentVariable($key)
        [Environment]::SetEnvironmentVariable($key, $EnvHash[$key])
    }
    try {
        $proc = Start-Process -FilePath $binPath `
            -RedirectStandardOutput $outLog `
            -RedirectStandardError $errLog `
            -PassThru -NoNewWindow
    } finally {
        foreach ($key in $saved.Keys) {
            [Environment]::SetEnvironmentVariable($key, $saved[$key])
        }
    }
    return $proc
}

# Wait-Healthy polls GET http://localhost:<port>/healthz until it returns 200,
# or throws after ~30s.
function Wait-Healthy {
    param(
        [int]$Port,
        [string]$Name,
        [int]$TimeoutSeconds = 30
    )
    $url = "http://localhost:$Port/healthz"
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        try {
            $resp = Invoke-WebRequest -Uri $url -UseBasicParsing -TimeoutSec 2
            if ($resp.StatusCode -eq 200) {
                Write-Host ("{0} healthy at {1}" -f $Name, $url) -ForegroundColor Green
                return
            }
        } catch {
            # not up yet — keep polling
        }
        Start-Sleep -Milliseconds 500
    }
    throw "$Name did not become healthy at $url within ${TimeoutSeconds}s"
}

# --- Launch ---------------------------------------------------------------

$started = @()   # list of @{ name = ...; pid = ... } for run/pids.json

# Stop-Started kills every process already launched this run, in case a later
# step fails mid-launch (e.g. B never comes up).
function Stop-Started {
    foreach ($s in $started) {
        try { Stop-Process -Id $s.proc.Id -Force -ErrorAction Stop } catch {}
    }
}

trap {
    Write-Host "Launch failed — stopping already-started processes." -ForegroundColor Red
    Stop-Started
    throw
}

if ($Mode -eq 'monolith') {
    $env1 = @{
        ROLES        = ''
        PORT         = '8080'
        DATABASE_URL = $DatabaseUrl
    }
    $proc = Start-Server -EnvHash $env1 -LogName 'monolith'
    $started += @{ name = 'monolith'; proc = $proc }
    Wait-Healthy -Port 8080 -Name 'monolith'

    $pidEntries = $started | ForEach-Object { @{ name = $_.name; pid = $_.proc.Id } }
    $pidEntries | ConvertTo-Json | Set-Content -Path $pidsFile

    Write-Host ""
    Write-Host "=== monolith running ===" -ForegroundColor Cyan
    Write-Host "  http://localhost:8080"
    Write-Host "  logs: run/monolith.out.log, run/monolith.err.log"
    Write-Host "  teardown: .\run.ps1 -Teardown"
    return
}

# --- microservices ---------------------------------------------------------
# Process A: accounts + characters. Hosts the QUIC edge server (:9000) and
# the outbox relay for character.* events. Started FIRST — B's remote stubs
# and the shared accounts schema migration must not race A's first boot (S7).
$envA = @{
    ROLES              = 'accounts,characters'
    PORT               = '8080'
    DATABASE_URL       = $DatabaseUrl
    EDGE_ADDR          = ':9000'
    # EVENTS_SUBSCRIBERS is read by the outbox relay, which runs in the
    # process hosting `characters` — i.e. THIS process (A), not B — because
    # the relay drains A's own characters.outbox table to remote sinks. It
    # points at B's synchronous sink endpoints (character-created/-deleted).
    EVENTS_SUBSCRIBERS = 'character.created=http://localhost:8081/events/character-created;character.deleted=http://localhost:8081/events/character-deleted'
}
Write-Host "Starting A (accounts,characters) on :8080, edge :9000 ..." -ForegroundColor Cyan
$procA = Start-Server -EnvHash $envA -LogName 'characters'
$started += @{ name = 'characters'; proc = $procA }
Wait-Healthy -Port 8080 -Name 'A (accounts,characters)'

# Process B: inventory + admin. Its accounts/characters dependencies resolve
# via remote stubs dialing A's edge server; admin fan-out reaches A's
# /admin-data/characters over PEER_HTTP_ADDR.
$envB = @{
    ROLES                = 'inventory,admin'
    PORT                 = '8081'
    DATABASE_URL         = $DatabaseUrl
    CHARACTERS_EDGE_ADDR = 'localhost:9000'
    ACCOUNTS_EDGE_ADDR   = 'localhost:9000'
    PEER_HTTP_ADDR       = 'localhost:8080'
}
Write-Host "Starting B (inventory,admin) on :8081 ..." -ForegroundColor Cyan
$procB = Start-Server -EnvHash $envB -LogName 'inventory'
$started += @{ name = 'inventory'; proc = $procB }
Wait-Healthy -Port 8081 -Name 'B (inventory,admin)'

$pidEntries = $started | ForEach-Object { @{ name = $_.name; pid = $_.proc.Id } }
$pidEntries | ConvertTo-Json | Set-Content -Path $pidsFile

Write-Host ""
Write-Host "=== microservices running ===" -ForegroundColor Cyan
Write-Host "  A (accounts,characters): http://localhost:8080  (edge :9000)"
Write-Host "  B (inventory,admin):     http://localhost:8081"
Write-Host "  admin UI (B):            http://localhost:8081/admin"
Write-Host "  logs: run/characters.{out,err}.log, run/inventory.{out,err}.log"
Write-Host "  teardown: .\run.ps1 -Teardown"
