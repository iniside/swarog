# split-proof.ps1 -- the SPLIT-topology proof for the rust-sketch (Steps 12 + 8).
#
# The whole point of the milestone: exercises the ELEVEN-PROCESS split (characters-svc =
# A on :8080 / edge :9000, inventory-svc = B on :8081 / edge :9001, gateway-svc = G on
# :8082 / player QUIC :9100, config-svc = C on :8083 / edge :9002, accounts-svc = D on
# :8084 / edge :9003, admin-svc = E on :8085, audit-svc = F on :8086 / edge :9004,
# scheduler-svc = H on :8087 / edge :9005, match-svc = I on :8088 / edge :9006,
# rating-svc = J on :8089 / edge :9007, leaderboard-svc = K on :8090 / edge :9008), NOT
# the monolith, driving the real player flows over HTTP
# (through the gateway front-door with a REAL bearer minted by register+login through
# the front -- Step 6 replaced the dev-<uuid> tokens), the sync authz over
# QUIC/mTLS, AND the NEW dedicated QUIC player front (Step 8): external players connect
# to gateway-svc over QUIC (server-cert-only TLS), the front auth-verifies the
# bearer-in-envelope once and dispatches the method through the route table (allow-list
# gate) to the owning peer over the internal mTLS edge. It:
#   1. mints the shared dev CA via edgeca,
#   2. starts A, B, then G in the background, gating each on /healthz,
#   3. runs the assertions below, tearing ALL down on exit (even on failure),
#   4. as a final stage boots the monolith (cmd/server) with the SAME player QUIC front
#      and proves parity (never-monolith-only-features), and
#   5. exits non-zero if ANY assertion fails.
#
# THE PROOF (all against the SPLIT, over real HTTP/QUIC):
#   - REAL AUTH (Step 6): register + login through G's front mint a DB-backed session
#     on D; the bearer then authorizes ops on every process (each gateway verifies it
#     against D's accounts.verifySession over QUIC/mTLS). NEGATIVE: a garbage token
#     and a dev-<uuid> token are both 401 through G (no ACCOUNTS_DEV_AUTH anywhere).
#   - Async event A->B: POST /characters on A -> 201; A emits character.created; its
#     relay POSTs to B /events; inventory's durable on_tx grants the starter item.
#     Poll GET /inventory/character/<id> on B until starter_sword x1 appears.
#   - Sync call over QUIC B->A: that same GET forces list_character to call owner_of
#     via the remote Stub over QUIC/mTLS to A -- a 200 with the holding proves the
#     sync path AND mTLS. NEGATIVE authz: the same GET as a DIFFERENT player -> 403.
#   - Integrity via event (not FK) A->B: DELETE /characters/<id> on A -> 204; A emits
#     character.deleted; inventory's on_tx wipes the holdings. Assert the DB holdings
#     row is genuinely gone (the HTTP 404 after delete alone only proves the character
#     is gone via owner_of and would mask an un-wiped row).
#   - CONFIG live-reload C->B (Step 5): change inventory/starter_item at runtime via
#     psql; config-svc publishes config.changed DURABLY; its relay POSTs to B, whose
#     CachedConfig + inventory starter spec both reload. A NEWLY created character then
#     gets the NEW starter -- cross-process live reload with no restart.
#
# THE QUIC PLAYER FRONT (Step 8, all through gateway-svc's :9100 QUIC front via the
# playercli tool -- exit 0 iff transport ok AND payload status=="Ok"):
#   - P1 characters.create over QUIC -> exit 0 (player QUIC -> G -> mTLS edge -> A).
#   - P2 inventory.listCharacter over QUIC -> exit 0 (player QUIC -> G -> Remote ->
#     B's NEW :9001 edge -> owner_of over QUIC -> A): the newest composition.
#   - P3 GET /inventory/character/<id> through G's HTTP :8082 -> 200.
#   - P4 no token / bad token on an auth op -> exit 1 + {status:"Unauthorized"}.
#   - P5 characters.ownerOf (wire-only, absent from the route table) -> exit 1 +
#     {status:"NotFound"} (the method allow-list gate, live).
#
# Uses curl.exe (ships with Windows 11) for HTTP parity with split-proof.sh.
# ASCII only -- PowerShell 5.1 chokes on em-dashes.

[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-Location -Path $PSScriptRoot

$RunDir   = Join-Path $PSScriptRoot 'run'
$BinDir   = Join-Path $PSScriptRoot 'target\debug'
$CaCert   = Join-Path $RunDir 'edge-ca.crt'
$CaKey    = Join-Path $RunDir 'edge-ca.key'
$APort     = 8080
$BPort     = 8081
$GPort     = 8082
$CPort     = 8083
$DPort     = 8084
$EPort     = 8085
$FPort     = 8086
$HPort     = 8087
$IPort     = 8088
$JPort     = 8089
$KPort     = 8090
$EdgePort  = 9000
$BEdgePort = 9001
$CEdgePort = 9002
$DEdgePort = 9003
$FEdgePort = 9004
$HEdgePort = 9005
$IEdgePort = 9006
$JEdgePort = 9007
$KEdgePort = 9008
$PlayerPort = 9100
$PlayerCli = Join-Path $BinDir 'playercli.exe'

$DefaultDsn = 'postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable'
if (-not $env:DATABASE_URL -or $env:DATABASE_URL.Trim() -eq '') { $env:DATABASE_URL = $DefaultDsn }

# Basic-auth creds for the admin portal (admin-svc runs WITH them).
$AdminUser = 'proofadmin'
$AdminPass = 'proofpass'

$script:Fails = 0
$script:AProc = $null
$script:BProc = $null
$script:GProc = $null
$script:CProc = $null
$script:DProc = $null
$script:EProc = $null
$script:FProc = $null
$script:HProc = $null
$script:IProc = $null
$script:JProc = $null
$script:KProc = $null
$script:MProc = $null

function Note($m) { Write-Host "[proof] $m" }
function Pass($m) { Write-Host "  PASS  $m" }
function Fail($m) { Write-Host "  FAIL  $m"; $script:Fails++ }

# curl.exe returns "body\n<httpcode>" via -w; split the last line off as the status.
function Invoke-Curl([string[]]$CurlArgs) {
    $raw = & curl.exe -s -w "`n%{http_code}" @CurlArgs 2>$null
    $text = ($raw -join "`n")
    $lines = $text -split "`n"
    $code = $lines[-1].Trim()
    $body = ($lines[0..($lines.Count - 2)] -join "`n")
    return [pscustomobject]@{ Code = $code; Body = $body }
}

function Find-Psql {
    $cmd = Get-Command psql.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $hits = Get-ChildItem 'C:\Program Files\PostgreSQL\*\bin\psql.exe' -ErrorAction SilentlyContinue
    if ($hits) { return $hits[0].FullName }
    return $null
}
$Psql = Find-Psql

# Run one SQL statement against the test DB (best-effort; no psql -> $null).
function Invoke-Sql([string]$Sql) {
    if (-not $Psql) { return $null }
    $env:PGPASSWORD = 'gamebackend'
    return (& $Psql -U gamebackend -h localhost -d gamebackend -t -A -c $Sql 2>$null)
}

# Health-check and player HTTP go to 127.0.0.1, NOT localhost: on Windows `localhost`
# resolves to IPv6 ::1 first, but the services bind IPv4 0.0.0.0, so Invoke-WebRequest
# would hang on ::1. (The relay's localhost:8081 subscriber URL is fine -- the Rust
# HTTP client falls back to IPv4.)
function Wait-Healthy([int]$Port, [string]$Name) {
    for ($i = 0; $i -lt 60; $i++) {
        try {
            Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:$Port/healthz" -TimeoutSec 2 | Out-Null
            Note "$Name healthy on :$Port"; return $true
        } catch { Start-Sleep -Milliseconds 500 }
    }
    Note "$Name NEVER became healthy on :$Port"; return $false
}

function Start-Svc([string]$Exe, [hashtable]$EnvVars, [string]$LogName) {
    foreach ($k in $EnvVars.Keys) { Set-Item -Path "Env:$k" -Value $EnvVars[$k] }
    $out = Join-Path $RunDir "$LogName.out.log"
    $err = Join-Path $RunDir "$LogName.err.log"
    return Start-Process -FilePath $Exe -NoNewWindow -PassThru -RedirectStandardOutput $out -RedirectStandardError $err
}

function Teardown {
    if ($script:AProc -and -not $script:AProc.HasExited) { Stop-Process -Id $script:AProc.Id -Force -ErrorAction SilentlyContinue; Note "stopped A (pid $($script:AProc.Id))" }
    if ($script:BProc -and -not $script:BProc.HasExited) { Stop-Process -Id $script:BProc.Id -Force -ErrorAction SilentlyContinue; Note "stopped B (pid $($script:BProc.Id))" }
    if ($script:GProc -and -not $script:GProc.HasExited) { Stop-Process -Id $script:GProc.Id -Force -ErrorAction SilentlyContinue; Note "stopped G (pid $($script:GProc.Id))" }
    if ($script:CProc -and -not $script:CProc.HasExited) { Stop-Process -Id $script:CProc.Id -Force -ErrorAction SilentlyContinue; Note "stopped C (pid $($script:CProc.Id))" }
    if ($script:DProc -and -not $script:DProc.HasExited) { Stop-Process -Id $script:DProc.Id -Force -ErrorAction SilentlyContinue; Note "stopped D (pid $($script:DProc.Id))" }
    if ($script:EProc -and -not $script:EProc.HasExited) { Stop-Process -Id $script:EProc.Id -Force -ErrorAction SilentlyContinue; Note "stopped E (pid $($script:EProc.Id))" }
    if ($script:FProc -and -not $script:FProc.HasExited) { Stop-Process -Id $script:FProc.Id -Force -ErrorAction SilentlyContinue; Note "stopped F (pid $($script:FProc.Id))" }
    if ($script:HProc -and -not $script:HProc.HasExited) { Stop-Process -Id $script:HProc.Id -Force -ErrorAction SilentlyContinue; Note "stopped H (pid $($script:HProc.Id))" }
    if ($script:IProc -and -not $script:IProc.HasExited) { Stop-Process -Id $script:IProc.Id -Force -ErrorAction SilentlyContinue; Note "stopped I (pid $($script:IProc.Id))" }
    if ($script:JProc -and -not $script:JProc.HasExited) { Stop-Process -Id $script:JProc.Id -Force -ErrorAction SilentlyContinue; Note "stopped J (pid $($script:JProc.Id))" }
    if ($script:KProc -and -not $script:KProc.HasExited) { Stop-Process -Id $script:KProc.Id -Force -ErrorAction SilentlyContinue; Note "stopped K (pid $($script:KProc.Id))" }
    if ($script:MProc -and -not $script:MProc.HasExited) { Stop-Process -Id $script:MProc.Id -Force -ErrorAction SilentlyContinue; Note "stopped monolith (pid $($script:MProc.Id))" }
    $script:AProc = $null; $script:BProc = $null; $script:GProc = $null; $script:CProc = $null; $script:DProc = $null; $script:EProc = $null; $script:FProc = $null; $script:HProc = $null; $script:IProc = $null; $script:JProc = $null; $script:KProc = $null; $script:MProc = $null
}

# Runs playercli, capturing stdout (joined) and the process exit code. Returns a
# pscustomobject { Rc; Out }. playercli exits 0 iff transport ok AND status=="Ok".
function Invoke-PlayerCli([string[]]$CliArgs) {
    $out = & $PlayerCli @CliArgs 2>$null
    $rc = $LASTEXITCODE
    return [pscustomobject]@{ Rc = $rc; Out = (($out | Out-String)).Trim() }
}

try {
    Note 'building edgeca + characters-svc + inventory-svc + gateway-svc + config-svc + accounts-svc + admin-svc + audit-svc + scheduler-svc + match-svc + rating-svc + leaderboard-svc + playercli + csharp-client-gen + server ...'
    cargo build -p edgeca -p characters-svc -p inventory-svc -p gateway-svc -p config-svc -p accounts-svc -p admin-svc -p audit-svc -p scheduler-svc -p match-svc -p rating-svc -p leaderboard-svc -p playercli -p csharp-client-gen -p server
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

    New-Item -ItemType Directory -Force -Path $RunDir | Out-Null

    # Clear stragglers from an aborted prior run so ports are free (idempotent reruns).
    foreach ($n in 'characters-svc', 'inventory-svc', 'gateway-svc', 'config-svc', 'accounts-svc', 'admin-svc', 'audit-svc', 'scheduler-svc', 'match-svc', 'rating-svc', 'leaderboard-svc', 'server') {
        Get-Process -Name $n -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Milliseconds 500

    Note "minting shared edge dev CA -> $CaCert"
    & (Join-Path $BinDir 'edgeca.exe') --cert $CaCert --key $CaKey
    if ($LASTEXITCODE -ne 0) { throw 'edgeca failed' }

    # D (accounts-svc) FIRST: owns the accounts schema and serves accounts.verifySession
    # + the auth op faces on its mTLS edge; every other gateway verifies bearers here.
    Note "starting D (accounts-svc) on :$DPort, edge :$DEdgePort ..."
    $script:DProc = Start-Svc (Join-Path $BinDir 'accounts-svc.exe') @{
        PORT               = ":$DPort"
        DATABASE_URL       = $env:DATABASE_URL
        EDGE_ADDR          = ":$DEdgePort"
        EDGE_CA_CERT       = $CaCert
        EDGE_CA_KEY        = $CaKey
        EVENTS_ORIGIN      = 'accounts-svc'
        EVENTS_SUBSCRIBERS = "player.registered=http://127.0.0.1:$FPort/events"
    } 'accounts'
    if (-not (Wait-Healthy $DPort 'D (accounts-svc)')) { throw 'D failed to start' }

    # F (audit-svc): audit, edge :9004. A PURE SINK (produces nothing, so no
    # EVENTS_SUBSCRIBERS): every producer peer's relay POSTs to F's /events and audit's
    # on_tx_raw records each on the handed inbox-dedup tx. Serves admin.adminData on its
    # mTLS edge so admin-svc fans the "Audit Log" page out over QUIC. Distinct
    # EVENTS_ORIGIN for its own outbox identity.
    Note "starting F (audit-svc) on :$FPort, edge :$FEdgePort ..."
    $script:FProc = Start-Svc (Join-Path $BinDir 'audit-svc.exe') @{
        PORT               = ":$FPort"
        DATABASE_URL       = $env:DATABASE_URL
        EDGE_ADDR          = ":$FEdgePort"
        EDGE_CA_CERT       = $CaCert
        EDGE_CA_KEY        = $CaKey
        EVENTS_ORIGIN      = 'audit-svc'
        EVENTS_SUBSCRIBERS = ''
    } 'audit'
    if (-not (Wait-Healthy $FPort 'F (audit-svc)')) { throw 'F failed to start' }

    # H (scheduler-svc): scheduler, edge :9005. A DURABLE PRODUCER: its 1s
    # loop fires scheduler.fired for every due schedule (race-safe via a per-schedule
    # pg_try_advisory_lock) and its relay POSTs scheduler.fired to audit-svc (F). Serves
    # admin.adminData ("Schedules") on its mTLS edge so admin-svc fans it out. Distinct
    # EVENTS_ORIGIN (names a remote sink, so a default origin would be rejected).
    Note "starting H (scheduler-svc) on :$HPort, edge :$HEdgePort ..."
    $script:HProc = Start-Svc (Join-Path $BinDir 'scheduler-svc.exe') @{
        PORT               = ":$HPort"
        DATABASE_URL       = $env:DATABASE_URL
        EDGE_ADDR          = ":$HEdgePort"
        EDGE_CA_CERT       = $CaCert
        EDGE_CA_KEY        = $CaKey
        EVENTS_ORIGIN      = 'scheduler-svc'
        EVENTS_SUBSCRIBERS = "scheduler.fired=http://127.0.0.1:$FPort/events"
    } 'scheduler'
    if (-not (Wait-Healthy $HPort 'H (scheduler-svc)')) { throw 'H failed to start' }

    # J (rating-svc): rating, edge :9007. Provides rating.mmr on its mTLS
    # edge (match-svc reads it sync before recording) and REACTS to match.finished
    # (+15/-15) durably. In-memory MMR (no schema) but OWNS an inbox, so it needs a DB
    # pool (the durable-events plane is app-owned, not a module dependency). Pure
    # sink for match.finished (no EVENTS_SUBSCRIBERS).
    Note "starting J (rating-svc) on :$JPort, edge :$JEdgePort ..."
    $script:JProc = Start-Svc (Join-Path $BinDir 'rating-svc.exe') @{
        PORT             = ":$JPort"
        DATABASE_URL     = $env:DATABASE_URL
        EDGE_ADDR        = ":$JEdgePort"
        EDGE_CA_CERT     = $CaCert
        EDGE_CA_KEY      = $CaKey
        EVENTS_ORIGIN    = 'rating-svc'
    } 'rating'
    if (-not (Wait-Healthy $JPort 'J (rating-svc)')) { throw 'J failed to start' }

    # K (leaderboard-svc): gateway + leaderboard, edge :9008. Owns schema
    # leaderboard + an inbox, REACTS to match.finished (upsert wins+1) durably, and serves
    # GET /leaderboard (gateway-svc routes it Remote here). Pure sink for match.finished.
    Note "starting K (leaderboard-svc) on :$KPort, edge :$KEdgePort ..."
    $script:KProc = Start-Svc (Join-Path $BinDir 'leaderboard-svc.exe') @{
        PORT               = ":$KPort"
        DATABASE_URL       = $env:DATABASE_URL
        EDGE_ADDR          = ":$KEdgePort"
        EDGE_CA_CERT       = $CaCert
        EDGE_CA_KEY        = $CaKey
        EVENTS_ORIGIN      = 'leaderboard-svc'
    } 'leaderboard'
    if (-not (Wait-Healthy $KPort 'K (leaderboard-svc)')) { throw 'K failed to start' }

    # I (match-svc): gateway + match + rating stub, edge :9006. Records
    # matches (schema match) and is a DURABLE PRODUCER: report SYNC-reads both players'
    # MMR from rating-svc (J) over the mTLS edge, INSERTs the match row + emit_tx's
    # match.finished IN ONE TX, and its relay POSTs match.finished to J, K and audit-svc
    # (F). Distinct EVENTS_ORIGIN (names remote sinks).
    Note "starting I (match-svc) on :$IPort, edge :$IEdgePort ..."
    $script:IProc = Start-Svc (Join-Path $BinDir 'match-svc.exe') @{
        PORT               = ":$IPort"
        DATABASE_URL       = $env:DATABASE_URL
        EDGE_ADDR          = ":$IEdgePort"
        EDGE_CA_CERT       = $CaCert
        EDGE_CA_KEY        = $CaKey
        RATING_EDGE_ADDR   = "127.0.0.1:$JEdgePort"
        EVENTS_ORIGIN      = 'match-svc'
        EVENTS_SUBSCRIBERS = "match.finished=http://127.0.0.1:$JPort/events,http://127.0.0.1:$KPort/events,http://127.0.0.1:$FPort/events"
    } 'match'
    if (-not (Wait-Healthy $IPort 'I (match-svc)')) { throw 'I failed to start' }

    Note "starting A (characters-svc) on :$APort, edge :$EdgePort ..."
    $script:AProc = Start-Svc (Join-Path $BinDir 'characters-svc.exe') @{
        PORT                = ":$APort"
        DATABASE_URL        = $env:DATABASE_URL
        EDGE_ADDR           = ":$EdgePort"
        EDGE_CA_CERT        = $CaCert
        EDGE_CA_KEY         = $CaKey
        EVENTS_ORIGIN       = 'characters-svc'
        EVENTS_SUBSCRIBERS  = "character.created=http://127.0.0.1:$BPort/events,http://127.0.0.1:$FPort/events;character.deleted=http://127.0.0.1:$BPort/events,http://127.0.0.1:$FPort/events"
    } 'characters'
    if (-not (Wait-Healthy $APort 'A (characters-svc)')) { throw 'A failed to start' }

    # Reset the config baseline: B must boot with the DEFAULT starter (starter_sword),
    # so the later runtime change to health_potion is provably a LIVE reload. C/B are not
    # up yet, so their boot loads see no row.
    if ($Psql) {
        Invoke-Sql "DELETE FROM config.settings WHERE namespace='inventory' AND key='starter_item';" | Out-Null
        Note 'reset config baseline (deleted inventory/starter_item)'
    } else {
        Note 'psql not found -- the config live-reload assertion will SKIP'
    }

    # C (config-svc): owns the config schema + LISTEN/NOTIFY listener, serves
    # config.snapshot on its mTLS edge, and relays config.changed durably to B. Distinct
    # EVENTS_ORIGIN or the app-owned durable-events plane's origin-collision guard
    # would reject it.
    Note "starting C (config-svc) on :$CPort, edge :$CEdgePort ..."
    $script:CProc = Start-Svc (Join-Path $BinDir 'config-svc.exe') @{
        PORT               = ":$CPort"
        DATABASE_URL       = $env:DATABASE_URL
        EDGE_ADDR          = ":$CEdgePort"
        EDGE_CA_CERT       = $CaCert
        EDGE_CA_KEY        = $CaKey
        EVENTS_ORIGIN      = 'config-svc'
        EVENTS_SUBSCRIBERS = "config.changed=http://127.0.0.1:$BPort/events,http://127.0.0.1:$FPort/events"
    } 'config'
    if (-not (Wait-Healthy $CPort 'C (config-svc)')) { throw 'C failed to start' }

    # B now ALSO serves its OWN mTLS edge (EDGE_ADDR=:9001) so gateway-svc can dispatch
    # inventory.* Remote to it; it dials A over CHARACTERS_EDGE_ADDR for owner_of and
    # config-svc over CONFIG_EDGE_ADDR for the CachedConfig boot-fill + snapshot refresh.
    Note "starting B (inventory-svc) on :$BPort, edge :$BEdgePort ..."
    $script:BProc = Start-Svc (Join-Path $BinDir 'inventory-svc.exe') @{
        PORT                 = ":$BPort"
        DATABASE_URL         = $env:DATABASE_URL
        EDGE_ADDR            = ":$BEdgePort"
        EDGE_CA_CERT         = $CaCert
        EDGE_CA_KEY          = $CaKey
        CHARACTERS_EDGE_ADDR = "127.0.0.1:$EdgePort"
        CONFIG_EDGE_ADDR     = "127.0.0.1:$CEdgePort"
        EVENTS_ORIGIN        = 'inventory-svc'
    } 'inventory'
    if (-not (Wait-Healthy $BPort 'B (inventory-svc)')) { throw 'B failed to start' }

    # G (gateway-svc): the dedicated front door -- HTTP :8082 + player QUIC :9100. No DB
    # (without_db), no provider modules: only remote::Stubs, so EVERY op it fronts
    # resolves Remote and is dialed over the mTLS edge to A (:9000) / B (:9001). It needs
    # the shared CA to dial peers AND to derive the player-front server cert.
    Note "starting G (gateway-svc) on :$GPort, player QUIC :$PlayerPort ..."
    $script:GProc = Start-Svc (Join-Path $BinDir 'gateway-svc.exe') @{
        PORT                 = ":$GPort"
        PLAYER_EDGE_ADDR     = ":$PlayerPort"
        EDGE_CA_CERT         = $CaCert
        EDGE_CA_KEY          = $CaKey
        CHARACTERS_EDGE_ADDR = "127.0.0.1:$EdgePort"
        INVENTORY_EDGE_ADDR  = "127.0.0.1:$BEdgePort"
        ACCOUNTS_EDGE_ADDR   = "127.0.0.1:$DEdgePort"
        MATCH_EDGE_ADDR      = "127.0.0.1:$IEdgePort"
        LEADERBOARD_EDGE_ADDR = "127.0.0.1:$KEdgePort"
        ADMIN_HTTP_ADDR      = "127.0.0.1:$EPort"
        ACCOUNTS_HTTP_ADDR   = "127.0.0.1:$DPort"
    } 'gateway'
    if (-not (Wait-Healthy $GPort 'G (gateway-svc)')) { throw 'G failed to start' }

    # E (admin-svc): the admin portal fortress -- HTTP :8085, no DB, no edge server. It
    # DIALS all six peer edges (A/B/C/D + audit + scheduler) to fan out their admin pages over QUIC; ADMIN_USER/
    # ADMIN_PASS gate the portal so the negative no-auth assertion returns 401.
    Note "starting E (admin-svc) on :$EPort ..."
    $script:EProc = Start-Svc (Join-Path $BinDir 'admin-svc.exe') @{
        PORT                 = ":$EPort"
        EDGE_CA_CERT         = $CaCert
        EDGE_CA_KEY          = $CaKey
        CHARACTERS_EDGE_ADDR = "127.0.0.1:$EdgePort"
        INVENTORY_EDGE_ADDR  = "127.0.0.1:$BEdgePort"
        CONFIG_EDGE_ADDR     = "127.0.0.1:$CEdgePort"
        ACCOUNTS_EDGE_ADDR   = "127.0.0.1:$DEdgePort"
        AUDIT_EDGE_ADDR      = "127.0.0.1:$FEdgePort"
        SCHEDULER_EDGE_ADDR  = "127.0.0.1:$HEdgePort"
        ADMIN_USER           = $AdminUser
        ADMIN_PASS           = $AdminPass
    } 'admin'
    if (-not (Wait-Healthy $EPort 'E (admin-svc)')) { throw 'E failed to start' }

    $RunSuffix = [guid]::NewGuid().ToString().Substring(0, 8)

    Write-Host ''
    Write-Host '================ REAL AUTH (Step 6) ================'
    # Register + login THROUGH the gateway front (G routes /accounts/* Remote to D over
    # the mTLS edge), then use the REAL bearer everywhere below. No dev- tokens.

    Write-Host "[A1] POST http://127.0.0.1:$GPort/accounts/register (through G -> D)"
    $reg = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/accounts/register",
        '-H', 'Content-Type: application/json',
        '-d', "{`"email`":`"proof-$RunSuffix@test.local`",`"password`":`"pw-$RunSuffix`",`"displayName`":`"Proof`"}")
    Write-Host "    -> HTTP $($reg.Code)  $($reg.Body)"
    $PlayerId = $null
    if ($reg.Body -match '"player_id":"([^"]+)"') { $PlayerId = $Matches[1] }
    if ($reg.Code -eq '201' -and $PlayerId) { Pass "register through the front -> 201, player_id=$PlayerId" } else { Fail "register expected 201 with player_id, got $($reg.Code)"; throw 'auth bootstrap failed' }

    Write-Host "[A2] POST http://127.0.0.1:$GPort/accounts/login (fresh session through G -> D)"
    $login = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/accounts/login",
        '-H', 'Content-Type: application/json',
        '-d', "{`"email`":`"proof-$RunSuffix@test.local`",`"password`":`"pw-$RunSuffix`"}")
    $Token = $null
    if ($login.Body -match '"token":"([^"]+)"') { $Token = $Matches[1] }
    Write-Host "    -> HTTP $($login.Code)  token=$(if ($Token) { $Token.Substring(0,12) })..."
    if ($login.Code -eq '200' -and $Token) { Pass 'login through the front -> 200 with a real bearer' } else { Fail "login expected 200 with token, got $($login.Code)"; throw 'auth bootstrap failed' }

    Write-Host "[A3] GET http://127.0.0.1:$GPort/accounts/me (Bearer <real token>)"
    $me = Invoke-Curl @("http://127.0.0.1:$GPort/accounts/me", '-H', "Authorization: Bearer $Token")
    Write-Host "    -> HTTP $($me.Code)  $($me.Body)"
    if ($me.Code -eq '200' -and $me.Body -match [regex]::Escape($PlayerId)) { Pass 'me -> 200 with the registered player (auth-once verified the real session)' } else { Fail "me expected 200 with player_id, got $($me.Code)" }

    # A second player for the negative authz assertion.
    $oreg = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/accounts/register",
        '-H', 'Content-Type: application/json',
        '-d', "{`"email`":`"other-$RunSuffix@test.local`",`"password`":`"pw2-$RunSuffix`",`"displayName`":`"Other`"}")
    $OtherToken = $null
    if ($oreg.Body -match '"token":"([^"]+)"') { $OtherToken = $Matches[1] }
    if (-not $OtherToken) { Fail 'second register produced no token'; throw 'auth bootstrap failed' }

    Write-Host '[A4] GET /characters through G with a GARBAGE token -> 401'
    $g1 = Invoke-Curl @("http://127.0.0.1:$GPort/characters", '-H', 'Authorization: Bearer totally-bogus-token')
    Write-Host "    -> HTTP $($g1.Code)"
    if ($g1.Code -eq '401') { Pass 'garbage token -> 401' } else { Fail "garbage token expected 401, got $($g1.Code)" }

    Write-Host '[A5] GET /characters through G with a dev-<uuid> token -> 401 (no ACCOUNTS_DEV_AUTH on G)'
    $g2 = Invoke-Curl @("http://127.0.0.1:$GPort/characters", '-H', "Authorization: Bearer dev-$([guid]::NewGuid())")
    Write-Host "    -> HTTP $($g2.Code)"
    if ($g2.Code -eq '401') { Pass 'dev- token -> 401 (gateway-svc verifies REAL sessions only)' } else { Fail "dev- token expected 401, got $($g2.Code)" }

    Write-Host ''
    Write-Host '================ SPLIT PROOF ================'

    # --- 1. CREATE through G (front-door HTTP op -> Remote -> characters-svc) ---
    # characters-svc no longer hosts a FrontDoor, so create is fronted by gateway-svc
    # (:8082) which dispatches characters.create Remote over the mTLS edge to A.
    Write-Host "[1] POST http://127.0.0.1:$GPort/characters (through G -> A, Bearer `$Token)"
    $c = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/characters",
        '-H', "Authorization: Bearer $Token", '-H', 'Content-Type: application/json',
        '-d', '{"name":"Aria","class":"mage"}')
    Write-Host "    -> HTTP $($c.Code)  $($c.Body)"
    $cid = $null
    if ($c.Body -match '"id":"([^"]+)"') { $cid = $Matches[1] }
    if ($c.Code -eq '201' -and $cid) { Pass "create -> 201, id=$cid" } else { Fail 'create expected 201 with id' }

    # --- 2. ASYNC event A->B + SYNC authz B->A over QUIC ---
    Write-Host "[2] poll GET http://127.0.0.1:$GPort/inventory/character/$cid until starter appears (through G -> B)"
    $starterOk = $false
    for ($i = 1; $i -le 30; $i++) {
        $r = Invoke-Curl @("http://127.0.0.1:$GPort/inventory/character/$cid", '-H', "Authorization: Bearer $Token")
        if ($r.Code -eq '200' -and $r.Body -match 'starter_sword') {
            Write-Host "    attempt $i -> HTTP 200 $($r.Body)"
            Pass 'starter_sword materialized in B (async event A->B) AND 200 proves owner_of over QUIC/mTLS B->A'
            $starterOk = $true; break
        }
        Start-Sleep -Milliseconds 500
    }
    if (-not $starterOk) { Fail 'starter never appeared in B (async cross-process grant / QUIC authz)' }

    # --- 3. NEGATIVE authz ---
    Write-Host "[3] GET /inventory/character/$cid through G as a DIFFERENT player (Bearer `$OtherToken)"
    $n = Invoke-Curl @("http://127.0.0.1:$GPort/inventory/character/$cid", '-H', "Authorization: Bearer $OtherToken")
    Write-Host "    -> HTTP $($n.Code)  $($n.Body)"
    if ($n.Code -eq '403' -or $n.Code -eq '404') { Pass "other player -> $($n.Code) (owner_of over QUIC gates)" } else { Fail "negative authz expected 403/404, got $($n.Code)" }

    # --- 4. DELETE through G -> A ---
    Write-Host "[4] DELETE http://127.0.0.1:$GPort/characters/$cid (through G -> A, Bearer `$Token)"
    $d = Invoke-Curl @('-X', 'DELETE', "http://127.0.0.1:$GPort/characters/$cid", '-H', "Authorization: Bearer $Token")
    Write-Host "    -> HTTP $($d.Code)"
    if ($d.Code -eq '204') { Pass 'delete -> 204' } else { Fail "delete expected 204, got $($d.Code)" }

    # --- 5. INTEGRITY via event, not FK: holdings wiped in B (DB is the real proof) ---
    Write-Host "[5] poll B until the character's holdings are WIPED (character.deleted A->B)"
    if ($Psql) {
        $env:PGPASSWORD = 'gamebackend'
        $wiped = $false
        for ($i = 1; $i -le 30; $i++) {
            $q = "SELECT count(*) FROM inventory.holdings WHERE owner_type='character' AND owner_id='$cid';"
            $out = (& $Psql -U gamebackend -h localhost -d gamebackend -t -A -c $q 2>$null)
            $count = ("$out").Trim()
            Write-Host "    attempt $i -> inventory.holdings rows for $cid = $count"
            if ($count -eq '0') { Pass 'holdings row wiped in B (integrity via character.deleted event, no FK cascade)'; $wiped = $true; break }
            Start-Sleep -Milliseconds 500
        }
        if (-not $wiped) { Fail 'holdings never wiped in B (wipe on_tx handler did not run)' }
    } else {
        Note 'psql not found -- falling back to HTTP 404 as a WEAKER wipe signal'
        $w = Invoke-Curl @("http://127.0.0.1:$GPort/inventory/character/$cid", '-H', "Authorization: Bearer $Token")
        Write-Host "    -> HTTP $($w.Code)"
        if ($w.Code -eq '404') { Pass 'post-delete GET -> 404 (character gone; DB wipe unverified, psql missing)' } else { Fail "post-delete expected 404, got $($w.Code)" }
    }

    Write-Host "[5b] post-delete GET /inventory/character/$cid through G (Bearer `$Token)"
    $w2 = Invoke-Curl @("http://127.0.0.1:$GPort/inventory/character/$cid", '-H', "Authorization: Bearer $Token")
    Write-Host "    -> HTTP $($w2.Code)  $($w2.Body)"

    Write-Host ''
    Write-Host "========= CONFIG LIVE-RELOAD (config-svc :$CPort -> inventory-svc) ========="
    # Prove the Step-5 snapshot-backed remote config reader live-reloads across processes:
    # change inventory/starter_item at RUNTIME on C's DB, and a NEWLY created character
    # must be granted the NEW starter in B -- config.changed flowed C's outbox -> B's
    # /events -> B's CachedConfig (snapshot refresh) + inventory starter spec, no restart.
    if (-not $Psql) {
        Note 'psql not found -- SKIPPING the config live-reload assertion'
    } else {
        # [C1] baseline: B booted with the default starter (no config row) -> starter_sword.
        Write-Host '[C1] baseline: create a character through G -> starter should be the DEFAULT starter_sword'
        $bc = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/characters",
            '-H', "Authorization: Bearer $Token", '-H', 'Content-Type: application/json',
            '-d', '{"name":"Baseline","class":"mage"}')
        $bcid = $null
        if ($bc.Body -match '"id":"([^"]+)"') { $bcid = $Matches[1] }
        $baseOk = $false
        for ($i = 1; $i -le 30; $i++) {
            $r = Invoke-Curl @("http://127.0.0.1:$GPort/inventory/character/$bcid", '-H', "Authorization: Bearer $Token")
            if ($r.Body -match 'starter_sword') { $baseOk = $true; break }
            if ($r.Body -match 'health_potion') { break }
            Start-Sleep -Milliseconds 500
        }
        if ($baseOk) { Pass 'baseline character granted starter_sword (B booted on the default via CachedConfig)' } else { Fail "baseline starter_sword not granted (bcid=$bcid)" }

        # [C2] runtime change on C's DB: trigger NOTIFYs -> C's listener emit_tx (durable)
        # -> C's relay POSTs config.changed -> B refreshes CachedConfig + reloads spec.
        Write-Host '[C2] set config inventory/starter_item=health_potion (via psql on C shared DB)'
        Invoke-Sql "INSERT INTO config.settings (namespace,key,value) VALUES ('inventory','starter_item','health_potion') ON CONFLICT (namespace,key) DO UPDATE SET value=excluded.value;" | Out-Null

        # [C3] a NEWLY created character must now be granted the NEW starter. The spec is
        # materialized at grant time, so retry with fresh characters until it takes.
        Write-Host '[C3] create fresh characters until one is granted health_potion (live reload C->B)'
        $reloadOk = $false
        for ($i = 1; $i -le 30; $i++) {
            $nc = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/characters",
                '-H', "Authorization: Bearer $Token", '-H', 'Content-Type: application/json',
                '-d', '{"name":"Reloaded","class":"mage"}')
            $ncid = $null
            if ($nc.Body -match '"id":"([^"]+)"') { $ncid = $Matches[1] }
            $r = $null
            for ($j = 1; $j -le 10; $j++) {
                $r = Invoke-Curl @("http://127.0.0.1:$GPort/inventory/character/$ncid", '-H', "Authorization: Bearer $Token")
                if ($r.Body -match 'starter_sword|health_potion') { break }
                Start-Sleep -Milliseconds 300
            }
            if ($r.Body -match 'health_potion') {
                Write-Host "    attempt $i -> char $ncid granted health_potion"
                $reloadOk = $true; break
            }
            Start-Sleep -Milliseconds 500
        }
        if ($reloadOk) { Pass 'new character granted health_potion (config.changed C->B live-reloaded CachedConfig + starter spec)' } else { Fail 'starter never changed to health_potion cross-process (config live-reload failed)' }

        # Reset to default so reruns start clean.
        Invoke-Sql "DELETE FROM config.settings WHERE namespace='inventory' AND key='starter_item';" | Out-Null
    }

    Write-Host ''
    Write-Host '========= ADMIN PORTAL (gateway-svc passthrough -> admin-svc -> providers over edge) ========='
    # The admin fan-out end-to-end: a browser hits gateway-svc :8082 /admin, reverse-
    # proxied (Step 7 passthrough) to admin-svc :8085, which fetches each provider's
    # admin page over the mTLS QUIC edge. The characters page must render a character
    # CREATED on characters-svc -- proving the data crossed TWO process hops.
    $aproof = "AdminProof-$RunSuffix"
    Write-Host "[AD0] create a character named $aproof through G -> A (for the admin table assertion)"
    $acr = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/characters",
        '-H', "Authorization: Bearer $Token", '-H', 'Content-Type: application/json',
        '-d', "{`"name`":`"$aproof`",`"class`":`"ranger`"}")
    if ($acr.Body -match '"id":"([^"]*)"') { Pass "admin-proof character created (id=$($Matches[1]))" } else { Fail 'admin-proof character not created' }

    Write-Host "[AD1] GET http://127.0.0.1:$GPort/admin WITHOUT Basic auth -> 401 (ADMIN_USER set on E)"
    $an = Invoke-Curl @("http://127.0.0.1:$GPort/admin")
    Write-Host "    -> HTTP $($an.Code)"
    if ($an.Code -eq '401') { Pass 'unauthenticated /admin -> 401 through the passthrough (Basic-auth gate live on admin-svc)' } else { Fail "unauthenticated /admin expected 401, got $($an.Code)" }

    Write-Host "[AD2] GET http://127.0.0.1:$GPort/admin/characters WITH Basic auth -> 200 + contains $aproof"
    $ad = Invoke-Curl @('-u', "${AdminUser}:${AdminPass}", "http://127.0.0.1:$GPort/admin/characters")
    Write-Host "    -> HTTP $($ad.Code)  (body $($ad.Body.Length) chars)"
    if ($ad.Code -eq '200' -and $ad.Body -match [regex]::Escape($aproof)) {
        Pass "admin /admin/characters renders $aproof cross-process (G passthrough -> E -> A admin.adminData over QUIC)"
    } else {
        Fail "admin characters page expected 200 containing $aproof, got $($ad.Code)"
    }

    Write-Host ''
    Write-Host "========= AUDIT LEDGER (durable events -> audit-svc :$FPort) ========="
    # The append-only ledger end-to-end across processes: each producer's relay POSTs its
    # durable event to audit-svc's /events, and audit's on_tx_raw records it in schema
    # `audit` (exactly-once, on the inbox-dedup tx). Assert the ROWS on the shared DB (the
    # definitive proof the cross-process record handler ran): the character CREATED in [1]
    # + DELETED in [4], and the player REGISTERED in [A1]. Then the "Audit Log" admin page
    # renders through the gateway passthrough (G -> E -> F over QUIC).
    if (-not $Psql) {
        Note 'psql not found -- SKIPPING the audit ledger DB assertions'
    } else {
        Write-Host "[AU1] poll audit.log on F for character.created + character.deleted rows (cid=$cid)"
        $auOk = $false
        for ($i = 1; $i -le 30; $i++) {
            $anC = ("" + (Invoke-Sql "SELECT count(*) FROM audit.log WHERE topic='character.created' AND payload->>'character_id'='$cid';")).Trim()
            $anD = ("" + (Invoke-Sql "SELECT count(*) FROM audit.log WHERE topic='character.deleted' AND payload->>'character_id'='$cid';")).Trim()
            Write-Host "    attempt $i -> created=$anC deleted=$anD"
            if ($anC -eq '1' -and $anD -eq '1') { Pass "audit-svc recorded character.created + character.deleted for $cid (durable A->F, exactly-once)"; $auOk = $true; break }
            Start-Sleep -Milliseconds 500
        }
        if (-not $auOk) { Fail "audit-svc never recorded both character events for $cid (durable delivery A->F)" }

        Write-Host "[AU2] poll audit.log on F for the player.registered row (pid=$PlayerId)"
        $au2Ok = $false
        for ($i = 1; $i -le 30; $i++) {
            $anR = ("" + (Invoke-Sql "SELECT count(*) FROM audit.log WHERE topic='player.registered' AND payload->>'player_id'='$PlayerId';")).Trim()
            Write-Host "    attempt $i -> player.registered=$anR"
            if ($anR -eq '1') { Pass "audit-svc recorded player.registered for $PlayerId (durable D->F)"; $au2Ok = $true; break }
            Start-Sleep -Milliseconds 500
        }
        if (-not $au2Ok) { Fail "audit-svc never recorded player.registered for $PlayerId (durable delivery D->F)" }
    }

    Write-Host "[AU3] GET http://127.0.0.1:$GPort/admin/audit-log WITH Basic auth -> 200 + a logged topic"
    $aud = Invoke-Curl @('-u', "${AdminUser}:${AdminPass}", "http://127.0.0.1:$GPort/admin/audit-log")
    Write-Host "    -> HTTP $($aud.Code)  (body $($aud.Body.Length) chars)"
    if ($aud.Code -eq '200' -and $aud.Body -match 'character\.(created|deleted)|player\.registered') {
        Pass 'admin /admin/audit-log renders the ledger cross-process (G passthrough -> E -> F admin.adminData over QUIC)'
    } else {
        Fail "admin audit-log page expected 200 containing a logged topic, got $($aud.Code)"
    }

    Write-Host ''
    Write-Host "========= SCHEDULER (scheduler-svc :$HPort -> audit-svc :$FPort) ========="
    # The data-driven durable emitter end-to-end: seed a short (2s) schedule on H's shared
    # DB, immediately due. H's 1s loop acquires the per-schedule advisory lock, re-checks
    # still-due, bumps last_fired + emit_tx's scheduler.fired IN ONE TX, and its relay
    # POSTs to audit-svc. Assert on the shared DB: (i) a scheduler.fired outbox row on H's
    # origin for proof-tick (advisory-lock fire), and (ii) that row RELAYED (sent_at IS
    # NOT NULL) -- audit-svc (F) accepted it on /events (H -> F cross-process delivery).
    if (-not $Psql) {
        Note 'psql not found -- SKIPPING the scheduler assertion'
    } else {
        Write-Host "[SC0] seed a 2s schedule 'proof-tick' on the shared DB (immediately due, epoch last_fired)"
        Invoke-Sql "INSERT INTO scheduler.schedules (name, interval_seconds, last_fired) VALUES ('proof-tick', 2, to_timestamp(0)) ON CONFLICT (name) DO UPDATE SET interval_seconds=2, last_fired=to_timestamp(0);" | Out-Null
        Write-Host "[SC1] poll asyncevents.outbox on H's origin for a produced+relayed scheduler.fired{proof-tick}"
        $scOk = $false
        for ($i = 1; $i -le 30; $i++) {
            $scFired = ("" + (Invoke-Sql "SELECT count(*) FROM asyncevents.outbox WHERE origin='scheduler-svc' AND topic='scheduler.fired' AND payload->>'name'='proof-tick';")).Trim()
            $scSent = ("" + (Invoke-Sql "SELECT count(*) FROM asyncevents.outbox WHERE origin='scheduler-svc' AND topic='scheduler.fired' AND payload->>'name'='proof-tick' AND sent_at IS NOT NULL;")).Trim()
            Write-Host "    attempt $i -> fired=$scFired relayed=$scSent"
            if ([int]($scFired -as [int]) -ge 1 -and [int]($scSent -as [int]) -ge 1) {
                Pass 'scheduler-svc fired proof-tick durably (advisory-lock + stillDue re-check) AND relayed it to audit-svc (H->F)'
                $scOk = $true; break
            }
            Start-Sleep -Milliseconds 500
        }
        if (-not $scOk) { Fail 'scheduler.fired{proof-tick} never produced+relayed (scheduler H -> audit F)' }
        Invoke-Sql "DELETE FROM scheduler.schedules WHERE name='proof-tick';" | Out-Null
    }

    Write-Host ''
    Write-Host "========= MATCH TRIO (match-svc :$IPort + rating-svc :$JPort + leaderboard-svc :$KPort) ========="
    # The match trio end-to-end across processes, all through the gateway front door:
    #   (i)   POST /match/report through G (AuthNone) -> 202. G routes match.report Remote
    #         to match-svc (I); I SYNC-reads both players' MMR from rating-svc (J) over QUIC
    #         (a 202 with J UP proves that sync seam), records the match + emit_tx's
    #         match.finished in one tx.
    #   (ii)  GET /leaderboard through G shows the winner with wins=1 (poll -- durable I->K
    #         delivery is async), proving the on_tx upsert ran AND G routes topScores to K.
    #   (iii) audit-svc (F) has a match.finished row (durable I->F, exactly-once).
    #   (iv)  a second report for the SAME winner -> wins=2 (accumulating upsert).
    #   (v)   rating (in-memory, no public read op): the sync MMR read is proven by (i)
    #         succeeding with J UP; the +15/-15 handler is covered by rating's unit tests.
    $Winner = "champ-$RunSuffix"
    $Loser = "chump-$RunSuffix"

    Write-Host "[MT1] POST http://127.0.0.1:$GPort/match/report (AuthNone, capitalized Winner/Loser body keys)"
    $mr = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/match/report",
        '-H', 'Content-Type: application/json',
        '-d', "{`"Winner`":`"$Winner`",`"Loser`":`"$Loser`"}")
    Write-Host "    -> HTTP $($mr.Code)"
    if ($mr.Code -eq '202') {
        Pass 'match.report through G -> 202 (AuthNone; match-svc read rating.mmr from rating-svc over QUIC, recorded + emit_tx match.finished)'
    } else {
        Fail "match.report expected 202, got $($mr.Code)"
    }

    Write-Host "[MT2] poll GET http://127.0.0.1:$GPort/leaderboard through G until $Winner shows wins=1"
    $lbOk = $false
    for ($i = 1; $i -le 30; $i++) {
        $lb = Invoke-Curl @("http://127.0.0.1:$GPort/leaderboard")
        if ($lb.Body -match "`"player`":`"$Winner`",`"wins`":1") {
            Write-Host "    attempt $i -> $($lb.Body)"
            Pass "leaderboard shows $Winner wins=1 (durable match.finished I->K + on_tx upsert; G routes leaderboard.topScores Remote to K)"
            $lbOk = $true; break
        }
        Start-Sleep -Milliseconds 500
    }
    if (-not $lbOk) { Fail "leaderboard never showed $Winner wins=1 (durable I->K delivery / upsert / routing)" }

    if (-not $Psql) {
        Note 'psql not found -- SKIPPING the match.finished audit-ledger assertion'
    } else {
        Write-Host "[MT3] poll audit.log on F for a match.finished row (winner=$Winner)"
        $mt3Ok = $false
        for ($i = 1; $i -le 30; $i++) {
            $anMf = ("" + (Invoke-Sql "SELECT count(*) FROM audit.log WHERE topic='match.finished' AND payload->>'winner'='$Winner';")).Trim()
            Write-Host "    attempt $i -> match.finished=$anMf"
            if ([int]($anMf -as [int]) -ge 1) {
                Pass "audit-svc recorded match.finished for $Winner (durable I->F, exactly-once)"
                $mt3Ok = $true; break
            }
            Start-Sleep -Milliseconds 500
        }
        if (-not $mt3Ok) { Fail "audit-svc never recorded match.finished for $Winner (durable delivery I->F)" }
    }

    Write-Host "[MT4] second POST /match/report same winner -> leaderboard wins=2 (accumulating upsert)"
    $mr2 = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/match/report",
        '-H', 'Content-Type: application/json',
        '-d', "{`"Winner`":`"$Winner`",`"Loser`":`"$Loser`"}")
    Write-Host "    -> report#2 HTTP $($mr2.Code)"
    if ($mr2.Code -ne '202') { Fail "second match.report expected 202, got $($mr2.Code)" }
    $mt4Ok = $false
    for ($i = 1; $i -le 30; $i++) {
        $lb = Invoke-Curl @("http://127.0.0.1:$GPort/leaderboard")
        if ($lb.Body -match "`"player`":`"$Winner`",`"wins`":2") {
            Write-Host "    attempt $i -> $Winner wins=2"
            Pass "second report -> $Winner wins=2 (leaderboard upsert accumulates across durable events)"
            $mt4Ok = $true; break
        }
        Start-Sleep -Milliseconds 500
    }
    if (-not $mt4Ok) { Fail "leaderboard never reached wins=2 for $Winner (accumulating upsert)" }

    if ($Psql) { Invoke-Sql "DELETE FROM leaderboard.scores WHERE player IN ('$Winner','$Loser');" | Out-Null }

    Write-Host ''
    Write-Host "========= PLAYER QUIC FRONT (via gateway-svc :$PlayerPort) ========="

    # --- P1. player QUIC create -> G -> mTLS edge -> A ---
    # A FRESH character owned by the registered player, created THROUGH the QUIC player front (the
    # original cid from [1] was deleted in [4]). playercli exits 0 iff transport ok AND
    # the payload's status=="Ok".
    Write-Host "[P1] playercli characters.create over QUIC :$PlayerPort (--token <real>)"
    $p1 = Invoke-PlayerCli @('--addr', "127.0.0.1:$PlayerPort", '--ca', $CaCert, '--token', $Token, 'characters.create', '{"name":"hero","class":""}')
    Write-Host "    -> rc=$($p1.Rc)  $($p1.Out)"
    $pcid = $null
    if ($p1.Out -match '"id":"([^"]+)"') { $pcid = $Matches[1] }
    if ($p1.Rc -eq 0 -and $pcid) { Pass "player create -> exit 0, id=$pcid (player QUIC -> G -> mTLS edge -> A)" } else { Fail "player create expected exit 0 with id, got rc=$($p1.Rc)" }

    # --- P2. player QUIC inventory list -> G -> Remote -> B's NEW :9001 edge ---
    # The newest composition: P1 alone only proves the G->A leg; this proves player QUIC
    # -> G -> Remote -> B, and B in turn calls owner_of over QUIC/mTLS to A.
    Write-Host "[P2] playercli inventory.listCharacter over QUIC :$PlayerPort (player QUIC -> G -> Remote -> B :$BEdgePort)"
    $p2 = Invoke-PlayerCli @('--addr', "127.0.0.1:$PlayerPort", '--ca', $CaCert, '--token', $Token, 'inventory.listCharacter', "{`"character_id`":`"$pcid`"}")
    Write-Host "    -> rc=$($p2.Rc)  $($p2.Out)"
    if ($p2.Rc -eq 0) { Pass "player inventory list -> exit 0 (player QUIC -> G -> Remote -> B :$BEdgePort -> owner_of QUIC -> A)" } else { Fail "player inventory list expected exit 0, got rc=$($p2.Rc)" }

    # --- P3. gateway-svc HTTP front still routes cross-provider inventory.* -> B ---
    Write-Host "[P3] GET http://127.0.0.1:$GPort/inventory/character/$pcid through gateway-svc HTTP front (Bearer `$Token)"
    $p3 = Invoke-Curl @("http://127.0.0.1:$GPort/inventory/character/$pcid", '-H', "Authorization: Bearer $Token")
    Write-Host "    -> HTTP $($p3.Code)  $($p3.Body)"
    if ($p3.Code -eq '200') { Pass 'gateway-svc HTTP front routes inventory.* -> B remote -> 200' } else { Fail "gateway-svc HTTP inventory expected 200, got $($p3.Code)" }

    # --- P4. auth gate: no token / bad token on an auth op -> Unauthorized ---
    Write-Host "[P4] playercli characters.create with NO token -> exit 1 + Unauthorized"
    $p4 = Invoke-PlayerCli @('--addr', "127.0.0.1:$PlayerPort", '--ca', $CaCert, 'characters.create', '{"name":"x","class":""}')
    Write-Host "    -> rc=$($p4.Rc)  $($p4.Out)"
    if ($p4.Rc -ne 0 -and $p4.Out -match 'Unauthorized') { Pass 'no-token auth op -> exit 1 + Unauthorized (bearer required at the front)' } else { Fail "no-token expected exit 1 + Unauthorized, got rc=$($p4.Rc) $($p4.Out)" }

    Write-Host "[P4b] playercli characters.create with BAD token (nope-x) -> exit 1 + Unauthorized"
    $p4b = Invoke-PlayerCli @('--addr', "127.0.0.1:$PlayerPort", '--ca', $CaCert, '--token', 'nope-x', 'characters.create', '{"name":"x","class":""}')
    Write-Host "    -> rc=$($p4b.Rc)  $($p4b.Out)"
    if ($p4b.Rc -ne 0 -and $p4b.Out -match 'Unauthorized') { Pass 'bad-token auth op -> exit 1 + Unauthorized (token verified, not just presence)' } else { Fail "bad-token expected exit 1 + Unauthorized, got rc=$($p4b.Rc) $($p4b.Out)" }

    # --- P5. allow-list gate: wire-only method absent from the route table ---
    # characters.ownerOf has no #[http] binding, so it is NOT in the front's route table
    # -> NotFound. Proves dispatch is method-allow-listed, never a blind prefix relay.
    Write-Host "[P5] playercli characters.ownerOf (wire-only, not routable) -> exit 1 + NotFound"
    $p5 = Invoke-PlayerCli @('--addr', "127.0.0.1:$PlayerPort", '--ca', $CaCert, '--token', $Token, 'characters.ownerOf', "{`"character_id`":`"$pcid`"}")
    Write-Host "    -> rc=$($p5.Rc)  $($p5.Out)"
    if ($p5.Rc -ne 0 -and $p5.Out -match 'NotFound') { Pass 'wire-only characters.ownerOf -> exit 1 + NotFound (allow-list gate live)' } else { Fail "ownerOf expected exit 1 + NotFound, got rc=$($p5.Rc) $($p5.Out)" }

    Write-Host '============================================'

    Write-Host ''
    Write-Host '========= HTTP METRICS (private Prometheus registry + /metrics, now a core-infra module) ========='
    # metrics is now a lifecycle Module listed in EVERY main (Config::without_metrics is gone).
    #  - MX1 (peer pipeline): characters-svc (A) serves GET /metrics from its private registry.
    #    Under the single front door the /characters ops route THROUGH gateway-svc over the mTLS
    #    QUIC edge, NOT A's HTTP port, so we fire ONE recorded non-infra request at A so its own
    #    counter has an http_requests_total child.
    #  - MX2 (the point): gateway-svc (G) now lists the metrics module too, so GET /metrics is
    #    200 (was 404 under without_metrics) AND records the op traffic that flowed through the
    #    front door above. G dispatches ops via an axum FALLBACK (no per-op MatchedPath), but the
    #    front door now STAMPS each matched op's route PATTERN onto the response
    #    (httpmw::RoutePattern), so metrics labels op traffic by pattern -- the POST /characters
    #    create records path="/characters",status="201" instead of collapsing to "unmatched".
    Write-Host "[MX1] GET http://127.0.0.1:$APort/metrics on characters-svc -> 200 + http_requests_total (peer pipeline)"
    Invoke-Curl @("http://127.0.0.1:$APort/__metrics_probe") | Out-Null  # one recorded non-infra hit -> a counter child
    $mx1 = Invoke-Curl @("http://127.0.0.1:$APort/metrics")
    Write-Host "    -> HTTP $($mx1.Code)  (body $($mx1.Body.Length) chars)"
    if ($mx1.Code -eq '200' -and $mx1.Body -match 'http_requests_total') {
        Pass 'characters-svc /metrics -> 200 with http_requests_total (peer private registry serves the scrape)'
    } else {
        Fail "characters-svc /metrics expected 200 containing http_requests_total, got $($mx1.Code)"
    }

    Write-Host "[MX2] GET http://127.0.0.1:$GPort/metrics on gateway-svc -> 200 + a REAL op PATTERN label (front door now labelled per-op)"
    $mx2 = Invoke-Curl @("http://127.0.0.1:$GPort/metrics")
    Write-Host "    -> HTTP $($mx2.Code)  (body $($mx2.Body.Length) chars)"
    # The POST /characters create fronted above records under the op's route PATTERN with its
    # 201 success (label order is alphabetical: method,path,status), proving RoutePattern
    # labelling replaced the old path="unmatched" collapse.
    if ($mx2.Code -eq '200' -and $mx2.Body -match 'http_requests_total' -and $mx2.Body -match 'http_requests_total\{[^}]*path="/characters"[^}]*status="2\d\d"') {
        Pass 'gateway-svc /metrics -> 200 recording real op traffic under path="/characters" with a 2xx status (front door per-op route-pattern labels live)'
    } else {
        Fail "gateway-svc /metrics expected 200 with an http_requests_total{path=`"/characters`",status=2xx} op-pattern line, got $($mx2.Code)"
    }

    Write-Host '============================================'

    Write-Host ''
    Write-Host '========= RATE LIMITING (Step 13: gateway-svc always-on 20 rps / burst 40) ========='
    # The front door ALWAYS rate limits (Config::with_rate_limit_default(20,40)); no env
    # override here, so burst is 40. Hammer a cheap AuthNone op (GET /leaderboard) with 60
    # rapid requests from ONE IP (127.0.0.1, untrusted -> its own bucket): with burst 40 at
    # least one MUST come back 429 (the limiter short-circuits before dispatch). Then a pause
    # lets the bucket refill (20 rps) and a normal request succeeds again. /healthz is
    # SkipInfra: never throttled even under the same hammering.
    # Fire the 60 requests in PARALLEL (curl.exe -Z) from one process: sequential curls
    # spawn slowly enough that the 20 rps refill outpaces the drain, so we hammer them
    # concurrently — the bucket (burst 40) is then provably exceeded and >=20 get 429.
    $lbUrls = @(); $hzUrls = @()
    for ($i = 0; $i -lt 60; $i++) {
        $lbUrls += "http://127.0.0.1:$GPort/leaderboard"
        $hzUrls += "http://127.0.0.1:$GPort/healthz"
    }

    Write-Host "[RL1] 60 PARALLEL GET /leaderboard through G (:$GPort) -> expect >=1 HTTP 429 (burst 40)"
    $rlCodes = & curl.exe -Z --parallel-max 60 -s -o NUL -w "%{http_code}`n" @lbUrls 2>$null
    $rl429 = @($rlCodes | Where-Object { $_ -match '429' }).Count
    Write-Host "    -> $rl429 of 60 responses were HTTP 429"
    if ($rl429 -ge 1) {
        Pass 'gateway-svc rate limited a rapid burst (>=1 429 over 60 parallel requests, burst 40)'
    } else {
        Fail 'gateway-svc never returned 429 over 60 parallel requests (rate limiting inactive?)'
    }

    Write-Host '[RL2] 60 PARALLEL GET /healthz through G -> expect ZERO 429 (SkipInfra)'
    $hzCodes = & curl.exe -Z --parallel-max 60 -s -o NUL -w "%{http_code}`n" @hzUrls 2>$null
    $rlHz = @($hzCodes | Where-Object { $_ -match '429' }).Count
    Write-Host "    -> $rlHz of 60 /healthz responses were HTTP 429"
    if ($rlHz -eq 0) {
        Pass '/healthz never rate limited under 60 rapid probes (SkipInfra holds)'
    } else {
        Fail "/healthz returned 429 $rlHz times (SkipInfra broken)"
    }

    Write-Host '[RL3] pause 2s for the bucket to refill, then GET /leaderboard -> 200'
    Start-Sleep -Seconds 2
    $rlOk = Invoke-Curl @("http://127.0.0.1:$GPort/leaderboard")
    Write-Host "    -> post-pause GET /leaderboard -> HTTP $($rlOk.Code)"
    if ($rlOk.Code -eq '200') {
        Pass 'token bucket refilled after a pause -> GET /leaderboard 200 (limiter recovers)'
    } else {
        Fail "post-pause GET /leaderboard expected 200, got $($rlOk.Code)"
    }

    Write-Host '============================================'

    # ========================================================================
    # MONOLITH PARITY: the SAME player QUIC front, all ops dispatched Local. Per the
    # never-monolith-only-features rule both topologies must serve the feature. Tear
    # the split down first (frees :8080 and :9100 and the DB), then boot cmd/server
    # with PLAYER_EDGE_ADDR=:9100 + the shared CA and drive one player create.
    # ========================================================================
    Write-Host ''
    Write-Host '================ MONOLITH PARITY ================'
    Note 'tearing down the split before the monolith stage ...'
    Teardown
    foreach ($n in 'characters-svc', 'inventory-svc', 'gateway-svc', 'config-svc', 'accounts-svc', 'admin-svc', 'audit-svc', 'scheduler-svc', 'match-svc', 'rating-svc', 'leaderboard-svc', 'server') {
        Get-Process -Name $n -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Seconds 2
    # Per-process env in this script leaks across Start-Svc calls; neutralize the
    # split-only knobs so the monolith gets a clean events config AND an OPEN admin
    # portal (no leaked Basic-auth creds; /admin is served locally, not proxied).
    $env:EVENTS_SUBSCRIBERS = ''
    $env:EVENTS_ORIGIN = ''
    $env:ADMIN_HTTP_ADDR = ''
    $env:ACCOUNTS_HTTP_ADDR = ''
    $env:ADMIN_USER = ''
    $env:ADMIN_PASS = ''

    Note "starting monolith (cmd/server) on :$APort, player QUIC :$PlayerPort ..."
    $script:MProc = Start-Svc (Join-Path $BinDir 'server.exe') @{
        PORT             = ":$APort"
        DATABASE_URL     = $env:DATABASE_URL
        PLAYER_EDGE_ADDR = ":$PlayerPort"
        EDGE_CA_CERT     = $CaCert
        EDGE_CA_KEY      = $CaKey
    } 'monolith'
    if (Wait-Healthy $APort 'monolith (server)') {
        $msuffix = [guid]::NewGuid().ToString().Substring(0, 8)
        Write-Host '[M0] register a player on the monolith (accounts module local, real session)'
        $mreg = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$APort/accounts/register",
            '-H', 'Content-Type: application/json',
            '-d', "{`"email`":`"mono-$msuffix@test.local`",`"password`":`"pw-$msuffix`",`"displayName`":`"Mono`"}")
        $mtoken = $null
        if ($mreg.Body -match '"token":"([^"]+)"') { $mtoken = $Matches[1] }
        if ($mtoken) { Pass 'monolith register -> real bearer (parity: same auth flow, all Local)' } else { Fail "monolith register produced no token -- $($mreg.Body)" }
        Write-Host "[M1] playercli characters.create over QUIC :$PlayerPort against the monolith (--token <real>)"
        $m1 = Invoke-PlayerCli @('--addr', "127.0.0.1:$PlayerPort", '--ca', $CaCert, '--token', $mtoken, 'characters.create', '{"name":"solo","class":""}')
        Write-Host "    -> rc=$($m1.Rc)  $($m1.Out)"
        if ($m1.Rc -eq 0) { Pass 'monolith player QUIC front -> exit 0 (all ops Local, parity proven)' } else { Fail "monolith player create expected exit 0, got rc=$($m1.Rc)" }
        Write-Host '[M2] monolith rejects a dev- token (real verifier resolved from the local accounts module)'
        $m2 = Invoke-PlayerCli @('--addr', "127.0.0.1:$PlayerPort", '--ca', $CaCert, '--token', "dev-$msuffix", 'characters.create', '{"name":"x","class":""}')
        Write-Host "    -> rc=$($m2.Rc)  $($m2.Out)"
        if ($m2.Rc -ne 0 -and $m2.Out -match 'Unauthorized') { Pass 'monolith dev- token -> Unauthorized (parity with the split front)' } else { Fail "monolith dev- token expected Unauthorized, got rc=$($m2.Rc) $($m2.Out)" }
        # [M3] admin portal parity: the monolith hosts the admin module with all four
        # providers LOCAL (no fan-out, no ADMIN_USER -> open). The characters page
        # renders the just-created "solo" character (never-monolith-only-features).
        Write-Host "[M3] GET http://127.0.0.1:$APort/admin/characters on the monolith -> 200 + contains solo"
        $m3 = Invoke-Curl @("http://127.0.0.1:$APort/admin/characters")
        Write-Host "    -> HTTP $($m3.Code)  (body $($m3.Body.Length) chars)"
        if ($m3.Code -eq '200' -and $m3.Body -match 'solo') { Pass 'monolith /admin/characters renders LOCAL items (admin portal parity)' } else { Fail "monolith admin characters page expected 200 containing solo, got $($m3.Code)" }
    } else {
        Fail "monolith (server) never became healthy on :$APort"
    }

    Write-Host '============================================'
}
finally {
    Teardown
}

if ($script:Fails -eq 0) {
    Write-Host 'SPLIT PROOF: PASS (all assertions held on the eleven-process split + monolith parity)'
    exit 0
} else {
    Write-Host "SPLIT PROOF: FAIL ($($script:Fails) assertion(s) failed)"
    exit 1
}
