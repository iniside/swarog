# run.ps1 -- build the rust-sketch binaries, then run either the monolith (one process
# hosting every module) or the FULL split (the 11-service microservice topology, each
# service a binary linking only its own modules and talking to peers over the mTLS QUIC
# edge). The split boot order + per-process env are transcribed from split-proof.ps1
# (the source of truth); unlike that proof this script runs NO assertions and leaves
# every process RUNNING -- teardown is the explicit -Teardown flag.
# PowerShell 5.1 compatible: ASCII only, no em-dashes.
#
# Usage:
#   .\run.ps1                    # monolith (server) on :8080  (DEFAULT)
#   .\run.ps1 split              # the full 11-service split (front door on :8082)
#   .\run.ps1 microservices      # deprecated alias for `split`
#   .\run.ps1 -Teardown          # stop whatever run.ps1 started last
#
# Assumes a local Postgres is already running (DATABASE_URL or the default DSN).
# Env passthrough: DATABASE_URL, ADMIN_USER/ADMIN_PASS (admin portal + monolith),
# ACCOUNTS_DEV_AUTH, etc. are inherited by every child process; unset ADMIN_USER =
# the admin portal is OPEN (each service logs its own loud warning).

[CmdletBinding()]
param(
    [ValidateSet('monolith', 'split', 'microservices')]
    [string]$Mode = 'monolith',
    [switch]$Teardown
)

$ErrorActionPreference = 'Stop'
Set-Location -Path $PSScriptRoot

if ($Mode -eq 'microservices') {
    Write-Host "NOTE: 'microservices' is a deprecated alias for 'split'."
    $Mode = 'split'
}

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

# Start-Svc NAME EXE ENV-HASHTABLE -- launch EXE in the background with the given env
# vars, redirecting stdout/stderr to run\<name>.{out,err}.log. Appends to the pid lists.
$script:StartedPids = @()
$script:StartedNames = @()
function Start-Svc([string]$Name, [string]$Exe, [hashtable]$EnvVars) {
    foreach ($k in $EnvVars.Keys) { Set-Item -Path "Env:$k" -Value $EnvVars[$k] }
    $out = Join-Path $RunDir "$Name.out.log"
    $err = Join-Path $RunDir "$Name.err.log"
    $p = Start-Process -FilePath $Exe -NoNewWindow -PassThru `
        -RedirectStandardOutput $out -RedirectStandardError $err
    $script:StartedPids += $p.Id
    $script:StartedNames += $Name
    return $p.Id
}

# Health-check goes to 127.0.0.1, NOT localhost: on Windows `localhost` resolves to IPv6
# ::1 first, but the services bind IPv4 0.0.0.0, so Invoke-WebRequest would hang on ::1.
function Wait-Healthy([int]$Port, [string]$Name) {
    $url = "http://127.0.0.1:$Port/healthz"
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

function Admin-Note {
    if ($env:ADMIN_USER) {
        Write-Host '    (Basic auth: ADMIN_USER/ADMIN_PASS are set)'
    } else {
        Write-Host '    (ADMIN_USER/ADMIN_PASS unset -> the admin portal is OPEN; set them to gate it)'
    }
}

# --- Build ------------------------------------------------------------------
# Both modes build edgeca + playercli: each topology fronts players over QUIC
# (PLAYER_EDGE_ADDR), so both need the shared dev CA (edgeca) and a client (playercli).
if ($Mode -eq 'monolith') {
    Write-Host 'Building server (monolith) + edgeca + playercli ...'
    cargo build -p server -p edgeca -p playercli
} else {
    Write-Host 'Building edgeca + the 11 split services + playercli ...'
    cargo build -p edgeca -p playercli `
        -p accounts-svc -p audit-svc -p scheduler-svc -p rating-svc -p leaderboard-svc `
        -p match-svc -p characters-svc -p config-svc -p inventory-svc -p gateway-svc -p admin-svc
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
    # ADMIN_USER/ADMIN_PASS + ACCOUNTS_DEV_AUTH inherit from the environment (not set here).
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
    Write-Host '======================= monolith running ======================='
    Write-Host '  Web UI (SPA demo): http://localhost:8080/'
    Write-Host '  Admin panel:       http://localhost:8080/admin'
    Admin-Note
    Write-Host '  Player QUIC front: :9100   (drive it with target\debug\playercli.exe)'
    Write-Host '  Metrics:           http://localhost:8080/metrics'
    Write-Host "  Logs:              $RunDir\monolith.{out,err}.log"
    Write-Host '  Teardown:          .\run.ps1 -Teardown'
    Write-Host '================================================================'
    return
}

# --- Split (the full 11-service microservice topology) ----------------------
# Boot ORDER + per-process env are transcribed from split-proof.ps1. Ordering notes:
#   - config-svc (C) MUST be up before inventory-svc (B): B's config stub boot-fills a
#     snapshot from C in `start` and fails loud if C is unreachable.
#   - accounts-svc (D) first: every gateway verifies bearers against it (lazy dial, so
#     not strictly required, but we mirror the proof's order).
#   - admin-svc (E) last: it dials A/B/C/D/F/H edges to fan out their admin pages.
# MESSAGING_ORIGIN is DISTINCT per process (never the "monolith" default): each relay
# drains ONLY its own origin's outbox rows. Peer *_EDGE_ADDR values are NUMERIC
# host:port (Rust's SocketAddr needs a literal IP). All peers share ONE dev CA.

# HTTP ports 8080-8090, internal mTLS edge ports 9000-9008, player QUIC :9100.
$APort = 8080; $BPort = 8081; $GPort = 8082; $CPort = 8083; $DPort = 8084; $EPort = 8085
$FPort = 8086; $HPort = 8087; $IPort = 8088; $JPort = 8089; $KPort = 8090
$AEdge = 9000; $BEdge = 9001; $CEdge = 9002; $DEdge = 9003; $FEdge = 9004
$HEdge = 9005; $IEdge = 9006; $JEdge = 9007; $KEdge = 9008; $PlayerPort = 9100

$CaCert = Join-Path $RunDir 'edge-ca.crt'
$CaKey = Join-Path $RunDir 'edge-ca.key'
Write-Host "Minting shared edge dev CA -> $CaCert ..."
& (Join-Path $BinDir 'edgeca.exe') --cert $CaCert --key $CaKey
if ($LASTEXITCODE -ne 0) { throw 'edgeca failed' }

# D: accounts-svc -- owns the accounts schema; serves accounts.verifySession on its edge
# (every other process verifies bearers against it). player.registered rides D's outbox
# to audit-svc (F).
Write-Host "Starting D (accounts-svc) on :$DPort, edge :$DEdge ..."
Start-Svc 'accounts' (Join-Path $BinDir 'accounts-svc.exe') @{
    PORT               = ":$DPort"
    DATABASE_URL       = $env:DATABASE_URL
    EDGE_ADDR          = ":$DEdge"
    EDGE_CA_CERT       = $CaCert
    EDGE_CA_KEY        = $CaKey
    MESSAGING_ORIGIN   = 'accounts-svc'
    EVENTS_SUBSCRIBERS = "player.registered=http://127.0.0.1:$FPort/events"
} | Out-Null
Wait-Healthy $DPort 'D (accounts-svc)'

# F: audit-svc -- append-only ledger. PURE SINK (empty EVENTS_SUBSCRIBERS): every
# producer's relay POSTs to F's /events. Serves admin.adminData ("Audit Log") on its edge.
Write-Host "Starting F (audit-svc) on :$FPort, edge :$FEdge ..."
Start-Svc 'audit' (Join-Path $BinDir 'audit-svc.exe') @{
    PORT               = ":$FPort"
    DATABASE_URL       = $env:DATABASE_URL
    EDGE_ADDR          = ":$FEdge"
    EDGE_CA_CERT       = $CaCert
    EDGE_CA_KEY        = $CaKey
    MESSAGING_ORIGIN   = 'audit-svc'
    EVENTS_SUBSCRIBERS = ''
} | Out-Null
Wait-Healthy $FPort 'F (audit-svc)'

# H: scheduler-svc -- DURABLE PRODUCER (1s loop fires scheduler.fired via advisory lock),
# relays scheduler.fired to audit-svc (F). Serves admin.adminData ("Schedules").
Write-Host "Starting H (scheduler-svc) on :$HPort, edge :$HEdge ..."
Start-Svc 'scheduler' (Join-Path $BinDir 'scheduler-svc.exe') @{
    PORT               = ":$HPort"
    DATABASE_URL       = $env:DATABASE_URL
    EDGE_ADDR          = ":$HEdge"
    EDGE_CA_CERT       = $CaCert
    EDGE_CA_KEY        = $CaKey
    MESSAGING_ORIGIN   = 'scheduler-svc'
    EVENTS_SUBSCRIBERS = "scheduler.fired=http://127.0.0.1:$FPort/events"
} | Out-Null
Wait-Healthy $HPort 'H (scheduler-svc)'

# J: rating-svc -- provides rating.mmr on its edge (match-svc reads it sync) and REACTS to
# match.finished (+15/-15). In-memory MMR but owns an inbox. Pure sink (no subscribers).
Write-Host "Starting J (rating-svc) on :$JPort, edge :$JEdge ..."
Start-Svc 'rating' (Join-Path $BinDir 'rating-svc.exe') @{
    PORT               = ":$JPort"
    DATABASE_URL       = $env:DATABASE_URL
    EDGE_ADDR          = ":$JEdge"
    EDGE_CA_CERT       = $CaCert
    EDGE_CA_KEY        = $CaKey
    MESSAGING_ORIGIN   = 'rating-svc'
    EVENTS_SUBSCRIBERS = ''
} | Out-Null
Wait-Healthy $JPort 'J (rating-svc)'

# K: leaderboard-svc -- owns schema leaderboard + an inbox; REACTS to match.finished
# (upsert wins+1); serves GET /leaderboard (gateway routes it Remote here). Pure sink.
Write-Host "Starting K (leaderboard-svc) on :$KPort, edge :$KEdge ..."
Start-Svc 'leaderboard' (Join-Path $BinDir 'leaderboard-svc.exe') @{
    PORT               = ":$KPort"
    DATABASE_URL       = $env:DATABASE_URL
    EDGE_ADDR          = ":$KEdge"
    EDGE_CA_CERT       = $CaCert
    EDGE_CA_KEY        = $CaKey
    ACCOUNTS_EDGE_ADDR = "127.0.0.1:$DEdge"
    MESSAGING_ORIGIN   = 'leaderboard-svc'
    EVENTS_SUBSCRIBERS = ''
} | Out-Null
Wait-Healthy $KPort 'K (leaderboard-svc)'

# I: match-svc -- records matches (schema match); DURABLE PRODUCER: `report` SYNC-reads
# both players' MMR from rating-svc (J) over the edge, INSERTs + emit_tx's match.finished
# in one tx; relays match.finished to J, K and F.
Write-Host "Starting I (match-svc) on :$IPort, edge :$IEdge ..."
Start-Svc 'match' (Join-Path $BinDir 'match-svc.exe') @{
    PORT               = ":$IPort"
    DATABASE_URL       = $env:DATABASE_URL
    EDGE_ADDR          = ":$IEdge"
    EDGE_CA_CERT       = $CaCert
    EDGE_CA_KEY        = $CaKey
    RATING_EDGE_ADDR   = "127.0.0.1:$JEdge"
    ACCOUNTS_EDGE_ADDR = "127.0.0.1:$DEdge"
    MESSAGING_ORIGIN   = 'match-svc'
    EVENTS_SUBSCRIBERS = "match.finished=http://127.0.0.1:$JPort/events,http://127.0.0.1:$KPort/events,http://127.0.0.1:$FPort/events"
} | Out-Null
Wait-Healthy $IPort 'I (match-svc)'

# A: characters-svc -- owns schema characters; emits character.created/.deleted, relayed
# to inventory-svc (B) and audit-svc (F).
Write-Host "Starting A (characters-svc) on :$APort, edge :$AEdge ..."
Start-Svc 'characters' (Join-Path $BinDir 'characters-svc.exe') @{
    PORT               = ":$APort"
    DATABASE_URL       = $env:DATABASE_URL
    EDGE_ADDR          = ":$AEdge"
    EDGE_CA_CERT       = $CaCert
    EDGE_CA_KEY        = $CaKey
    ACCOUNTS_EDGE_ADDR = "127.0.0.1:$DEdge"
    MESSAGING_ORIGIN   = 'characters-svc'
    EVENTS_SUBSCRIBERS = "character.created=http://127.0.0.1:$BPort/events,http://127.0.0.1:$FPort/events;character.deleted=http://127.0.0.1:$BPort/events,http://127.0.0.1:$FPort/events"
} | Out-Null
Wait-Healthy $APort 'A (characters-svc)'

# C: config-svc -- owns the config schema + LISTEN/NOTIFY listener; serves config.snapshot
# on its edge; relays config.changed durably to B. MUST be up before B (B boot-fills from C).
Write-Host "Starting C (config-svc) on :$CPort, edge :$CEdge ..."
Start-Svc 'config' (Join-Path $BinDir 'config-svc.exe') @{
    PORT               = ":$CPort"
    DATABASE_URL       = $env:DATABASE_URL
    EDGE_ADDR          = ":$CEdge"
    EDGE_CA_CERT       = $CaCert
    EDGE_CA_KEY        = $CaKey
    ACCOUNTS_EDGE_ADDR = "127.0.0.1:$DEdge"
    MESSAGING_ORIGIN   = 'config-svc'
    EVENTS_SUBSCRIBERS = "config.changed=http://127.0.0.1:$BPort/events,http://127.0.0.1:$FPort/events"
} | Out-Null
Wait-Healthy $CPort 'C (config-svc)'

# B: inventory-svc -- owns schema inventory; serves its OWN edge (:9001) so gateway can
# dispatch inventory.* Remote to it; dials A (owner_of), C (CachedConfig), D (verify).
Write-Host "Starting B (inventory-svc) on :$BPort, edge :$BEdge ..."
Start-Svc 'inventory' (Join-Path $BinDir 'inventory-svc.exe') @{
    PORT                 = ":$BPort"
    DATABASE_URL         = $env:DATABASE_URL
    EDGE_ADDR            = ":$BEdge"
    EDGE_CA_CERT         = $CaCert
    EDGE_CA_KEY          = $CaKey
    CHARACTERS_EDGE_ADDR = "127.0.0.1:$AEdge"
    CONFIG_EDGE_ADDR     = "127.0.0.1:$CEdge"
    ACCOUNTS_EDGE_ADDR   = "127.0.0.1:$DEdge"
    MESSAGING_ORIGIN     = 'inventory-svc'
} | Out-Null
Wait-Healthy $BPort 'B (inventory-svc)'

# G: gateway-svc -- the dedicated front door: HTTP :8082 + player QUIC :9100. No DB, no
# provider modules: only remote::Stubs, so EVERY op resolves Remote over the edge. Also
# reverse-proxies /admin -> admin-svc (E) and /accounts/epic -> accounts-svc (D).
Write-Host "Starting G (gateway-svc) on :$GPort, player QUIC :$PlayerPort ..."
Start-Svc 'gateway' (Join-Path $BinDir 'gateway-svc.exe') @{
    PORT                  = ":$GPort"
    PLAYER_EDGE_ADDR      = ":$PlayerPort"
    EDGE_CA_CERT          = $CaCert
    EDGE_CA_KEY           = $CaKey
    CHARACTERS_EDGE_ADDR  = "127.0.0.1:$AEdge"
    INVENTORY_EDGE_ADDR   = "127.0.0.1:$BEdge"
    ACCOUNTS_EDGE_ADDR    = "127.0.0.1:$DEdge"
    MATCH_EDGE_ADDR       = "127.0.0.1:$IEdge"
    LEADERBOARD_EDGE_ADDR = "127.0.0.1:$KEdge"
    ADMIN_HTTP_ADDR       = "127.0.0.1:$EPort"
    ACCOUNTS_HTTP_ADDR    = "127.0.0.1:$DPort"
} | Out-Null
Wait-Healthy $GPort 'G (gateway-svc)'

# E: admin-svc -- the admin portal (HTTP :8085, no DB, no edge server). It DIALS the
# provider edges (A/B/C/D/F/H) to fan their admin pages out over QUIC. ADMIN_USER/
# ADMIN_PASS inherit from the environment (unset -> open portal + loud warning).
Write-Host "Starting E (admin-svc) on :$EPort ..."
Start-Svc 'admin' (Join-Path $BinDir 'admin-svc.exe') @{
    PORT                 = ":$EPort"
    EDGE_CA_CERT         = $CaCert
    EDGE_CA_KEY          = $CaKey
    CHARACTERS_EDGE_ADDR = "127.0.0.1:$AEdge"
    INVENTORY_EDGE_ADDR  = "127.0.0.1:$BEdge"
    CONFIG_EDGE_ADDR     = "127.0.0.1:$CEdge"
    ACCOUNTS_EDGE_ADDR   = "127.0.0.1:$DEdge"
    AUDIT_EDGE_ADDR      = "127.0.0.1:$FEdge"
    SCHEDULER_EDGE_ADDR  = "127.0.0.1:$HEdge"
} | Out-Null
Wait-Healthy $EPort 'E (admin-svc)'

Write-Pids
Write-Host ''
Write-Host '==================== split running (11 services) ===================='
Write-Host "  Front door (gateway-svc): http://localhost:$GPort   (player QUIC :$PlayerPort)"
Write-Host "  Admin panel:              http://localhost:$GPort/admin   (through the gateway front)"
Admin-Note
Write-Host "  Metrics (front door):     http://localhost:$GPort/metrics"
Write-Host ''
Write-Host '  Peers (direct HTTP, normally reached THROUGH the front door):'
Write-Host "    A characters-svc :$APort (edge :$AEdge)   B inventory-svc :$BPort (edge :$BEdge)"
Write-Host "    C config-svc     :$CPort (edge :$CEdge)   D accounts-svc  :$DPort (edge :$DEdge)"
Write-Host "    E admin-svc      :$EPort               F audit-svc     :$FPort (edge :$FEdge)"
Write-Host "    H scheduler-svc  :$HPort (edge :$HEdge)   I match-svc     :$IPort (edge :$IEdge)"
Write-Host "    J rating-svc     :$JPort (edge :$JEdge)   K leaderboard-svc :$KPort (edge :$KEdge)"
Write-Host ''
Write-Host "  Drive the player QUIC front: target\debug\playercli.exe --addr 127.0.0.1:$PlayerPort --ca $CaCert ..."
Write-Host "  Logs:     $RunDir\<service>.{out,err}.log"
Write-Host '  Teardown: .\run.ps1 -Teardown'
Write-Host '====================================================================='
