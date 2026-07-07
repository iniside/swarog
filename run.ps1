# run.ps1 - build the per-service binaries, then run either the monolith (one
# binary hosting every module) or the two-process microservices split, where
# EACH service is its OWN binary linking only its own modules.
#
# Usage:
#   .\run.ps1                              # monolith (bin/server.exe), default DB
#   .\run.ps1 -Mode microservices           # characters-svc + inventory-svc (two binaries)
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
$serverBin = Join-Path $binDir 'server.exe'
$charactersBin = Join-Path $binDir 'characters-svc.exe'
$inventoryBin = Join-Path $binDir 'inventory-svc.exe'
$schedulerBin = Join-Path $binDir 'scheduler-svc.exe'
$gatewayBin = Join-Path $binDir 'gateway-svc.exe'

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

# --- Build --------------------------------------------------------------------
New-Item -ItemType Directory -Force -Path $binDir | Out-Null
New-Item -ItemType Directory -Force -Path $runDir | Out-Null

# Build only the binaries this mode needs. Each `go build` links ONLY the
# packages its entrypoint imports — the microservice binaries do not carry the
# other service's modules.
function Build-Bin {
    param([string]$Pkg, [string]$Out)
    Write-Host "Building $Pkg -> $Out ..." -ForegroundColor Cyan
    & go build -o $Out $Pkg
    if ($LASTEXITCODE -ne 0) {
        Write-Error "go build $Pkg failed — aborting."
        exit 1
    }
}

if ($Mode -eq 'monolith') {
    Build-Bin './cmd/server' $serverBin
} else {
    Build-Bin './cmd/characters-svc' $charactersBin
    Build-Bin './cmd/inventory-svc' $inventoryBin
    Build-Bin './cmd/scheduler-svc' $schedulerBin
    Build-Bin './cmd/gateway-svc' $gatewayBin
}
Write-Host "Build OK." -ForegroundColor Green

# --- Helpers ---------------------------------------------------------------

# Start-Server launches the given binary with the given env hash, redirecting
# stdout/stderr to run/<logName>.{out,err}.log. Returns the Process object.
function Start-Server {
    param(
        [string]$BinPath,
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
        $proc = Start-Process -FilePath $BinPath `
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
        PORT         = '8080'
        DATABASE_URL = $DatabaseUrl
    }
    $proc = Start-Server -BinPath $serverBin -EnvHash $env1 -LogName 'monolith'
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
# Mint ONE shared dev CA for the edge mutual-TLS hop. Every edge process
# (characters-svc + inventory-svc + gateway-svc) mints its own short-lived leaf
# under THIS CA, so a backend accepts a stream ONLY from a peer holding a
# CA-signed client cert (and each client verifies the server against the same
# anchor). scheduler-svc has NO edge (no server, no client) so it needs no CA;
# the monolith runs no edge at all and sets nothing.
$edgeCaCert = Join-Path $runDir 'edge-ca.crt'
$edgeCaKey = Join-Path $runDir 'edge-ca.key'
Write-Host "Minting shared edge dev CA -> $edgeCaCert ..." -ForegroundColor Cyan
& go run ./tools/edgeca -cert $edgeCaCert -key $edgeCaKey
if ($LASTEXITCODE -ne 0) {
    Write-Error "minting edge dev CA failed — aborting."
    exit 1
}

# Process A: characters-svc (accounts + characters, its OWN binary). Hosts the
# QUIC edge server (:9000) and the outbox relay for character.* events. Started
# FIRST — B's remote stubs and the shared accounts schema migration must not
# race A's first boot (S7).
$envA = @{
    PORT               = '8080'
    DATABASE_URL       = $DatabaseUrl
    EDGE_ADDR          = ':9000'
    EDGE_CA_CERT       = $edgeCaCert
    EDGE_CA_KEY        = $edgeCaKey
    MESSAGING_ORIGIN   = 'characters-svc'
    # EVENTS_SUBSCRIBERS is read by messaging's relay, which runs in the
    # process hosting `characters` — i.e. THIS process (A), not B — because
    # the relay drains only ITS OWN origin's rows in messaging.outbox (origin=
    # characters-svc) and delivers them to remote peers. Both topics point at
    # B's single consolidated inbound route (POST /events, topic in the
    # X-Event-Topic header) — there is no more per-topic sink path.
    # MESSAGING_ORIGIN must be stable across restarts (never a pid/hostname)
    # so a crashed process resumes draining its own unsent outbox rows.
    EVENTS_SUBSCRIBERS = 'character.created=http://localhost:8081/events;character.deleted=http://localhost:8081/events'
}
Write-Host "Starting A (characters-svc: accounts,characters) on :8080, edge :9000 ..." -ForegroundColor Cyan
$procA = Start-Server -BinPath $charactersBin -EnvHash $envA -LogName 'characters'
$started += @{ name = 'characters'; proc = $procA }
Wait-Healthy -Port 8080 -Name 'A (characters-svc)'

# Process B: inventory-svc (inventory + admin, its OWN binary). Its accounts/
# characters dependencies resolve via remote stubs dialing A's edge server;
# admin fan-out reaches A's adminData operation over that SAME QUIC edge (no HTTP).
$envB = @{
    PORT                 = '8081'
    DATABASE_URL         = $DatabaseUrl
    EDGE_ADDR            = ':9001'
    EDGE_CA_CERT         = $edgeCaCert
    EDGE_CA_KEY          = $edgeCaKey
    CHARACTERS_EDGE_ADDR = 'localhost:9000'
    ACCOUNTS_EDGE_ADDR   = 'localhost:9000'
    MESSAGING_ORIGIN     = 'inventory-svc'
}
Write-Host "Starting B (inventory-svc: inventory,admin) on :8081, edge :9001 ..." -ForegroundColor Cyan
$procB = Start-Server -BinPath $inventoryBin -EnvHash $envB -LogName 'inventory'
$started += @{ name = 'inventory'; proc = $procB }
Wait-Healthy -Port 8081 -Name 'B (inventory-svc)'

# Process D: scheduler-svc (scheduler ONLY, its OWN binary, no edge). A pure
# event producer: its messaging relay POSTs scheduler.fired to B's consolidated
# POST /events route, where audit (hosted in B) durably consumes it via OnTx.
# Started after B so the sink exists; the relay retries anyway.
$envD = @{
    PORT               = '8083'
    DATABASE_URL       = $DatabaseUrl
    MESSAGING_ORIGIN   = 'scheduler-svc'
    EVENTS_SUBSCRIBERS = 'scheduler.fired=http://localhost:8081/events'
}
Write-Host "Starting D (scheduler-svc: scheduler) on :8083 ..." -ForegroundColor Cyan
$procD = Start-Server -BinPath $schedulerBin -EnvHash $envD -LogName 'scheduler'
$started += @{ name = 'scheduler'; proc = $procD }
Wait-Healthy -Port 8083 -Name 'D (scheduler-svc)'

# Process C: gateway-svc (stateless QUIC prefix router + HTTP reverse proxy
# front door). Fronts both A and B, so it starts LAST, once both are healthy.
$envC = @{
    PORT                  = '8082'
    GATEWAY_EDGE_ADDR     = ':9100'
    EDGE_CA_CERT          = $edgeCaCert
    EDGE_CA_KEY           = $edgeCaKey
    CHARACTERS_EDGE_ADDR  = 'localhost:9000'
    INVENTORY_EDGE_ADDR   = 'localhost:9001'
    CHARACTERS_HTTP_ADDR  = 'localhost:8080'
    INVENTORY_HTTP_ADDR   = 'localhost:8081'
}
Write-Host "Starting C (gateway-svc: player front door) on :8082, edge :9100 ..." -ForegroundColor Cyan
$procC = Start-Server -BinPath $gatewayBin -EnvHash $envC -LogName 'gateway'
$started += @{ name = 'gateway'; proc = $procC }
Wait-Healthy -Port 8082 -Name 'C (gateway-svc)'

$pidEntries = $started | ForEach-Object { @{ name = $_.name; pid = $_.proc.Id } }
$pidEntries | ConvertTo-Json | Set-Content -Path $pidsFile

Write-Host ""
Write-Host "=== microservices running ===" -ForegroundColor Cyan
Write-Host "  A (characters-svc: accounts,characters): http://localhost:8080  (edge :9000)"
Write-Host "  B (inventory-svc: inventory,admin):      http://localhost:8081  (edge :9001)"
Write-Host "  D (scheduler-svc: scheduler):            http://localhost:8083  (event producer, no edge)"
Write-Host "  admin UI (B):                            http://localhost:8081/admin"
Write-Host "  player front door (gateway):              quic localhost:9100 / http http://localhost:8082"
Write-Host "  logs: run/characters.{out,err}.log, run/inventory.{out,err}.log, run/scheduler.{out,err}.log, run/gateway.{out,err}.log"
Write-Host "  teardown: .\run.ps1 -Teardown"
