# run.ps1 -- build the rust-sketch binaries, then run either the monolith (one
# process hosting every module) or the two-process split (characters-svc = A,
# inventory-svc = B), where each service is its own binary linking only its own
# modules. PowerShell 5.1 compatible: ASCII only, no em-dashes.
#
# Usage:
#   .\run.ps1                    # monolith (server) on :8080
#   .\run.ps1 microservices      # A (characters-svc) + B (inventory-svc)
#   .\run.ps1 -Teardown          # stop whatever run.ps1 started last
#
# Assumes a local Postgres is already running (DATABASE_URL or the default DSN).

[CmdletBinding()]
param(
    [ValidateSet('monolith', 'microservices')]
    [string]$Mode = 'monolith',
    [switch]$Teardown
)

$ErrorActionPreference = 'Stop'
Set-Location -Path $PSScriptRoot

$RunDir = Join-Path $PSScriptRoot 'run'
$PidsFile = Join-Path $RunDir 'pids.txt'
$BinDir = Join-Path $PSScriptRoot 'target\debug'

$DefaultDsn = 'postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable'
if (-not $env:DATABASE_URL -or $env:DATABASE_URL.Trim() -eq '') { $env:DATABASE_URL = $DefaultDsn }

# --- Teardown ---------------------------------------------------------------
if ($Teardown) {
    if (-not (Test-Path $PidsFile)) { Write-Host "No $PidsFile -- nothing to tear down."; return }
    foreach ($line in Get-Content $PidsFile) {
        if ($line.Trim() -eq '') { continue }
        $parts = $line.Split('=', 2)
        $name = $parts[0]; $procId = [int]$parts[1]
        $p = Get-Process -Id $procId -ErrorAction SilentlyContinue
        if ($p) { Stop-Process -Id $procId -Force; Write-Host "Stopped $name (pid $procId)" }
        else { Write-Host "$name (pid $procId) was not running" }
    }
    Remove-Item $PidsFile -Force
    Write-Host 'Teardown complete.'
    return
}

New-Item -ItemType Directory -Force -Path $RunDir | Out-Null

# start_server NAME EXE ENV-HASHTABLE -- launches EXE in the background with the
# given env vars, redirecting stdout/stderr to run\<name>.{out,err}.log. Returns pid.
$script:StartedPids = @()
$script:StartedNames = @()
function Start-Svc([string]$Name, [string]$Exe, [hashtable]$Env) {
    foreach ($k in $Env.Keys) { Set-Item -Path "Env:$k" -Value $Env[$k] }
    $out = Join-Path $RunDir "$Name.out.log"
    $err = Join-Path $RunDir "$Name.err.log"
    $p = Start-Process -FilePath $Exe -NoNewWindow -PassThru `
        -RedirectStandardOutput $out -RedirectStandardError $err
    $script:StartedPids += $p.Id
    $script:StartedNames += $Name
    return $p.Id
}

function Wait-Healthy([int]$Port, [string]$Name) {
    $url = "http://localhost:$Port/healthz"
    for ($i = 0; $i -lt 60; $i++) {
        try {
            Invoke-WebRequest -UseBasicParsing -Uri $url -TimeoutSec 2 | Out-Null
            Write-Host "$Name healthy at $url"
            return
        } catch { Start-Sleep -Milliseconds 500 }
    }
    throw "$Name did not become healthy at $url within ~30s"
}

function Write-Pids {
    $lines = for ($i = 0; $i -lt $script:StartedNames.Count; $i++) {
        "$($script:StartedNames[$i])=$($script:StartedPids[$i])"
    }
    Set-Content -Path $PidsFile -Value $lines
}

# --- Build ------------------------------------------------------------------
# Both modes build edgeca + playercli: the monolith ALSO fronts players over QUIC
# (PLAYER_EDGE_ADDR), so it needs the shared dev CA (edgeca) and a client (playercli).
if ($Mode -eq 'monolith') {
    Write-Host 'Building server (monolith) + edgeca + playercli ...'
    cargo build -p server -p edgeca -p playercli
} else {
    Write-Host 'Building edgeca + characters-svc + inventory-svc + gateway-svc + playercli ...'
    cargo build -p edgeca -p characters-svc -p inventory-svc -p gateway-svc -p playercli
}
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
Write-Host 'Build OK.'

# --- Monolith ---------------------------------------------------------------
if ($Mode -eq 'monolith') {
    # The monolith ALSO serves the QUIC player front (PLAYER_EDGE_ADDR=:9100, all ops
    # Local) -- per never-monolith-only-features both topologies serve the feature. It
    # needs the shared dev CA to derive the player-front server cert, so mint one here.
    $CaCert = Join-Path $RunDir 'edge-ca.crt'
    $CaKey = Join-Path $RunDir 'edge-ca.key'
    Write-Host "Minting edge dev CA (player front) -> $CaCert ..."
    & (Join-Path $BinDir 'edgeca.exe') --cert $CaCert --key $CaKey
    if ($LASTEXITCODE -ne 0) { throw 'edgeca failed' }
    Start-Svc 'monolith' (Join-Path $BinDir 'server.exe') @{
        PORT             = ':8080'
        DATABASE_URL     = $env:DATABASE_URL
        PLAYER_EDGE_ADDR = ':9100'
        EDGE_CA_CERT     = $CaCert
        EDGE_CA_KEY      = $CaKey
        # default MESSAGING_ORIGIN ("monolith") is fine -- one process, one origin.
    } | Out-Null
    Wait-Healthy 8080 'monolith'
    Write-Pids
    Write-Host ''
    Write-Host '=== monolith running ==='
    Write-Host '  http://localhost:8080  (player QUIC :9100)'
    Write-Host "  logs: $RunDir\monolith.out.log, $RunDir\monolith.err.log"
    Write-Host '  teardown: .\run.ps1 -Teardown'
    return
}

# --- Microservices ----------------------------------------------------------
# Mint ONE shared dev CA for the edge mutual-TLS hop. Both A and B load it via
# EDGE_CA_CERT / EDGE_CA_KEY, so a backend accepts a stream ONLY from a peer holding
# a CA-signed client cert (and each client verifies the server against the same root).
$CaCert = Join-Path $RunDir 'edge-ca.crt'
$CaKey = Join-Path $RunDir 'edge-ca.key'
Write-Host "Minting shared edge dev CA -> $CaCert ..."
& (Join-Path $BinDir 'edgeca.exe') --cert $CaCert --key $CaKey
if ($LASTEXITCODE -ne 0) { throw 'edgeca failed' }

# Process A: characters-svc. Hosts the QUIC edge server (:9000) and the outbox relay
# for character.* events. MESSAGING_ORIGIN MUST be distinct per process (never the
# "monolith" default): the relay drains ONLY its own origin's outbox rows, so a shared
# origin would have B's relay drain A's rows -- the async-split correctness lynchpin.
# Started FIRST so B's remote stub + the front door can reach it.
Write-Host 'Starting A (characters-svc: gateway,characters,messaging) on :8080, edge :9000 ...'
Start-Svc 'characters' (Join-Path $BinDir 'characters-svc.exe') @{
    PORT               = ':8080'
    DATABASE_URL       = $env:DATABASE_URL
    EDGE_ADDR          = ':9000'
    EDGE_CA_CERT       = $CaCert
    EDGE_CA_KEY        = $CaKey
    MESSAGING_ORIGIN   = 'characters-svc'
    EVENTS_SUBSCRIBERS = 'character.created=http://localhost:8081/events;character.deleted=http://localhost:8081/events'
} | Out-Null
Wait-Healthy 8080 'A (characters-svc)'

# Process B: inventory-svc. characters resolves via a remote::Stub dialing A's edge
# server. B ALSO serves its OWN mTLS edge (EDGE_ADDR=:9001) so gateway-svc can dispatch
# inventory.* Remote to it. CHARACTERS_EDGE_ADDR is a NUMERIC host:port (Rust's
# SocketAddr needs a literal IP, unlike Go's dialer).
Write-Host 'Starting B (inventory-svc: gateway,config,inventory,messaging,remote) on :8081, edge :9001 ...'
Start-Svc 'inventory' (Join-Path $BinDir 'inventory-svc.exe') @{
    PORT                 = ':8081'
    DATABASE_URL         = $env:DATABASE_URL
    EDGE_ADDR            = ':9001'
    EDGE_CA_CERT         = $CaCert
    EDGE_CA_KEY          = $CaKey
    CHARACTERS_EDGE_ADDR = '127.0.0.1:9000'
    MESSAGING_ORIGIN     = 'inventory-svc'
} | Out-Null
Wait-Healthy 8081 'B (inventory-svc)'

# Process G: gateway-svc. The dedicated front door -- HTTP :8082 + player QUIC :9100.
# No DB, no provider modules: only remote::Stubs, so EVERY op it fronts resolves Remote
# and is dialed over the mTLS edge to A (:9000) / B (:9001). It needs the shared CA to
# dial peers AND to derive the player-front server cert.
Write-Host 'Starting G (gateway-svc: gateway + characters/inventory stubs) on :8082, player QUIC :9100 ...'
Start-Svc 'gateway' (Join-Path $BinDir 'gateway-svc.exe') @{
    PORT                 = ':8082'
    PLAYER_EDGE_ADDR     = ':9100'
    EDGE_CA_CERT         = $CaCert
    EDGE_CA_KEY          = $CaKey
    CHARACTERS_EDGE_ADDR = '127.0.0.1:9000'
    INVENTORY_EDGE_ADDR  = '127.0.0.1:9001'
} | Out-Null
Wait-Healthy 8082 'G (gateway-svc)'

Write-Pids
Write-Host ''
Write-Host '=== microservices running ==='
Write-Host '  A (characters-svc): http://localhost:8080  (edge :9000)'
Write-Host '  B (inventory-svc):  http://localhost:8081  (edge :9001)'
Write-Host '  G (gateway-svc):    http://localhost:8082  (player QUIC :9100)'
Write-Host "  logs: $RunDir\characters.{out,err}.log, $RunDir\inventory.{out,err}.log, $RunDir\gateway.{out,err}.log"
Write-Host '  teardown: .\run.ps1 -Teardown'
