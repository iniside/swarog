# split-proof.ps1 -- the SPLIT-topology proof for the rust-sketch (Steps 12 + 8).
#
# The whole point of the milestone: exercises the TWELVE-PROCESS split (characters-svc =
# A on :8080 / edge :9000, inventory-svc = B on :8081 / edge :9001, gateway-svc = G on
# :8082 / player QUIC :9100, config-svc = C on :8083 / edge :9002, accounts-svc = D on
# :8084 / edge :9003, admin-svc = E on :8085, audit-svc = F on :8086 / edge :9004,
# scheduler-svc = H on :8087 / edge :9005, match-svc = I on :8088 / edge :9006,
# rating-svc = J on :8089 / edge :9007, leaderboard-svc = K on :8090 / edge :9008,
# apikeys-svc = L on :8091 / edge :9009), NOT
# the monolith, driving the real player flows over HTTP
#
# Port assignments here are manual config (this table); FLEET MEMBERSHIP (the set of
# cmd/*-svc processes) is the drift-guarded source of truth in
# tools/checkmodules::split_fleet_matches_cmd_dirs (Step 15) -- add a new svc there
# before adding it to this script. This script ALSO self-checks that assumption:
# a preflight compares $FleetSvcs against the cmd/*-svc directories on disk and
# dies (naming exactly what is missing/stale) BEFORE booting anything, so a
# forgotten svc is a loud failure, never a silently weaker proof.
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
#   - API KEY POLICY (api-key-policy plan, Step 4): every op-dispatched request now
#     ALSO carries X-Api-Key/--api-key, checked BEFORE session auth. Every op curl
#     below carries dev-key-client (the player-facing policy) EXCEPT match/report,
#     which carries dev-key-server (full) -- dev-client's policy deliberately omits
#     match.report. [K1]-[K4] right after [A5] assert the policy directly: no key ->
#     401, bogus key -> 401, dev-key-client on match.report -> 403 (policy denies),
#     dev-key-server on match.report -> 202 (allowed). Keyless surfaces stay keyless:
#     /healthz, /metrics, /admin* (session-auth passthrough).
#   - REAL AUTH (Step 6): register + login through G's front mint a DB-backed session
#     on D; the bearer then authorizes ops on every process (each gateway verifies it
#     against D's accounts.verifySession over QUIC/mTLS). NEGATIVE: a garbage token
#     and a dev-<uuid> token are both 401 through G (no ACCOUNTS_DEV_AUTH anywhere).
#   - Async event A->B: POST /characters on A -> 201; A appends character.created to
#     the shared durable log; B's pull worker delivers it to inventory's durable
#     on_tx, which grants the starter item. Poll GET /inventory/character/<id> on B
#     until starter_sword x1 appears.
#   - Sync call over QUIC B->A: that same GET forces list_character to call owner_of
#     via the remote Stub over QUIC/mTLS to A -- a 200 with the holding proves the
#     sync path AND mTLS. NEGATIVE authz: the same GET as a DIFFERENT player -> 403.
#   - Integrity via event (not FK) A->B: DELETE /characters/<id> on A -> 204; A emits
#     character.deleted; inventory's on_tx wipes the holdings. Assert the DB holdings
#     row is genuinely gone (the HTTP 404 after delete alone only proves the character
#     is gone via owner_of and would mask an un-wiped row).
#   - CONFIG live-reload C->B (Step 7): change inventory/starter_item at runtime via
#     psql; config's write trigger bumps the revision, pg_notifies config_changed, and
#     appends config.changed durably. B's invalidation plane (LISTENing config_changed on
#     the shared DB) refreshes CachedConfig; inventory's starter spec reloads on the
#     durable event. A NEWLY created character then gets the NEW starter -- cross-process
#     live reload with no restart.
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

# --- Live log tee: every invocation writes its full console output to a timestamped
# log file (in addition to the console), with the log path printed FIRST so a human or
# an agent can tail it live. PS7 supports nested transcripts, which matters when
# verify.ps1 invokes this script as a child stage.
$LogsDir = Join-Path $PSScriptRoot 'run/logs'
New-Item -ItemType Directory -Force -Path $LogsDir | Out-Null
$LogPath = Join-Path $LogsDir "split-proof-$(Get-Date -Format 'yyyyMMdd-HHmmss').log"
Write-Host "[log] $LogPath"
Start-Transcript -Path $LogPath | Out-Null

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
$LPort     = 8091
$EdgePort  = 9000
$BEdgePort = 9001
$CEdgePort = 9002
$DEdgePort = 9003
$FEdgePort = 9004
$HEdgePort = 9005
$IEdgePort = 9006
$JEdgePort = 9007
$KEdgePort = 9008
$LEdgePort = 9009
$PlayerPort = 9100
$PlayerCli = Join-Path $BinDir 'playercli.exe'

# The svc processes this proof boots -- the ONE hand-maintained fleet list (the build
# list and the straggler-kill list both derive from it). The preflight below pins it
# to the cmd/*-svc directories on disk.
$FleetSvcs = @(
    'characters-svc', 'inventory-svc', 'gateway-svc', 'config-svc', 'accounts-svc',
    'admin-svc', 'audit-svc', 'scheduler-svc', 'match-svc', 'rating-svc',
    'leaderboard-svc', 'apikeys-svc'
)

$DefaultDsn = 'postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable'
if (-not $env:DATABASE_URL -or $env:DATABASE_URL.Trim() -eq '') { $env:DATABASE_URL = $DefaultDsn }

# Session-auth admin logins for the proof. proofadmin is the happy-path operator;
# prooflock is a dedicated user we deliberately lock out ([AD2]). Both are minted
# pre-boot via adminctl (session auth replaced the old ADMIN_USER/ADMIN_PASS Basic gate).
$ProofAdminUser = 'proofadmin'
$ProofAdminPass = 'proofpass'
$ProofLockUser  = 'prooflock'
$ProofLockPass  = 'lockpass'

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
$script:LProc = $null
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
if (-not $Psql) {
    Write-Error 'split-proof: psql not found -- local Postgres is the test DB and the DB assertions are mandatory; install psql or put it on PATH'
    exit 1
}

# Run one SQL statement against the test DB. Follows DATABASE_URL natively -- psql
# accepts a connection URI directly, so no DSN parsing is needed and percent-encoded
# passwords / sslmode query params ride along for free.
function Invoke-Sql([string]$Sql) {
    $out = & $Psql $env:DATABASE_URL -v ON_ERROR_STOP=1 -t -A -c $Sql 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Error "FATAL psql rc=$LASTEXITCODE for: $Sql`n$out"
        throw "psql failed (rc=$LASTEXITCODE) for: $Sql"
    }
    return $out
}

# Health-check and player HTTP go to 127.0.0.1, NOT localhost: on Windows `localhost`
# resolves to IPv6 ::1 first, but the services bind IPv4 0.0.0.0, so Invoke-WebRequest
# would hang on ::1.
function Wait-Healthy([int]$Port, [string]$Name) {
    for ($i = 0; $i -lt 60; $i++) {
        try {
            Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:$Port/readyz" -TimeoutSec 2 | Out-Null
            Note "$Name healthy on :$Port"; return $true
        } catch { Start-Sleep -Milliseconds 500 }
    }
    Note "$Name NEVER became healthy on :$Port"
    try {
        $resp = Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:$Port/readyz" -TimeoutSec 2 -SkipHttpErrorCheck
        Note "  readyz body: $($resp.Content)"
    } catch { Note "  readyz body: $($_.Exception.Message)" }
    return $false
}

function Start-Svc([string]$Exe, [hashtable]$EnvVars, [string]$LogName) {
    foreach ($k in $EnvVars.Keys) { Set-Item -Path "Env:$k" -Value $EnvVars[$k] }
    $out = Join-Path $RunDir "$LogName.out.log"
    $err = Join-Path $RunDir "$LogName.err.log"
    $pidFile = Join-Path $RunDir "$LogName.pid"
    Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue
    # Run the short-lived spawner through Start-Process with file-backed output.
    # Calling it directly under verify's redirected native pipeline leaves another
    # inheritable pipe handle in the spawned service; PowerShell then waits for EOF
    # until that long-lived service exits even though winctrl itself already returned.
    $spawnOut = Join-Path $RunDir "$LogName.spawn.out.log"
    $spawnErr = Join-Path $RunDir "$LogName.spawn.err.log"
    $spawn = Start-Process -FilePath $Winctrl -ArgumentList @(
        'spawn', '--pid-file', $pidFile, '--stdout', $out, '--stderr', $err, '--', $Exe
    ) -PassThru -NoNewWindow -RedirectStandardOutput $spawnOut -RedirectStandardError $spawnErr
    # Start-Process -Wait deliberately waits for the entire descendant tree, which
    # includes the long-lived service winctrl just created. WaitForExit targets only
    # the short-lived winctrl process handle.
    $spawn.WaitForExit()
    if ($spawn.ExitCode -ne 0 -or -not (Test-Path -LiteralPath $pidFile)) {
        throw "winctrl failed to spawn $LogName"
    }
    $childPid = [int](Get-Content -LiteralPath $pidFile -Raw).Trim()
    return Get-Process -Id $childPid -ErrorAction Stop
}

function Stop-Svc([System.Diagnostics.Process]$Proc, [string]$Label) {
    if (-not $Proc) { return $true }
    if ($Proc.HasExited) {
        # A real process handle that was already gone before we sent CTRL_BREAK — we
        # never observed a drain, so this can't be scored graceful regardless of what
        # killed it. Exit-code read is best-effort for the log line; an
        # inaccessible/unknown code still means $false.
        try { $ec = $Proc.ExitCode } catch { $ec = '?' }
        Note "$Label already exited (code $ec) before shutdown was initiated"
        return $false
    }
    & $Winctrl break $Proc.Id
    $breakSent = $LASTEXITCODE -eq 0
    if ($breakSent -and $Proc.WaitForExit(10000)) {
        if ($Proc.ExitCode -eq 0) {
            Note "gracefully stopped $Label (pid $($Proc.Id))"
            return $true
        }
        Note "$Label (pid $($Proc.Id)) drained but exited $($Proc.ExitCode)"
        return $false
    }
    Note "$Label (pid $($Proc.Id)) did not drain after CTRL_BREAK; forcing"
    Stop-Process -Id $Proc.Id -Force -ErrorAction SilentlyContinue
    return $false
}

function Teardown([bool]$AssertGraceful = $false, [string]$Assertion = 'W graceful shutdown') {
    $graceful = $true
    $entries = @(
        [pscustomobject]@{ Proc=$script:AProc; Label='A' }, [pscustomobject]@{ Proc=$script:BProc; Label='B' },
        [pscustomobject]@{ Proc=$script:GProc; Label='G' }, [pscustomobject]@{ Proc=$script:CProc; Label='C' },
        [pscustomobject]@{ Proc=$script:DProc; Label='D' }, [pscustomobject]@{ Proc=$script:EProc; Label='E' },
        [pscustomobject]@{ Proc=$script:FProc; Label='F' }, [pscustomobject]@{ Proc=$script:HProc; Label='H' },
        [pscustomobject]@{ Proc=$script:IProc; Label='I' }, [pscustomobject]@{ Proc=$script:JProc; Label='J' },
        [pscustomobject]@{ Proc=$script:KProc; Label='K' }, [pscustomobject]@{ Proc=$script:LProc; Label='L' },
        [pscustomobject]@{ Proc=$script:MProc; Label='monolith' })
    foreach ($entry in $entries) {
        if (-not (Stop-Svc $entry.Proc $entry.Label)) { $graceful = $false }
    }
    if ($AssertGraceful) {
        if ($graceful) { Pass "[$Assertion] CTRL_BREAK drained every process without Force fallback" }
        else { Fail "[$Assertion] at least one process required Force fallback" }
    }
    $script:AProc = $null; $script:BProc = $null; $script:GProc = $null; $script:CProc = $null; $script:DProc = $null; $script:EProc = $null; $script:FProc = $null; $script:HProc = $null; $script:IProc = $null; $script:JProc = $null; $script:KProc = $null; $script:LProc = $null; $script:MProc = $null
}

# Runs playercli, capturing stdout (joined) and the process exit code. Returns a
# pscustomobject { Rc; Out }. playercli exits 0 iff transport ok AND status=="Ok".
function Invoke-PlayerCli([string[]]$CliArgs) {
    $out = & $PlayerCli @CliArgs 2>&1
    $rc = $LASTEXITCODE
    return [pscustomobject]@{ Rc = $rc; Out = (($out | Out-String)).Trim() }
}

try {
    # --- Fleet-membership tripwire: the boot blocks below are inherently manual
    # (ports, env, named assertions), so VERIFY the "I didn't forget a svc"
    # assumption instead of trusting memory. Compares $FleetSvcs against the
    # cmd/*-svc directories on disk and dies BEFORE booting anything, naming
    # exactly what drifted and what to do about it.
    $DiskSvcs = @(Get-ChildItem (Join-Path $PSScriptRoot 'cmd') -Directory |
        Where-Object { $_.Name -like '*-svc' } | ForEach-Object { $_.Name })
    $NotBooted = @($DiskSvcs  | Where-Object { $FleetSvcs -notcontains $_ })
    $NotOnDisk = @($FleetSvcs | Where-Object { $DiskSvcs  -notcontains $_ })
    if ($NotBooted.Count -or $NotOnDisk.Count) {
        Note 'FATAL fleet drift: the svcs this script boots != the cmd/*-svc directories on disk.'
        foreach ($n in $NotBooted) {
            Note "  cmd/$n exists but this script never boots it -- add a port, a boot block, and a named assertion for it (CLAUDE.md 'Adding a module' step 4), then extend `$FleetSvcs"
        }
        foreach ($n in $NotOnDisk) {
            Note "  `$FleetSvcs lists '$n' but cmd/$n does not exist -- remove its entry and boot block, or restore the crate"
        }
        throw "fleet drift: $($NotBooted.Count) svc(s) not booted, $($NotOnDisk.Count) stale entr(ies) -- see [proof] lines above"
    }
    Note "fleet preflight OK: $($FleetSvcs.Count) svcs booted here == cmd/*-svc on disk"

    $BuildPkgs = @('edgeca', 'winctrl') + $FleetSvcs + @('adminctl', 'playercli', 'csharp-client-gen', 'server')
    Note "building $($BuildPkgs -join ' + ') ..."
    $CargoArgs = @($BuildPkgs | ForEach-Object { @('-p', $_) })
    cargo build @CargoArgs
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

    New-Item -ItemType Directory -Force -Path $RunDir | Out-Null
    $Winctrl = Join-Path $BinDir 'winctrl.exe'

    # Clear stragglers from an aborted prior run so ports are free (idempotent reruns).
    foreach ($n in ($FleetSvcs + 'server')) {
        Get-Process -Name $n -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Milliseconds 500

    Note "minting shared edge dev CA -> $CaCert"
    & (Join-Path $BinDir 'edgeca.exe') --cert $CaCert --key $CaKey
    if ($LASTEXITCODE -ne 0) { throw 'edgeca failed' }

    # Seed the admin logins PRE-BOOT (session auth replaced Basic auth). adminctl ensures
    # schema `admin` + admin.users itself and upserts the login (password over stdin,
    # never argv), so it runs before admin-svc migrates the rest of the schema.
    $AdminCtl = Join-Path $BinDir 'adminctl.exe'
    foreach ($seed in @(@($ProofAdminUser, $ProofAdminPass), @($ProofLockUser, $ProofLockPass))) {
        $seed[1] | & $AdminCtl create-user $seed[0] --password-stdin | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "failed to seed admin user $($seed[0]) via adminctl" }
        Note "seeded admin login $($seed[0]) (adminctl create-user)"
    }

    # D (accounts-svc) FIRST: owns the accounts schema and serves accounts.verifySession
    # + the auth op faces on its mTLS edge; every other gateway verifies bearers here.
    # ACCOUNTS_DEV_AUTH=1: dev/password auth is now an explicit opt-in (fail-closed
    # default) and D hosts the accounts module, so the register/login the proof drives
    # (via G Remote) need it enabled here. G never sets it -- [A5] still proves 401.
    Note "starting D (accounts-svc) on :$DPort, edge :$DEdgePort ..."
    $script:DProc = Start-Svc (Join-Path $BinDir 'accounts-svc.exe') @{
        PORT               = ":$DPort"
        DATABASE_URL       = $env:DATABASE_URL
        EDGE_ADDR          = ":$DEdgePort"
        EDGE_CA_CERT       = $CaCert
        EDGE_CA_KEY        = $CaKey
        ACCOUNTS_DEV_AUTH  = '1'
        EPIC_CLIENT_ID     = 'test'
        EPIC_CLIENT_SECRET = 'test'
        EPIC_TOKEN_URL     = 'http://127.0.0.1:1/token'
    } 'accounts'
    if (-not (Wait-Healthy $DPort 'D (accounts-svc)')) { throw 'D failed to start' }

    # L (apikeys-svc): apikeys, edge :9009. Owns the apikeys schema (plaintext key ->
    # policy) and serves apikeys.keys on its mTLS edge; gateway-svc (G) and admin-svc
    # (E) resolve/dial it via APIKEYS_EDGE_ADDR. APIKEYS_DEV_SEED=1 self-heals the two
    # well-known dev keys (dev-key-client, dev-key-server) on every boot so the K1-K4
    # assertions below have a stable fixture.
    Note "starting L (apikeys-svc) on :$LPort, edge :$LEdgePort ..."
    $script:LProc = Start-Svc (Join-Path $BinDir 'apikeys-svc.exe') @{
        PORT          = ":$LPort"
        DATABASE_URL  = $env:DATABASE_URL
        EDGE_ADDR     = ":$LEdgePort"
        EDGE_CA_CERT  = $CaCert
        EDGE_CA_KEY   = $CaKey
        APIKEYS_DEV_SEED = '1'
    } 'apikeys'
    if (-not (Wait-Healthy $LPort 'L (apikeys-svc)')) { throw 'L failed to start' }

    # F (audit-svc): audit, edge :9004. A PURE CONSUMER (produces nothing): its pull
    # workers drain its six subscriptions from the shared log, and audit's on_tx_raw
    # records each on the handed delivery tx (exactly-once with the cursor advance).
    # Serves admin.adminData on its mTLS edge so admin-svc fans the "Audit Log" page
    # out over QUIC.
    Note "starting F (audit-svc) on :$FPort, edge :$FEdgePort ..."
    $script:FProc = Start-Svc (Join-Path $BinDir 'audit-svc.exe') @{
        PORT               = ":$FPort"
        DATABASE_URL       = $env:DATABASE_URL
        EDGE_ADDR          = ":$FEdgePort"
        EDGE_CA_CERT       = $CaCert
        EDGE_CA_KEY        = $CaKey
    } 'audit'
    if (-not (Wait-Healthy $FPort 'F (audit-svc)')) { throw 'F failed to start' }

    # H (scheduler-svc): scheduler, edge :9005. A DURABLE PRODUCER: its 1s
    # loop fires scheduler.fired for every due schedule (race-safe via a per-schedule
    # pg_try_advisory_lock), appending to the shared log (audit-svc pulls it). Serves
    # admin.adminData ("Schedules") on its mTLS edge so admin-svc fans it out.
    Note "starting H (scheduler-svc) on :$HPort, edge :$HEdgePort ..."
    $script:HProc = Start-Svc (Join-Path $BinDir 'scheduler-svc.exe') @{
        PORT               = ":$HPort"
        DATABASE_URL       = $env:DATABASE_URL
        EDGE_ADDR          = ":$HEdgePort"
        EDGE_CA_CERT       = $CaCert
        EDGE_CA_KEY        = $CaKey
    } 'scheduler'
    if (-not (Wait-Healthy $HPort 'H (scheduler-svc)')) { throw 'H failed to start' }

    # J (rating-svc): rating, edge :9007. Provides rating.mmr on its mTLS
    # edge (match-svc reads it sync before recording) and pulls match.finished
    # (+15/-15) from the shared log. In-memory MMR (no schema) but hosts a durable
    # subscription, so it needs a DB pool (the durable-events plane is app-owned,
    # not a module dependency).
    Note "starting J (rating-svc) on :$JPort, edge :$JEdgePort ..."
    $script:JProc = Start-Svc (Join-Path $BinDir 'rating-svc.exe') @{
        PORT             = ":$JPort"
        DATABASE_URL     = $env:DATABASE_URL
        EDGE_ADDR        = ":$JEdgePort"
        EDGE_CA_CERT     = $CaCert
        EDGE_CA_KEY      = $CaKey
    } 'rating'
    if (-not (Wait-Healthy $JPort 'J (rating-svc)')) { throw 'J failed to start' }

    # K (leaderboard-svc): gateway + leaderboard, edge :9008. Owns schema
    # leaderboard, pulls match.finished (upsert wins+1) from the shared log, and serves
    # GET /leaderboard (gateway-svc routes it Remote here).
    Note "starting K (leaderboard-svc) on :$KPort, edge :$KEdgePort ..."
    $script:KProc = Start-Svc (Join-Path $BinDir 'leaderboard-svc.exe') @{
        PORT               = ":$KPort"
        DATABASE_URL       = $env:DATABASE_URL
        EDGE_ADDR          = ":$KEdgePort"
        EDGE_CA_CERT       = $CaCert
        EDGE_CA_KEY        = $CaKey
    } 'leaderboard'
    if (-not (Wait-Healthy $KPort 'K (leaderboard-svc)')) { throw 'K failed to start' }

    # I (match-svc): gateway + match + rating stub, edge :9006. Records
    # matches (schema match) and is a DURABLE PRODUCER: report SYNC-reads both players'
    # MMR from rating-svc (J) over the mTLS edge, INSERTs the match row + emit_tx's
    # match.finished IN ONE TX onto the shared log; J, K and audit-svc (F) pull it.
    Note "starting I (match-svc) on :$IPort, edge :$IEdgePort ..."
    $script:IProc = Start-Svc (Join-Path $BinDir 'match-svc.exe') @{
        PORT               = ":$IPort"
        DATABASE_URL       = $env:DATABASE_URL
        EDGE_ADDR          = ":$IEdgePort"
        EDGE_CA_CERT       = $CaCert
        EDGE_CA_KEY        = $CaKey
        RATING_EDGE_ADDR   = "127.0.0.1:$JEdgePort"
    } 'match'
    if (-not (Wait-Healthy $IPort 'I (match-svc)')) { throw 'I failed to start' }

    Note "starting A (characters-svc) on :$APort, edge :$EdgePort ..."
    $script:AProc = Start-Svc (Join-Path $BinDir 'characters-svc.exe') @{
        PORT                = ":$APort"
        DATABASE_URL        = $env:DATABASE_URL
        EDGE_ADDR           = ":$EdgePort"
        EDGE_CA_CERT        = $CaCert
        EDGE_CA_KEY         = $CaKey
    } 'characters'
    if (-not (Wait-Healthy $APort 'A (characters-svc)')) { throw 'A failed to start' }

    # Reset the config baseline: B must boot with the DEFAULT starter (starter_sword),
    # so the later runtime change to health_potion is provably a LIVE reload. C/B are not
    # up yet, so their boot loads see no row.
    Invoke-Sql "DELETE FROM config.settings WHERE namespace='inventory' AND key='starter_item'; DELETE FROM config.settings WHERE namespace='proof';" | Out-Null
    Note 'reset config baseline (deleted inventory/starter_item)'

    # C (config-svc): owns the config schema + write trigger, serves config.snapshot on
    # its mTLS edge, and (via the trigger) bumps the revision, pg_notifies config_changed,
    # and appends config.changed durably onto the shared log (B and F pull the event; B
    # also LISTENs config_changed for cache invalidation).
    Note "starting C (config-svc) on :$CPort, edge :$CEdgePort ..."
    $script:CProc = Start-Svc (Join-Path $BinDir 'config-svc.exe') @{
        PORT               = ":$CPort"
        DATABASE_URL       = $env:DATABASE_URL
        EDGE_ADDR          = ":$CEdgePort"
        EDGE_CA_CERT       = $CaCert
        EDGE_CA_KEY        = $CaKey
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
        INVENTORY_DEV_GRANT  = '1'
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
        APIKEYS_EDGE_ADDR    = "127.0.0.1:$LEdgePort"
        ADMIN_HTTP_ADDR      = "127.0.0.1:$EPort"
        ACCOUNTS_HTTP_ADDR   = "127.0.0.1:$DPort"
    } 'gateway'
    if (-not (Wait-Healthy $GPort 'G (gateway-svc)')) { throw 'G failed to start' }

    # E (admin-svc): the admin portal fortress -- HTTP :8085, its OWN DB (schema admin:
    # users/sessions/login_attempts), no edge server. It DIALS all six peer edges
    # (A/B/C/D + audit + scheduler) to fan out their admin pages over QUIC. Session auth now
    # gates the portal (no ADMIN_USER/ADMIN_PASS): TRUSTED_PROXY_CIDRS=127.0.0.1/32 makes the
    # lockout ip:<addr> subject the real client behind G's passthrough, and
    # ADMIN_COOKIE_SECURE=0 lets curl carry the session cookie over plain http.
    Note "starting E (admin-svc) on :$EPort ..."
    $script:EProc = Start-Svc (Join-Path $BinDir 'admin-svc.exe') @{
        PORT                 = ":$EPort"
        DATABASE_URL         = $env:DATABASE_URL
        EDGE_CA_CERT         = $CaCert
        EDGE_CA_KEY          = $CaKey
        CHARACTERS_EDGE_ADDR = "127.0.0.1:$EdgePort"
        INVENTORY_EDGE_ADDR  = "127.0.0.1:$BEdgePort"
        CONFIG_EDGE_ADDR     = "127.0.0.1:$CEdgePort"
        ACCOUNTS_EDGE_ADDR   = "127.0.0.1:$DEdgePort"
        AUDIT_EDGE_ADDR      = "127.0.0.1:$FEdgePort"
        SCHEDULER_EDGE_ADDR  = "127.0.0.1:$HEdgePort"
        APIKEYS_EDGE_ADDR    = "127.0.0.1:$LEdgePort"
        TRUSTED_PROXY_CIDRS  = '127.0.0.1/32'
        ADMIN_COOKIE_SECURE  = '0'
    } 'admin'
    if (-not (Wait-Healthy $EPort 'E (admin-svc)')) { throw 'E failed to start' }

    # --- [GW-RDY] the DB-less front's /readyz reflects its peers, not a bare 200 --------
    # gateway-svc holds a Stub per consumed provider; each Stub contributes a
    # `stub:<provider>` httpmw::ReadyCheck that dials the peer's edge. With the WHOLE fleet
    # up, G's /readyz must be 200 "ok" (a bare-200 DB-less front used to answer ready even
    # with the backend dead -- the readiness probe closes that gap).
    Write-Host ''
    Write-Host "[GW-RDY] GET http://127.0.0.1:$GPort/readyz through G -> expect 200 ok (peers probed)"
    $rdy = Invoke-Curl @("http://127.0.0.1:$GPort/readyz")
    Write-Host "    -> HTTP $($rdy.Code)  $($rdy.Body)"
    if ($rdy.Code -eq '200' -and $rdy.Body -match 'ok') {
        Pass 'gateway-svc /readyz -> 200 ok with the full fleet up (per-stub probes pass)'
    } else {
        Fail "gateway-svc /readyz expected 200 ok, got $($rdy.Code) ($($rdy.Body))"; throw 'GW-RDY failed'
    }

    $RunSuffix = [guid]::NewGuid().ToString().Substring(0, 8)

    Write-Host ''
    Write-Host '================ REAL AUTH (Step 6) ================'
    # Register + login THROUGH the gateway front (G routes /accounts/* Remote to D over
    # the mTLS edge), then use the REAL bearer everywhere below. No dev- tokens.

    Write-Host "[A1] POST http://127.0.0.1:$GPort/accounts/register (through G -> D)"
    $reg = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/accounts/register",
        '-H', 'X-Api-Key: dev-key-client',
        '-H', 'Content-Type: application/json',
        '-d', "{`"email`":`"proof-$RunSuffix@test.local`",`"password`":`"pw-$RunSuffix`",`"displayName`":`"Proof`"}")
    Write-Host "    -> HTTP $($reg.Code)  $($reg.Body)"
    $PlayerId = $null
    if ($reg.Body -match '"player_id":"([^"]+)"') { $PlayerId = $Matches[1] }
    if ($reg.Code -eq '201' -and $PlayerId) { Pass "register through the front -> 201, player_id=$PlayerId" } else { Fail "register expected 201 with player_id, got $($reg.Code)"; throw 'auth bootstrap failed' }

    Write-Host "[A2] POST http://127.0.0.1:$GPort/accounts/login (fresh session through G -> D)"
    $login = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/accounts/login",
        '-H', 'X-Api-Key: dev-key-client',
        '-H', 'Content-Type: application/json',
        '-d', "{`"email`":`"proof-$RunSuffix@test.local`",`"password`":`"pw-$RunSuffix`"}")
    $Token = $null
    if ($login.Body -match '"token":"([^"]+)"') { $Token = $Matches[1] }
    Write-Host "    -> HTTP $($login.Code)  token=$(if ($Token) { $Token.Substring(0,12) })..."
    if ($login.Code -eq '200' -and $Token) { Pass 'login through the front -> 200 with a real bearer' } else { Fail "login expected 200 with token, got $($login.Code)"; throw 'auth bootstrap failed' }

    Write-Host "[A3] GET http://127.0.0.1:$GPort/accounts/me (Bearer <real token>)"
    $me = Invoke-Curl @("http://127.0.0.1:$GPort/accounts/me", '-H', 'X-Api-Key: dev-key-client', '-H', "Authorization: Bearer $Token")
    Write-Host "    -> HTTP $($me.Code)  $($me.Body)"
    if ($me.Code -eq '200' -and $me.Body -match [regex]::Escape($PlayerId)) { Pass 'me -> 200 with the registered player (auth-once verified the real session)' } else { Fail "me expected 200 with player_id, got $($me.Code)" }

    # A second player for the negative authz assertion.
    $oreg = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/accounts/register",
        '-H', 'X-Api-Key: dev-key-client',
        '-H', 'Content-Type: application/json',
        '-d', "{`"email`":`"other-$RunSuffix@test.local`",`"password`":`"pw2-$RunSuffix`",`"displayName`":`"Other`"}")
    $OtherToken = $null
    if ($oreg.Body -match '"token":"([^"]+)"') { $OtherToken = $Matches[1] }
    if (-not $OtherToken) { Fail 'second register produced no token'; throw 'auth bootstrap failed' }

    Write-Host '[A4] GET /characters through G with a GARBAGE token -> 401'
    $g1 = Invoke-Curl @("http://127.0.0.1:$GPort/characters", '-H', 'X-Api-Key: dev-key-client', '-H', 'Authorization: Bearer totally-bogus-token')
    Write-Host "    -> HTTP $($g1.Code)"
    if ($g1.Code -eq '401') { Pass 'garbage token -> 401' } else { Fail "garbage token expected 401, got $($g1.Code)" }

    Write-Host '[A5] GET /characters through G with a dev-<uuid> token -> 401 (no ACCOUNTS_DEV_AUTH on G)'
    $g2 = Invoke-Curl @("http://127.0.0.1:$GPort/characters", '-H', 'X-Api-Key: dev-key-client', '-H', "Authorization: Bearer dev-$([guid]::NewGuid())")
    Write-Host "    -> HTTP $($g2.Code)"
    if ($g2.Code -eq '401') { Pass 'dev- token -> 401 (gateway-svc verifies REAL sessions only)' } else { Fail "dev- token expected 401, got $($g2.Code)" }

    Write-Host ''
    Write-Host '================ EPIC OAUTH REDIRECT (browser flow: G passthrough -> D) ================'
    # G reverse-proxies /accounts/epic/* to accounts-svc (D). D's callback exchanges the
    # code with EPIC_TOKEN_URL (pointed at an unreachable 127.0.0.1:1) so the exchange
    # fails deterministically and D answers 302 -> /?epic=error. The proof: the gateway
    # proxy RELAYS that 303 verbatim (reqwest Policy::none()) instead of following it
    # server-side -- a follow would swallow the redirect (and the real login's #token
    # fragment). curl.exe (no -L) never follows, so we observe the raw 303.
    Write-Host "[EP1] POST http://127.0.0.1:$GPort/accounts/epic/start through G (passthrough, keyless) -> {authorize_url}"
    $estart = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/accounts/epic/start")
    Write-Host "    -> $($estart.Body)"
    $estate = $null
    if ($estart.Body -match 'state=([^&"]+)') { $estate = $Matches[1] }
    if ($estate) {
        Pass "epic start relayed through G -> authorize_url with state=$($estate.Substring(0, [Math]::Min(8, $estate.Length)))..."
    } else {
        Fail "epic start expected authorize_url with a state param, got $($estart.Body)"; throw 'epic start failed'
    }

    Write-Host "[EP2] GET /accounts/epic/callback?code=x&state=<state> through G (no redirect follow) -> 303 relayed verbatim"
    # curl.exe never follows redirects without -L (Invoke-WebRequest -MaximumRedirection 0
    # THROWS on a 3xx even with -SkipHttpErrorCheck, so it can't assert this).
    $eraw = & curl.exe -s -D - -o NUL -w "%{http_code}" "http://127.0.0.1:$GPort/accounts/epic/callback?code=x&state=$estate" 2>$null
    $elines = ($eraw -join "`n") -split "`n"
    $ecode = [int]$elines[-1].Trim()
    $eloc = ($elines | Where-Object { $_ -match '^(?i)location:' } | Select-Object -First 1) -replace '^(?i)location:\s*', ''
    if ($eloc) { $eloc = $eloc.Trim() }
    Write-Host "    -> HTTP $ecode  Location=$eloc"
    if ($ecode -eq 303 -and $eloc -eq '/?epic=error') {
        Pass "epic-oauth-redirect-through-gateway: G relays D's 303 verbatim (Location: $eloc) -- proxy does not follow"
    } else {
        Fail "epic callback expected 303 (axum Redirect::to) with Location /?epic=error, got HTTP $ecode Location=$eloc"; throw 'epic redirect failed'
    }

    Write-Host ''
    Write-Host '================ API KEY POLICY (apikeys-svc via G) ================'
    # K1-K4: the policy check runs BEFORE session auth on both planes (Decision 5 of
    # the api-key-policy plan), so these use an AuthNone op (GET /leaderboard, POST
    # /match/report) to isolate the key check from bearer auth.

    Write-Host '[K1] GET /leaderboard through G with NO X-Api-Key -> 401 (missing api key)'
    $k1 = Invoke-Curl @("http://127.0.0.1:$GPort/leaderboard")
    Write-Host "    -> HTTP $($k1.Code)"
    if ($k1.Code -eq '401') { Pass 'no api key -> 401 (missing key)' } else { Fail "no api key expected 401, got $($k1.Code)" }

    Write-Host '[K2] GET /leaderboard through G with a BOGUS X-Api-Key -> 401 (invalid api key)'
    $k2 = Invoke-Curl @("http://127.0.0.1:$GPort/leaderboard", '-H', 'X-Api-Key: totally-bogus-key')
    Write-Host "    -> HTTP $($k2.Code)"
    if ($k2.Code -eq '401') { Pass 'bogus api key -> 401 (invalid key)' } else { Fail "bogus api key expected 401, got $($k2.Code)" }

    Write-Host '[K3] POST /match/report through G with dev-key-client (player-facing policy, NO match.report) -> 403'
    $k3 = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/match/report",
        '-H', 'X-Api-Key: dev-key-client', '-H', 'Content-Type: application/json',
        '-d', "{`"ReportId`":`"k3-$RunSuffix`",`"Winner`":`"k3-winner`",`"Loser`":`"k3-loser`"}")
    Write-Host "    -> HTTP $($k3.Code)"
    if ($k3.Code -eq '403') { Pass 'dev-key-client on match.report -> 403 (policy forbids this operation)' } else { Fail "dev-key-client on match.report expected 403, got $($k3.Code)" }

    Write-Host '[K4] POST /match/report through G with dev-key-server (full policy) -> 202'
    $k4 = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/match/report",
        '-H', 'X-Api-Key: dev-key-server', '-H', 'Content-Type: application/json',
        '-d', "{`"ReportId`":`"k4-$RunSuffix`",`"Winner`":`"k4-winner`",`"Loser`":`"k4-loser`"}")
    Write-Host "    -> HTTP $($k4.Code)"
    if ($k4.Code -eq '202') { Pass "dev-key-server (full) on match.report -> 202 (op's real success code)" } else { Fail "dev-key-server on match.report expected 202, got $($k4.Code)" }
    Invoke-Sql "DELETE FROM leaderboard.scores WHERE player IN ('k3-winner','k3-loser','k4-winner','k4-loser');" | Out-Null

    # [K5] key-verifier load-shed is 503, never a mislabeled 401 (and never a crash).
    # The verifier bounds concurrent uncached lookups with a global 64-permit semaphore
    # (+ a flight-lock table); when exhausted it sheds with 503 Unavailable ("no verdict
    # was reached"), NOT 401 ("your key is invalid"). A DEFINITE 503 is not reliably
    # reproducible through the front door from this script: gateway-svc's always-on
    # rate limiter (burst 40, asserted in [RL1]) admits fewer concurrent requests from
    # one client IP than the semaphore has permits (64), so full saturation cannot be
    # forced here. We assert the fix's GUARANTEED weaker observable instead: a parallel
    # burst of DISTINCT uncached keys (each takes a real permit-guarded lookup over the
    # edge to apikeys-svc) yields ONLY 401 (invalid key), 503 (shed) or 429 (rate
    # limiter, orthogonal) -- never a 200 (bogus key admitted) and never another 5xx
    # (crash). Any 503s observed are reported as best-effort shed evidence, not asserted.
    Start-Sleep -Seconds 2 # let the [RL] token bucket refill so the burst isn't eaten by 429s
    Write-Host '[K5] 30 PARALLEL GET /leaderboard with DISTINCT bogus keys -> every response 401/503/429, >=1 401, nothing else'
    $k5Args = @()
    for ($i = 1; $i -le 30; $i++) {
        if ($k5Args.Count -gt 0) { $k5Args += '--next' }
        $k5Args += @('-s', '-o', 'NUL', '-w', "%{http_code}`n", '-H', "X-Api-Key: k5-$RunSuffix-$i", "http://127.0.0.1:$GPort/leaderboard")
    }
    $k5Codes = @(& curl.exe -Z --parallel-max 30 @k5Args 2>$null)
    $k5Total = @($k5Codes | Where-Object { $_ -match '\d' }).Count
    $k5c401 = @($k5Codes | Where-Object { $_ -match '401' }).Count
    $k5c503 = @($k5Codes | Where-Object { $_ -match '503' }).Count
    $k5c429 = @($k5Codes | Where-Object { $_ -match '429' }).Count
    $k5Other = $k5Total - $k5c401 - $k5c503 - $k5c429
    Write-Host "    -> $k5Total responses: ${k5c401}x401, ${k5c503}x503 (shed, best-effort), ${k5c429}x429, ${k5Other} other"
    if ($k5Total -eq 30 -and $k5Other -eq 0 -and $k5c401 -ge 1) {
        Pass "distinct-key burst: all 30 responses 401/503/429 -- no bogus key admitted, no 5xx crash ($k5c503 load-shed 503s observed)"
    } else {
        Fail "distinct-key burst expected 30 responses all 401/503/429 with >=1 401, got total=$k5Total 401=$k5c401 503=$k5c503 429=$k5c429 other=$k5Other"
    }

    # [K5b] the burst RELEASED its permits/flight locks (shed is transient, never sticky):
    # one more fresh distinct key must reach a definitive verdict (401 invalid), not 503.
    Write-Host '[K5b] fresh distinct bogus key after the burst -> 401 (verifier recovered, permits released)'
    $k5b = Invoke-Curl @("http://127.0.0.1:$GPort/leaderboard", '-H', "X-Api-Key: k5b-$RunSuffix")
    Write-Host "    -> HTTP $($k5b.Code)"
    if ($k5b.Code -eq '401') {
        Pass 'post-burst distinct key -> 401 (semaphore permits released after the burst)'
    } else {
        Fail "post-burst distinct key expected 401, got $($k5b.Code)"
    }

    Write-Host ''
    Write-Host '================ SPLIT PROOF ================'

    # --- 1. CREATE through G (front-door HTTP op -> Remote -> characters-svc) ---
    # characters-svc no longer hosts a FrontDoor, so create is fronted by gateway-svc
    # (:8082) which dispatches characters.create Remote over the mTLS edge to A.
    Write-Host "[1] POST http://127.0.0.1:$GPort/characters (through G -> A, Bearer `$Token)"
    $c = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/characters",
        '-H', 'X-Api-Key: dev-key-client', '-H', "Authorization: Bearer $Token", '-H', 'Content-Type: application/json',
        '-d', '{"name":"Aria","class":"mage"}')
    Write-Host "    -> HTTP $($c.Code)  $($c.Body)"
    $cid = $null
    if ($c.Body -match '"id":"([^"]+)"') { $cid = $Matches[1] }
    if ($c.Code -eq '201' -and $cid) { Pass "create -> 201, id=$cid" } else { Fail 'create expected 201 with id' }

    # --- 2. ASYNC event A->B + SYNC authz B->A over QUIC ---
    Write-Host "[2] poll GET http://127.0.0.1:$GPort/inventory/character/$cid until starter appears (through G -> B)"
    $starterOk = $false
    for ($i = 1; $i -le 30; $i++) {
        $r = Invoke-Curl @("http://127.0.0.1:$GPort/inventory/character/$cid", '-H', 'X-Api-Key: dev-key-client', '-H', "Authorization: Bearer $Token")
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
    $n = Invoke-Curl @("http://127.0.0.1:$GPort/inventory/character/$cid", '-H', 'X-Api-Key: dev-key-client', '-H', "Authorization: Bearer $OtherToken")
    Write-Host "    -> HTTP $($n.Code)  $($n.Body)"
    if ($n.Code -eq '403' -or $n.Code -eq '404') { Pass "other player -> $($n.Code) (owner_of over QUIC gates)" } else { Fail "negative authz expected 403/404, got $($n.Code)" }

    # --- 4. DELETE through G -> A ---
    Write-Host "[4] DELETE http://127.0.0.1:$GPort/characters/$cid (through G -> A, Bearer `$Token)"
    $d = Invoke-Curl @('-X', 'DELETE', "http://127.0.0.1:$GPort/characters/$cid", '-H', 'X-Api-Key: dev-key-client', '-H', "Authorization: Bearer $Token")
    Write-Host "    -> HTTP $($d.Code)"
    if ($d.Code -eq '204') { Pass 'delete -> 204' } else { Fail "delete expected 204, got $($d.Code)" }

    # --- 5. INTEGRITY via event, not FK: holdings wiped in B (DB is the real proof) ---
    Write-Host "[5] poll B until the character's holdings are WIPED (character.deleted A->B)"
    $wiped = $false
    for ($i = 1; $i -le 30; $i++) {
        $count = (Invoke-Sql "SELECT count(*) FROM inventory.holdings WHERE owner_type='character' AND owner_id='$cid';").Trim()
        Write-Host "    attempt $i -> inventory.holdings rows for $cid = $count"
        if ($count -eq '0') { Pass 'holdings row wiped in B (integrity via character.deleted event, no FK cascade)'; $wiped = $true; break }
        Start-Sleep -Milliseconds 500
    }
    if (-not $wiped) { Fail 'holdings never wiped in B (wipe on_tx handler did not run)' }

    # [5t] the wipe handler also plants the tombstone (inventory.wiped_characters) in the
    # SAME delivery tx -- the guard that keeps a reordered/late character.created grant
    # from resurrecting holdings for this dead character.
    $tomb = (Invoke-Sql "SELECT count(*) FROM inventory.wiped_characters WHERE character_id='$cid';").Trim()
    Write-Host "[5t] inventory.wiped_characters rows for $cid = $tomb"
    if ($tomb -eq '1') {
        Pass 'wipe planted the tombstone (late character.created can no longer resurrect holdings)'
    } else {
        Fail "expected 1 tombstone row in inventory.wiped_characters for $cid, got $tomb"
    }

    # [5b] the same character is gone via owner_of over QUIC too (a second, independent
    # signal alongside the DB wipe check above).
    Write-Host "[5b] post-delete GET /inventory/character/$cid through G (Bearer `$Token) -> 404"
    $w2 = Invoke-Curl @("http://127.0.0.1:$GPort/inventory/character/$cid", '-H', 'X-Api-Key: dev-key-client', '-H', "Authorization: Bearer $Token")
    Write-Host "    -> HTTP $($w2.Code)  $($w2.Body)"
    if ($w2.Code -eq '404') { Pass 'post-delete GET -> 404 (character gone via owner_of over QUIC)' } else { Fail "post-delete GET expected 404, got $($w2.Code)" }

    Write-Host ''
    Write-Host "========= CONFIG LIVE-RELOAD (config-svc :$CPort -> inventory-svc) ========="
    # Prove the Step-5 snapshot-backed remote config reader live-reloads across processes:
    # change inventory/starter_item at RUNTIME on C's DB, and a NEWLY created character
    # must be granted the NEW starter in B -- config.changed flowed C's append -> the
    # shared log -> B's pull worker -> B's CachedConfig (snapshot refresh) + inventory
    # starter spec, no restart.
    # [C1] baseline: B booted with the default starter (no config row) -> starter_sword.
    Write-Host '[C1] baseline: create a character through G -> starter should be the DEFAULT starter_sword'
    $bc = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/characters",
        '-H', 'X-Api-Key: dev-key-client', '-H', "Authorization: Bearer $Token", '-H', 'Content-Type: application/json',
        '-d', '{"name":"Baseline","class":"mage"}')
    $bcid = $null
    if ($bc.Body -match '"id":"([^"]+)"') { $bcid = $Matches[1] }
    $baseOk = $false
    for ($i = 1; $i -le 30; $i++) {
        $r = Invoke-Curl @("http://127.0.0.1:$GPort/inventory/character/$bcid", '-H', 'X-Api-Key: dev-key-client', '-H', "Authorization: Bearer $Token")
        if ($r.Body -match 'starter_sword') { $baseOk = $true; break }
        if ($r.Body -match 'health_potion') { break }
        Start-Sleep -Milliseconds 500
    }
    if ($baseOk) { Pass 'baseline character granted starter_sword (B booted on the default via CachedConfig)' } else { Fail "baseline starter_sword not granted (bcid=$bcid)" }

    # [C2] runtime change on C's DB: the write trigger bumps the revision, pg_notifies
    # config_changed (B's invalidation plane refreshes CachedConfig), and appends
    # config.changed durably (B's pull worker delivers it -> inventory reloads its
    # starter spec).
    Write-Host '[C2] set config inventory/starter_item=health_potion (via psql on C shared DB)'
    Invoke-Sql "INSERT INTO config.settings (namespace,key,value) VALUES ('inventory','starter_item','health_potion') ON CONFLICT (namespace,key) DO UPDATE SET value=excluded.value;" | Out-Null

    # [C3] a NEWLY created character must now be granted the NEW starter. The spec is
    # materialized at grant time, so retry with fresh characters until it takes.
    Write-Host '[C3] create fresh characters until one is granted health_potion (live reload C->B)'
    $reloadOk = $false
    for ($i = 1; $i -le 30; $i++) {
        $nc = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/characters",
            '-H', 'X-Api-Key: dev-key-client', '-H', "Authorization: Bearer $Token", '-H', 'Content-Type: application/json',
            '-d', '{"name":"Reloaded","class":"mage"}')
        $ncid = $null
        if ($nc.Body -match '"id":"([^"]+)"') { $ncid = $Matches[1] }
        $r = $null
        for ($j = 1; $j -le 10; $j++) {
            $r = Invoke-Curl @("http://127.0.0.1:$GPort/inventory/character/$ncid", '-H', 'X-Api-Key: dev-key-client', '-H', "Authorization: Bearer $Token")
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

    # [C4] a >8KB config value must COMMIT (the pg_notify large-value fix). pg_notify
    # hard-caps its payload at 8000 bytes; before the fix the write trigger put the full
    # value into the NOTIFY payload, so ANY large config value ABORTED the writing tx.
    # The trigger now NOTIFYs value-less (revision only) while the durable config.changed
    # event still carries the full value. Assert: the INSERT commits (Invoke-Sql throws
    # on a psql error, so reaching the checks below IS the no-abort proof), the revision
    # bumped by exactly one, the 9000-char value round-trips at full length, and the
    # durable event appended in the same tx carries the full value.
    $c4Key = "c4-large-$RunSuffix"
    $c4RevBefore = [long](("" + (Invoke-Sql "SELECT revision FROM config.revision;")).Trim())
    Write-Host "[C4] write a >8KB config value (proof/$c4Key, 9000 chars) -- must NOT abort (revision $c4RevBefore -> +1)"
    Invoke-Sql "INSERT INTO config.settings (namespace,key,value) VALUES ('proof','$c4Key',repeat('x',9000));" | Out-Null
    $c4RevAfter = [long](("" + (Invoke-Sql "SELECT revision FROM config.revision;")).Trim())
    $c4Len = ("" + (Invoke-Sql "SELECT length(value) FROM config.settings WHERE namespace='proof' AND key='$c4Key';")).Trim()
    $c4Evt = ("" + (Invoke-Sql "SELECT count(*) FROM asyncevents.events WHERE topic='config.changed' AND payload->>'key'='$c4Key' AND length(payload->>'value')=9000;")).Trim()
    Write-Host "    -> revision $c4RevBefore -> $c4RevAfter, stored length=$c4Len, durable config.changed rows (full value)=$c4Evt"
    if ($c4RevAfter -eq ($c4RevBefore + 1) -and $c4Len -eq '9000' -and $c4Evt -eq '1') {
        Pass 'large config value committed: revision +1, 9000-char value round-trips, durable event carries the full value (NOTIFY stayed value-less)'
    } else {
        Fail "large config write expected rev+1 / len=9000 / 1 durable event, got rev $c4RevBefore->$c4RevAfter len=$c4Len events=$c4Evt"
    }

    # [C4b] the reload pipeline still works WITH the 9KB row in the store: the
    # reset-to-default DELETE below bumps the revision and NOTIFYs, and B's refresh
    # re-reads the WHOLE snapshot -- which now contains the large value -- so a fresh
    # character reverting to starter_sword proves CachedConfig + the one-statement
    # snapshot query swallow a large value cross-process. (This replaces the previously
    # silent rerun-cleanliness reset with an asserted one.)
    Write-Host '[C4b] reset starter_item (DELETE) -> fresh characters revert to starter_sword (reload with the 9KB row present)'
    Invoke-Sql "DELETE FROM config.settings WHERE namespace='inventory' AND key='starter_item';" | Out-Null
    $revertOk = $false
    for ($i = 1; $i -le 30; $i++) {
        $vc = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/characters",
            '-H', 'X-Api-Key: dev-key-client', '-H', "Authorization: Bearer $Token", '-H', 'Content-Type: application/json',
            '-d', '{"name":"Reverted","class":"mage"}')
        $vcid = $null
        if ($vc.Body -match '"id":"([^"]+)"') { $vcid = $Matches[1] }
        $r = $null
        for ($j = 1; $j -le 10; $j++) {
            $r = Invoke-Curl @("http://127.0.0.1:$GPort/inventory/character/$vcid", '-H', 'X-Api-Key: dev-key-client', '-H', "Authorization: Bearer $Token")
            if ($r.Body -match 'starter_sword|health_potion') { break }
            Start-Sleep -Milliseconds 300
        }
        if ($r.Body -match 'starter_sword') {
            Write-Host "    attempt $i -> char $vcid granted starter_sword"
            $revertOk = $true; break
        }
        Start-Sleep -Milliseconds 500
    }
    if ($revertOk) {
        Pass 'starter reverted to starter_sword after the delete (snapshot refresh works with a >8KB value in the store)'
    } else {
        Fail 'starter never reverted to starter_sword after the reset (reload broken alongside the large value?)'
    }
    # Drop the large row so reruns start clean (each run keys it by RunSuffix anyway).
    Invoke-Sql "DELETE FROM config.settings WHERE namespace='proof' AND key='$c4Key';" | Out-Null

    Write-Host ''
    Write-Host '========= ADMIN PORTAL (gateway-svc passthrough -> admin-svc -> providers over edge) ========='
    # The admin fan-out end-to-end: a browser hits gateway-svc :8082 /admin, reverse-
    # proxied (Step 7 passthrough) to admin-svc :8085, which fetches each provider's
    # admin page over the mTLS QUIC edge. The characters page must render a character
    # CREATED on characters-svc -- proving the data crossed TWO process hops.
    $aproof = "AdminProof-$RunSuffix"
    Write-Host "[AD0] create a character named $aproof through G -> A (for the admin table assertion)"
    $acr = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/characters",
        '-H', 'X-Api-Key: dev-key-client', '-H', "Authorization: Bearer $Token", '-H', 'Content-Type: application/json',
        '-d', "{`"name`":`"$aproof`",`"class`":`"ranger`"}")
    if ($acr.Body -match '"id":"([^"]*)"') { Pass "admin-proof character created (id=$($Matches[1]))" } else { Fail 'admin-proof character not created' }

    $AdminJar = Join-Path $RunDir 'admin-proof.jar'
    Remove-Item $AdminJar -ErrorAction SilentlyContinue

    Write-Host "[AD1] GET http://127.0.0.1:$GPort/admin WITHOUT a session -> 303 Location /admin/login"
    # -o NUL, not -o $null: pwsh drops a $null native arg entirely, so -o would swallow
    # -w and the http_code would come back empty (observed under pwsh 7.6).
    $an = "" + (& curl.exe -s -o NUL -w '%{http_code} %{redirect_url}' "http://127.0.0.1:$GPort/admin")
    $anParts = $an.Trim() -split ' ', 2
    $anCode = $anParts[0]; $anLoc = if ($anParts.Count -gt 1) { $anParts[1] } else { '' }
    Write-Host "    -> HTTP $anCode  Location=$anLoc"
    if ($anCode -eq '303' -and $anLoc -match '/admin/login$') {
        Pass 'unauthenticated /admin -> 303 to /admin/login through the passthrough (session gate live on admin-svc)'
    } else {
        Fail "unauthenticated /admin expected 303 -> /admin/login, got $anCode $anLoc"
    }

    # [AD2] asymmetric lockout: prooflock fails 6x. Each answer is the SAME generic 401
    # body (no username/lock oracle). The user row locks at 5 fails; the ip row (shared
    # subject, threshold 20) increments but does NOT lock. Clear both subjects first so the
    # assertion is deterministic across reruns.
    Invoke-Sql "DELETE FROM admin.login_attempts WHERE subject = 'user:$ProofLockUser' OR subject LIKE 'ip:%';" | Out-Null
    Write-Host "[AD2] POST /admin/login as $ProofLockUser x6 wrong password -> each 401 identical body; user locks, ip does not"
    $ad2Ok = $true
    $ad2First = $null
    for ($i = 1; $i -le 6; $i++) {
        $l6 = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/admin/login", '-d', "username=$ProofLockUser&password=wrong-$ProofLockUser")
        if ($l6.Code -ne '401') { $ad2Ok = $false; Write-Host "    attempt $i -> HTTP $($l6.Code) (expected 401)" }
        if ($null -eq $ad2First) { $ad2First = $l6.Body } elseif ($l6.Body -ne $ad2First) { $ad2Ok = $false; Write-Host "    attempt $i -> body differs from the first 401 (oracle leak)" }
    }
    $uFails = ("" + (Invoke-Sql "SELECT fails FROM admin.login_attempts WHERE subject='user:$ProofLockUser';")).Trim()
    $uLocked = ("" + (Invoke-Sql "SELECT locked_until IS NOT NULL FROM admin.login_attempts WHERE subject='user:$ProofLockUser';")).Trim()
    $ipLocked = ("" + (Invoke-Sql "SELECT count(*) FROM admin.login_attempts WHERE subject LIKE 'ip:%' AND locked_until IS NOT NULL;")).Trim()
    Write-Host "    -> user:$ProofLockUser fails=$uFails locked=$uLocked ; ip-rows-locked=$ipLocked"
    if ($ad2Ok -and [int]$uFails -ge 5 -and $uLocked -eq 't' -and $ipLocked -eq '0') {
        Pass "6 wrong logins -> identical 401 bodies; user:$ProofLockUser locked (fails>=5), ip subject NOT locked (asymmetric thresholds)"
    } else {
        Fail "lockout expected identical 401 x6 + user locked (fails>=5) + ip not locked, got ok=$ad2Ok fails=$uFails locked=$uLocked ip-locked=$ipLocked"
    }

    # [AD2b] Race twelve requests within the production burst. The DB advisory
    # locks must serialize the threshold and emit login-locked exactly once.
    $ad2bIp = '198.51.100.42'
    Invoke-Sql "DELETE FROM admin.login_attempts WHERE subject IN ('user:$ProofLockUser','ip:$ad2bIp'); DELETE FROM asyncevents.events WHERE topic='admin.action' AND payload->>'actor'='$ProofLockUser' AND payload->>'action'='login-locked';" | Out-Null
    Write-Host '[AD2b] 12 parallel wrong logins -> exact threshold and one lock event'
    $jobs = 1..12 | ForEach-Object {
        $i = $_; $url = "http://127.0.0.1:$GPort/admin/login"; $user = $ProofLockUser; $ip = $ad2bIp
        Start-Job -ScriptBlock {
            param($url, $user, $ip, $i)
            & curl.exe -s -o NUL -w '%{http_code}' -X POST $url -H "X-Forwarded-For: $ip" -d "username=$user&password=wrong-$i"
        } -ArgumentList $url, $user, $ip, $i
    }
    $ad2bCodes = $jobs | Wait-Job | Receive-Job
    $jobs | Remove-Job -Force
    $ad2bFails = ("" + (Invoke-Sql "SELECT fails FROM admin.login_attempts WHERE subject='user:$ProofLockUser';")).Trim()
    $ad2bLocked = ("" + (Invoke-Sql "SELECT locked_until > now() FROM admin.login_attempts WHERE subject='user:$ProofLockUser';")).Trim()
    $ad2bEvents = ("" + (Invoke-Sql "SELECT count(*) FROM asyncevents.events WHERE topic='admin.action' AND payload->>'actor'='$ProofLockUser' AND payload->>'action'='login-locked';")).Trim()
    if ($ad2bFails -eq '5' -and $ad2bLocked -eq 't' -and $ad2bEvents -eq '1' -and @($ad2bCodes).Count -eq 12) {
        Pass 'parallel lockout serialized: fails=5, active lock, one login-locked event'
    } else {
        Fail "parallel lockout expected fails=5/locked/one event, got $ad2bFails/$ad2bLocked/$ad2bEvents"
    }

    # [AD2c] A distinct trusted-proxy IP exhausts only the login admission burst.
    $ad2cIp = '198.51.100.43'
    Write-Host '[AD2c] login admission burst -> exact 429 + Retry-After: 1'
    # Start-Job spins up 21 PowerShell runtimes serially and can take longer than
    # the limiter's one-second refill window. Launch curl processes directly so the
    # production burst is actually concurrent and the assertion is deterministic.
    $ad2cDir = Join-Path $RunDir 'ad2c-ps'
    Remove-Item -LiteralPath $ad2cDir -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Path $ad2cDir | Out-Null
    $ad2cProcs = 1..40 | ForEach-Object {
        $i = $_
        $headers = Join-Path $ad2cDir "$i.headers"
        $code = Join-Path $ad2cDir "$i.code"
        $err = Join-Path $ad2cDir "$i.err"
        Start-Process -FilePath 'curl.exe' -ArgumentList @(
            '-s', '-D', $headers, '-o', 'NUL', '-w', '%{http_code}', '-X', 'POST',
            "http://127.0.0.1:$GPort/admin/login", '-H', "X-Forwarded-For:$ad2cIp",
            '-d', "username=ghost-ad2c-$i&password=wrong"
        ) -PassThru -NoNewWindow -RedirectStandardOutput $code -RedirectStandardError $err
    }
    $ad2cProcs | ForEach-Object { $_.WaitForExit() }
    $ad2cResults = @(1..40 | ForEach-Object {
        $headers = Join-Path $ad2cDir "$_.headers"
        $code = Join-Path $ad2cDir "$_.code"
        [pscustomobject]@{
            Code = (Get-Content -LiteralPath $code -Raw).Trim()
            Retry = [bool](Select-String -Path $headers -Pattern '^Retry-After: 1' -CaseSensitive:$false -Quiet)
        }
    })
    $limited = @($ad2cResults | Where-Object Code -eq '429')
    if ($limited.Count -ge 1 -and @($limited | Where-Object { -not $_.Retry }).Count -eq 0) {
        Pass 'admin login limiter returns 429 with Retry-After: 1'
    } else {
        Fail "admin login limiter expected every 429 to carry Retry-After: 1, got $($limited.Count) limited responses"
    }

    # [AD3] happy-path session login: proofadmin logs in through G's passthrough, gets a
    # 303 + admin_session cookie in the jar, then the jar authorizes the cross-process
    # admin pages -- /admin/characters (G -> E -> A over QUIC, renders the AD0 character)
    # and /admin/api-keys (G -> E -> L over QUIC, renders the seeded dev-client key; the old
    # [K5] rides this session).
    Write-Host "[AD3] POST /admin/login as $ProofAdminUser (curl -c jar) -> 303 + admin_session cookie"
    $ad3Code = "" + (& curl.exe -s -c $AdminJar -o NUL -w '%{http_code}' -X POST "http://127.0.0.1:$GPort/admin/login" -d "username=$ProofAdminUser&password=$ProofAdminPass")
    $ad3Cookie = (Test-Path $AdminJar) -and (Select-String -Path $AdminJar -Pattern 'admin_session' -Quiet)
    Write-Host "    -> HTTP $($ad3Code.Trim())  (admin_session cookie: $ad3Cookie)"
    if ($ad3Code.Trim() -eq '303' -and $ad3Cookie) {
        Pass "$ProofAdminUser login -> 303 + admin_session cookie minted (session auth live)"
    } else {
        Fail "$ProofAdminUser login expected 303 + admin_session cookie, got $($ad3Code.Trim())"
    }

    Write-Host "[AD3a] GET /admin/characters WITH the session jar -> 200 + contains $aproof"
    $ad = Invoke-Curl @('-b', $AdminJar, "http://127.0.0.1:$GPort/admin/characters")
    Write-Host "    -> HTTP $($ad.Code)  (body $($ad.Body.Length) chars)"
    if ($ad.Code -eq '200' -and $ad.Body -match [regex]::Escape($aproof)) {
        Pass "admin /admin/characters renders $aproof cross-process (session jar; G passthrough -> E -> A admin.adminData over QUIC)"
    } else {
        Fail "admin characters page expected 200 containing $aproof, got $($ad.Code)"
    }

    Write-Host "[AD3b] GET /admin/api-keys WITH the session jar -> 200 + contains dev-client (old K5 rides the session)"
    # The apikeys admin fan-out end-to-end: G's HTTP passthrough -> admin-svc :8085, then
    # admin-svc's admin.adminData -> apikeys-svc :$LPort over the mTLS QUIC edge. The page
    # must render the seeded `dev-client` key row (APIKEYS_DEV_SEED=1 on L), proving the
    # remote apikeys admin item composed across TWO process hops. (The slug is `api-keys`:
    # the admin portal derives it from the "API Keys" LABEL, like "Audit Log" -> audit-log.)
    $k5 = Invoke-Curl @('-b', $AdminJar, "http://127.0.0.1:$GPort/admin/api-keys")
    Write-Host "    -> HTTP $($k5.Code)  (body $($k5.Body.Length) chars)"
    if ($k5.Code -eq '200' -and $k5.Body -match 'dev-client') {
        Pass 'admin /admin/apikeys renders dev-client cross-process (session jar; G passthrough -> E -> L admin.adminData over QUIC)'
    } else {
        Fail "admin apikeys page expected 200 containing dev-client, got $($k5.Code)"
    }

    # [AD4] CSRF: a POST with a valid session but NO _csrf field is rejected 403 -- the
    # CSRF check runs BEFORE the local/remote editability decision, so the remote apikeys
    # item answers 403 (not the 405 a remote form would otherwise get).
    Write-Host "[AD4] POST /admin/api-keys WITH the session jar but NO _csrf -> 403"
    $ad4 = Invoke-Curl @('-X', 'POST', '-b', $AdminJar, "http://127.0.0.1:$GPort/admin/api-keys", '-d', 'dummy=1')
    Write-Host "    -> HTTP $($ad4.Code)"
    if ($ad4.Code -eq '403') {
        Pass 'session POST without _csrf -> 403 (CSRF gate precedes the editability decision)'
    } else {
        Fail "CSRF-less POST expected 403, got $($ad4.Code)"
    }

    # [AD5] the durable admin.action trail: the login-succeeded ([AD3]) and login-locked
    # ([AD2]) events are on the shared log, and audit-svc's 7th subscription records them.
    Write-Host '[AD5] admin.action durable trail: asyncevents.events >= 2 AND audit.log has admin.action rows'
    $ad5Events = ("" + (Invoke-Sql "SELECT count(*) FROM asyncevents.events WHERE topic='admin.action';")).Trim()
    Write-Host "    -> asyncevents.events admin.action rows = $ad5Events"
    $ad5Ok = $false
    for ($i = 1; $i -le 30; $i++) {
        $ad5Audit = ("" + (Invoke-Sql "SELECT count(*) FROM audit.log WHERE topic='admin.action';")).Trim()
        Write-Host "    attempt $i -> audit.log admin.action rows = $ad5Audit"
        if ([int]$ad5Events -ge 2 -and [int]$ad5Audit -ge 2) {
            Pass 'admin.action emitted (>=2 on the shared log) AND recorded by audit-svc (durable E->F, 7th subscription)'
            $ad5Ok = $true; break
        }
        Start-Sleep -Milliseconds 500
    }
    if (-not $ad5Ok) { Fail "admin.action never reached >=2 events + audit rows (asyncevents=$ad5Events audit=$ad5Audit)" }

    Write-Host ''
    Write-Host "========= AUDIT LEDGER (durable events -> audit-svc :$FPort) ========="
    # The append-only ledger end-to-end across processes: each producer appends its
    # durable event to the shared log, audit-svc's pull worker delivers it, and audit's
    # on_tx_raw records it in schema `audit` (exactly-once, on the delivery tx). Assert the ROWS on the shared DB (the
    # definitive proof the cross-process record handler ran): the character CREATED in [1]
    # + DELETED in [4], and the player REGISTERED in [A1]. Then the "Audit Log" admin page
    # renders through the gateway passthrough (G -> E -> F over QUIC).
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

    Write-Host "[AU3] GET http://127.0.0.1:$GPort/admin/audit-log WITH the session jar -> 200 + a logged topic"
    $aud = Invoke-Curl @('-b', $AdminJar, "http://127.0.0.1:$GPort/admin/audit-log")
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
    # still-due, bumps last_fired + emit_tx's scheduler.fired IN ONE TX onto the shared
    # log. Assert on the shared DB: (i) a scheduler.fired event in asyncevents.events for
    # proof-tick (advisory-lock fire), and (ii) audit-svc's pull cursor (subscription
    # audit.prune-on-scheduler.v1) advanced PAST that event's position (H -> F delivery).
    Write-Host "[SC0] seed a 2s schedule 'proof-tick' on the shared DB (immediately due, epoch last_fired)"
    # Drop stale proof-tick events from earlier runs so SC1 proves THIS run's fire.
    Invoke-Sql "DELETE FROM asyncevents.events WHERE topic='scheduler.fired' AND payload->>'name'='proof-tick';" | Out-Null
    Invoke-Sql "INSERT INTO scheduler.schedules (name, interval_seconds, last_fired) VALUES ('proof-tick', 2, to_timestamp(0)) ON CONFLICT (name) DO UPDATE SET interval_seconds=2, last_fired=to_timestamp(0);" | Out-Null
    Write-Host "[SC1] poll the shared log for scheduler.fired{proof-tick} AND audit's pull cursor past it"
    $scOk = $false
    for ($i = 1; $i -le 30; $i++) {
        $scFired = ("" + (Invoke-Sql "SELECT count(*) FROM asyncevents.events WHERE topic='scheduler.fired' AND payload->>'name'='proof-tick';")).Trim()
        $scConsumed = ("" + (Invoke-Sql "SELECT count(*) FROM asyncevents.subscriptions s, asyncevents.events e WHERE s.subscription_id='audit.prune-on-scheduler.v1' AND e.topic='scheduler.fired' AND e.payload->>'name'='proof-tick' AND (s.cursor_generation, s.cursor_xid, s.cursor_tie) >= (e.generation, e.producer_xid, e.tie_breaker);")).Trim()
        Write-Host "    attempt $i -> fired=$scFired consumed=$scConsumed"
        if ([int]($scFired -as [int]) -ge 1 -and [int]($scConsumed -as [int]) -ge 1) {
            Pass "scheduler-svc fired proof-tick durably (advisory-lock + stillDue re-check) AND audit's cursor advanced past it (H's event pulled by F)"
            $scOk = $true; break
        }
        Start-Sleep -Milliseconds 500
    }
    if (-not $scOk) { Fail 'scheduler.fired{proof-tick} never produced+consumed (scheduler H -> shared log -> audit F)' }
    Invoke-Sql "DELETE FROM scheduler.schedules WHERE name='proof-tick';" | Out-Null

    Write-Host ''
    Write-Host "========= SESSION PRUNE (scheduler-svc :$HPort -> accounts-svc :$DPort) ========="
    # The durable session-prune reaction end-to-end across processes: accounts-svc (D)
    # subscribes accounts.prune-on-scheduler.v1 and, on scheduler.fired{accounts-sessions-prune},
    # DELETEs expired rows from accounts.sessions in the delivery tx. We plant an already-expired
    # session on the shared DB, force the SEEDED daily schedule to fire NOW (reset last_fired to
    # the epoch -> immediately due, like proof-tick above; a reused dev DB has it advanced, so
    # the reset makes the fire deterministic), then poll until D's handler has removed the row.
    # NOT via a synthetic asyncevents.append_event: forging an event the scheduler solely
    # produces would violate publisher-owns-the-event (and feed audit's raw sink a fake row).
    Write-Host "[SP0] plant a throwaway player + an EXPIRED session on the shared DB (FK needs a real player)"
    # psql prints the INSERT command tag after the RETURNING row even with -t -A;
    # only the first line is the uuid.
    $spPid = ("" + @(Invoke-Sql "INSERT INTO accounts.players (display_name) VALUES ('prune-proof-$RunSuffix') RETURNING id::text;")[0]).Trim()
    if (-not $spPid) { Fail 'could not insert throwaway player for the session-prune proof'; throw 'session-prune bootstrap failed' }
    $spToken = "prune-proof-$RunSuffix"
    Invoke-Sql "INSERT INTO accounts.sessions (token, player_id, expires_at) VALUES ('$spToken', '$spPid'::uuid, now() - interval '1 day');" | Out-Null
    Write-Host "[SP1] force the seeded 'accounts-sessions-prune' schedule due NOW (reset last_fired to epoch)"
    Invoke-Sql "UPDATE scheduler.schedules SET last_fired = to_timestamp(0) WHERE name = 'accounts-sessions-prune';" | Out-Null
    Write-Host "[SP2] poll accounts.sessions until D's prune handler removes the expired row"
    $spOk = $false
    for ($i = 1; $i -le 30; $i++) {
        $spLeft = ("" + (Invoke-Sql "SELECT count(*) FROM accounts.sessions WHERE token='$spToken';")).Trim()
        Write-Host "    attempt $i -> expired_rows_left=$spLeft"
        if ($spLeft -eq '0') {
            Pass "scheduler-svc fired accounts-sessions-prune; accounts-svc pruned the expired session (durable H -> D on the delivery tx)"
            $spOk = $true; break
        }
        Start-Sleep -Milliseconds 500
    }
    if (-not $spOk) { Fail 'expired session was never pruned (scheduler H -> shared log -> accounts D subscription accounts.prune-on-scheduler.v1)' }
    # Clean up the throwaway player (CASCADE removes any residual session) so reruns start fresh.
    Invoke-Sql "DELETE FROM accounts.players WHERE id='$spPid'::uuid;" | Out-Null

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
    #   (v)   rating (DB-backed projection, no public read op): the sync MMR read is proven
    #         by (i) succeeding with J UP; the +15/-15 durable handler persists to
    #         rating.ratings on J, asserted directly in [MT5] after both reports.
    #   (vi)  re-POSTing [MT1]'s exact ReportId is an idempotent no-op: still 202, one
    #         match row, no third match.finished (a caller replay after an ambiguous
    #         response MUST NOT double-commit a match).
    $Winner = "champ-$RunSuffix"
    $Loser = "chump-$RunSuffix"
    # ReportId is the REQUIRED idempotency key. Per-run-unique (RunSuffix): the cleanup
    # below deletes leaderboard/rating rows but NOT match.matches, so a constant id
    # would dedup on the SECOND split-proof run and [MT2]/[MT4] would never see wins move.
    $Mt1Rid = "mt1-$RunSuffix"
    $Mt4Rid = "mt4-$RunSuffix"

    Write-Host "[MT1] POST http://127.0.0.1:$GPort/match/report (AuthNone, capitalized ReportId/Winner/Loser body keys)"
    $mr = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/match/report",
        '-H', 'X-Api-Key: dev-key-server',
        '-H', 'Content-Type: application/json',
        '-d', "{`"ReportId`":`"$Mt1Rid`",`"Winner`":`"$Winner`",`"Loser`":`"$Loser`"}")
    Write-Host "    -> HTTP $($mr.Code)"
    if ($mr.Code -eq '202') {
        Pass 'match.report through G -> 202 (AuthNone; match-svc read rating.mmr from rating-svc over QUIC, recorded + emit_tx match.finished)'
    } else {
        Fail "match.report expected 202, got $($mr.Code)"
    }

    Write-Host "[MT2] poll GET http://127.0.0.1:$GPort/leaderboard through G until $Winner shows wins=1"
    $lbOk = $false
    for ($i = 1; $i -le 30; $i++) {
        $lb = Invoke-Curl @("http://127.0.0.1:$GPort/leaderboard", '-H', 'X-Api-Key: dev-key-client')
        if ($lb.Body -match "`"player`":`"$Winner`",`"wins`":1") {
            Write-Host "    attempt $i -> $($lb.Body)"
            Pass "leaderboard shows $Winner wins=1 (durable match.finished I->K + on_tx upsert; G routes leaderboard.topScores Remote to K)"
            $lbOk = $true; break
        }
        Start-Sleep -Milliseconds 500
    }
    if (-not $lbOk) { Fail "leaderboard never showed $Winner wins=1 (durable I->K delivery / upsert / routing)" }

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

    Write-Host "[MT4] second POST /match/report same winner -> leaderboard wins=2 (accumulating upsert)"
    $mr2 = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/match/report",
        '-H', 'X-Api-Key: dev-key-server',
        '-H', 'Content-Type: application/json',
        '-d', "{`"ReportId`":`"$Mt4Rid`",`"Winner`":`"$Winner`",`"Loser`":`"$Loser`"}")
    Write-Host "    -> report#2 HTTP $($mr2.Code)"
    if ($mr2.Code -ne '202') { Fail "second match.report expected 202, got $($mr2.Code)" }
    $mt4Ok = $false
    for ($i = 1; $i -le 30; $i++) {
        $lb = Invoke-Curl @("http://127.0.0.1:$GPort/leaderboard", '-H', 'X-Api-Key: dev-key-client')
        if ($lb.Body -match "`"player`":`"$Winner`",`"wins`":2") {
            Write-Host "    attempt $i -> $Winner wins=2"
            Pass "second report -> $Winner wins=2 (leaderboard upsert accumulates across durable events)"
            $mt4Ok = $true; break
        }
        Start-Sleep -Milliseconds 500
    }
    if (-not $mt4Ok) { Fail "leaderboard never reached wins=2 for $Winner (accumulating upsert)" }

    # rating is a DURABLE PROJECTION (Step 9), not in-memory: the +15/-15 handler upserts
    # rating.ratings on J inside the delivery tx. After the two reports above the winner
    # is 1000+15+15=1030 and the loser 1000-15-15=970 -- a persisted value the checkpoint
    # advanced over, so a restart would NOT reset it.
    Write-Host "[MT5] poll rating.ratings on J for the persisted projection (winner=$Winner -> mmr=1030, loser=$Loser -> mmr=970)"
    $mt5Ok = $false
    for ($i = 1; $i -le 30; $i++) {
        $wMmr = ("" + (Invoke-Sql "SELECT coalesce((SELECT mmr FROM rating.ratings WHERE player='$Winner'), -1);")).Trim()
        $lMmr = ("" + (Invoke-Sql "SELECT coalesce((SELECT mmr FROM rating.ratings WHERE player='$Loser'), -1);")).Trim()
        Write-Host "    attempt $i -> winner mmr=$wMmr, loser mmr=$lMmr"
        if ([int]($wMmr -as [int]) -eq 1030 -and [int]($lMmr -as [int]) -eq 970) {
            Pass "rating.ratings persisted $Winner=1030 / $Loser=970 (durable +15/-15 projection on J, restart-safe)"
            $mt5Ok = $true; break
        }
        Start-Sleep -Milliseconds 500
    }
    if (-not $mt5Ok) { Fail "rating.ratings never reached winner=1030 / loser=970 (durable projection on J)" }

    # --- [MT6] duplicate-report-idempotent: mutating RPCs are not transport-retried,
    # but a caller may re-send after an ambiguous result. Re-POST with [MT1]'s exact ReportId -> still
    # 202, but NO second match row (psql, the strong assertion) and NO third
    # match.finished (leaderboard wins stays 2).
    Write-Host "[MT6] duplicate POST /match/report with [MT1]'s ReportId ($Mt1Rid) -> 202 no-op (dedup)"
    $mr3 = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$GPort/match/report",
        '-H', 'X-Api-Key: dev-key-server',
        '-H', 'Content-Type: application/json',
        '-d', "{`"ReportId`":`"$Mt1Rid`",`"Winner`":`"$Winner`",`"Loser`":`"$Loser`"}")
    Write-Host "    -> duplicate report HTTP $($mr3.Code)"
    if ($mr3.Code -ne '202') { Fail "duplicate match.report expected 202 (idempotent no-op), got $($mr3.Code)" }
    Start-Sleep -Seconds 2 # give a hypothetical (wrong) third match.finished time to reach leaderboard
    $mt6Rows = ("" + (Invoke-Sql "SELECT count(*) FROM match.matches WHERE report_id='$Mt1Rid';")).Trim()
    $mt6Lb = Invoke-Curl @("http://127.0.0.1:$GPort/leaderboard", '-H', 'X-Api-Key: dev-key-client')
    Write-Host "    -> match.matches rows for $Mt1Rid = $mt6Rows"
    if ([int]($mt6Rows -as [int]) -eq 1 -and $mt6Lb.Body -match "`"player`":`"$Winner`",`"wins`":2") {
        Pass 'duplicate ReportId -> 202, 1 match row, leaderboard wins still 2 (dedup skipped the emit)'
    } else {
        Fail "duplicate ReportId not idempotent (rows=$mt6Rows, wins!=2?)"
    }

    Invoke-Sql "DELETE FROM leaderboard.scores WHERE player IN ('$Winner','$Loser');" | Out-Null
    Invoke-Sql "DELETE FROM rating.ratings WHERE player IN ('$Winner','$Loser');" | Out-Null

    Write-Host ''
    Write-Host "========= PLAYER QUIC FRONT (via gateway-svc :$PlayerPort) ========="
    # DEFERRED (documented, not faked): the player-QUIC ANTI-SPOOF branch -- admission
    # gated on a validated source address (unvalidated Incoming -> quinn retry(), no
    # connection slot consumed) -- cannot be asserted here: this script cannot forge an
    # off-path UDP source address, and a happy-path playercli dial only proves the
    # validated branch. Coverage lives in the unit tests (core/edge/src/player_tests.rs).
    # The player RATE-LIMIT (a different, observable guard) IS asserted below in [P6].

    # --- P1. player QUIC create -> G -> mTLS edge -> A ---
    # A FRESH character owned by the registered player, created THROUGH the QUIC player front (the
    # original cid from [1] was deleted in [4]). playercli exits 0 iff transport ok AND
    # the payload's status=="Ok".
    Write-Host "[P1] playercli characters.create over QUIC :$PlayerPort (--token <real> --api-key dev-key-client)"
    $p1 = Invoke-PlayerCli @('--addr', "127.0.0.1:$PlayerPort", '--ca', $CaCert, '--token', $Token, '--api-key', 'dev-key-client', 'characters.create', '{"name":"hero","class":""}')
    Write-Host "    -> rc=$($p1.Rc)  $($p1.Out)"
    $pcid = $null
    if ($p1.Out -match '"id":"([^"]+)"') { $pcid = $Matches[1] }
    if ($p1.Rc -eq 0 -and $pcid) { Pass "player create -> exit 0, id=$pcid (player QUIC -> G -> mTLS edge -> A)" } else { Fail "player create expected exit 0 with id, got rc=$($p1.Rc)" }

    # --- P2. player QUIC inventory list -> G -> Remote -> B's NEW :9001 edge ---
    # The newest composition: P1 alone only proves the G->A leg; this proves player QUIC
    # -> G -> Remote -> B, and B in turn calls owner_of over QUIC/mTLS to A.
    Write-Host "[P2] playercli inventory.listCharacter over QUIC :$PlayerPort (player QUIC -> G -> Remote -> B :$BEdgePort)"
    $p2 = Invoke-PlayerCli @('--addr', "127.0.0.1:$PlayerPort", '--ca', $CaCert, '--token', $Token, '--api-key', 'dev-key-client', 'inventory.listCharacter', "{`"character_id`":`"$pcid`"}")
    Write-Host "    -> rc=$($p2.Rc)  $($p2.Out)"
    if ($p2.Rc -eq 0) { Pass "player inventory list -> exit 0 (player QUIC -> G -> Remote -> B :$BEdgePort -> owner_of QUIC -> A)" } else { Fail "player inventory list expected exit 0, got rc=$($p2.Rc)" }

    # --- P3. gateway-svc HTTP front still routes cross-provider inventory.* -> B ---
    Write-Host "[P3] GET http://127.0.0.1:$GPort/inventory/character/$pcid through gateway-svc HTTP front (Bearer `$Token)"
    $p3 = Invoke-Curl @("http://127.0.0.1:$GPort/inventory/character/$pcid", '-H', 'X-Api-Key: dev-key-client', '-H', "Authorization: Bearer $Token")
    Write-Host "    -> HTTP $($p3.Code)  $($p3.Body)"
    if ($p3.Code -eq '200') { Pass 'gateway-svc HTTP front routes inventory.* -> B remote -> 200' } else { Fail "gateway-svc HTTP inventory expected 200, got $($p3.Code)" }

    # --- P4. auth gate: no token / bad token on an auth op -> Unauthorized ---
    Write-Host "[P4] playercli characters.create with NO token (--api-key dev-key-client) -> exit 1 + Unauthorized"
    $p4 = Invoke-PlayerCli @('--addr', "127.0.0.1:$PlayerPort", '--ca', $CaCert, '--api-key', 'dev-key-client', 'characters.create', '{"name":"x","class":""}')
    Write-Host "    -> rc=$($p4.Rc)  $($p4.Out)"
    if ($p4.Rc -ne 0 -and $p4.Out -match 'Unauthorized') { Pass 'no-token auth op -> exit 1 + Unauthorized (bearer required at the front)' } else { Fail "no-token expected exit 1 + Unauthorized, got rc=$($p4.Rc) $($p4.Out)" }

    Write-Host "[P4b] playercli characters.create with BAD token (nope-x, --api-key dev-key-client) -> exit 1 + Unauthorized"
    $p4b = Invoke-PlayerCli @('--addr', "127.0.0.1:$PlayerPort", '--ca', $CaCert, '--token', 'nope-x', '--api-key', 'dev-key-client', 'characters.create', '{"name":"x","class":""}')
    Write-Host "    -> rc=$($p4b.Rc)  $($p4b.Out)"
    if ($p4b.Rc -ne 0 -and $p4b.Out -match 'Unauthorized') { Pass 'bad-token auth op -> exit 1 + Unauthorized (token verified, not just presence)' } else { Fail "bad-token expected exit 1 + Unauthorized, got rc=$($p4b.Rc) $($p4b.Out)" }

    # --- P5. allow-list gate: wire-only method absent from the route table ---
    # characters.ownerOf has no #[http] binding, so it is NOT in the front's route table
    # -> NotFound. Proves dispatch is method-allow-listed, never a blind prefix relay.
    Write-Host "[P5] playercli characters.ownerOf (wire-only, not routable) -> exit 1 + NotFound"
    $p5 = Invoke-PlayerCli @('--addr', "127.0.0.1:$PlayerPort", '--ca', $CaCert, '--token', $Token, '--api-key', 'dev-key-client', 'characters.ownerOf', "{`"character_id`":`"$pcid`"}")
    Write-Host "    -> rc=$($p5.Rc)  $($p5.Out)"
    if ($p5.Rc -ne 0 -and $p5.Out -match 'NotFound') { Pass 'wire-only characters.ownerOf -> exit 1 + NotFound (allow-list gate live)' } else { Fail "ownerOf expected exit 1 + NotFound, got rc=$($p5.Rc) $($p5.Out)" }

    Write-Host '[P6] persistent player connection consumes burst, gets exact denial, refills, then succeeds'
    $p6 = Invoke-PlayerCli @('--addr', "127.0.0.1:$PlayerPort", '--ca', $CaCert, '--api-key', 'dev-key-client', '--repeat', '22', '--pause-before-last-ms', '2000', 'leaderboard.topScores', '{}')
    Write-Host "    -> rc=$($p6.Rc)  $($p6.Out)"
    $okCount = ([regex]::Matches($p6.Out, '"status":"Ok"')).Count
    if ($p6.Rc -ne 0 -and $p6.Out -match 'player request rate limit exceeded' -and $okCount -ge 21) { Pass 'persistent player request limiter returns pinned denial and admits after refill' } else { Fail "persistent player request limiter proof failed: rc=$($p6.Rc) $($p6.Out)" }

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
    $rlCodes = & curl.exe -Z --parallel-max 60 -s -o NUL -w "%{http_code}`n" -H 'X-Api-Key: dev-key-client' @lbUrls 2>$null
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
    $rlOk = Invoke-Curl @("http://127.0.0.1:$GPort/leaderboard", '-H', 'X-Api-Key: dev-key-client')
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
    # The in-flight-request-survives-drain probe is DEFERRED: the fleet has no
    # artificial-delay endpoint to race a request against shutdown without being
    # racy/low-value. DEFERRED for the same reason: the internal-edge STREAM-REAP fix
    # (bounded read/stopped waits on a hung peer stream) -- there is no fault-injection
    # endpoint in the fleet to wedge a stream cross-process. Coverage lives in the edge
    # unit tests (core/edge/src/server_tests.rs, loopback peer with controlled waits).
    Teardown $true 'W1 split graceful shutdown'
    foreach ($n in ($FleetSvcs + 'server')) {
        Get-Process -Name $n -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Seconds 2
    # Wipe the split phase's lockout + session residue so the monolith-parity login starts
    # clean (proofadmin's split session and any prooflock lock rows must not poison parity).
    # admin.users (the seeded logins) is left intact -- the monolith reuses proofadmin.
    Invoke-Sql 'DELETE FROM admin.login_attempts; DELETE FROM admin.sessions;' | Out-Null
    Note 'cleared admin.login_attempts + admin.sessions before the monolith parity phase'
    # Per-process env in this script leaks across Start-Svc calls; neutralize the split
    # passthrough knobs so the monolith serves /admin LOCALLY (not proxied). Admin now boots
    # on the shared `admin` schema with session auth (no ADMIN_USER/ADMIN_PASS).
    # APIKEYS_DEV_SEED=1 self-heals the dev keys against the local apikeys module.
    $env:ADMIN_HTTP_ADDR = ''
    $env:ACCOUNTS_HTTP_ADDR = ''
    $env:APIKEYS_DEV_SEED = '1'

    Note "starting monolith (cmd/server) on :$APort, player QUIC :$PlayerPort ..."
    $script:MProc = Start-Svc (Join-Path $BinDir 'server.exe') @{
        PORT               = ":$APort"
        DATABASE_URL       = $env:DATABASE_URL
        PLAYER_EDGE_ADDR   = ":$PlayerPort"
        EDGE_CA_CERT       = $CaCert
        EDGE_CA_KEY        = $CaKey
        APIKEYS_DEV_SEED   = '1'
        ACCOUNTS_DEV_AUTH  = '1'
        INVENTORY_DEV_GRANT = '1'
        TLS_MODE           = 'off'
        ADMIN_COOKIE_SECURE = '0'
        TRUSTED_PROXY_CIDRS = '127.0.0.1/32'
    } 'monolith'
    if (Wait-Healthy $APort 'monolith (server)') {
        $msuffix = [guid]::NewGuid().ToString().Substring(0, 8)
        Write-Host '[M0] register a player on the monolith (accounts module local, real session)'
        $mreg = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$APort/accounts/register",
            '-H', 'X-Api-Key: dev-key-client',
            '-H', 'Content-Type: application/json',
            '-d', "{`"email`":`"mono-$msuffix@test.local`",`"password`":`"pw-$msuffix`",`"displayName`":`"Mono`"}")
        $mtoken = $null
        if ($mreg.Body -match '"token":"([^"]+)"') { $mtoken = $Matches[1] }
        if ($mtoken) { Pass 'monolith register -> real bearer (parity: same auth flow, all Local)' } else { Fail "monolith register produced no token -- $($mreg.Body)" }
        Write-Host "[M1] playercli characters.create over QUIC :$PlayerPort against the monolith (--token <real> --api-key dev-key-client)"
        $m1 = Invoke-PlayerCli @('--addr', "127.0.0.1:$PlayerPort", '--ca', $CaCert, '--token', $mtoken, '--api-key', 'dev-key-client', 'characters.create', '{"name":"solo","class":""}')
        Write-Host "    -> rc=$($m1.Rc)  $($m1.Out)"
        if ($m1.Rc -eq 0) { Pass 'monolith player QUIC front -> exit 0 (all ops Local, parity proven)' } else { Fail "monolith player create expected exit 0, got rc=$($m1.Rc)" }
        Write-Host '[M2] monolith rejects a dev- token (real verifier resolved from the local accounts module)'
        $m2 = Invoke-PlayerCli @('--addr', "127.0.0.1:$PlayerPort", '--ca', $CaCert, '--token', "dev-$msuffix", '--api-key', 'dev-key-client', 'characters.create', '{"name":"x","class":""}')
        Write-Host "    -> rc=$($m2.Rc)  $($m2.Out)"
        if ($m2.Rc -ne 0 -and $m2.Out -match 'Unauthorized') { Pass 'monolith dev- token -> Unauthorized (parity with the split front)' } else { Fail "monolith dev- token expected Unauthorized, got rc=$($m2.Rc) $($m2.Out)" }
        # [M3] admin portal parity: the monolith hosts the admin module LOCAL (no fan-out),
        # with session auth on the SAME shared `admin` schema. A fresh cookie jar logs in as
        # proofadmin, then the jar renders the LOCAL characters page (the just-created "solo"
        # character) -- proving the same session-auth portal serves both topologies.
        $MonoJar = Join-Path $RunDir 'admin-mono.jar'
        Remove-Item $MonoJar -ErrorAction SilentlyContinue
        Write-Host "[M3] POST /admin/login on the monolith as $ProofAdminUser (fresh jar) -> 303, then GET /admin/characters -> 200 + solo"
        $m3l = "" + (& curl.exe -s -c $MonoJar -o NUL -w '%{http_code}' -X POST "http://127.0.0.1:$APort/admin/login" -d "username=$ProofAdminUser&password=$ProofAdminPass")
        $m3 = Invoke-Curl @('-b', $MonoJar, "http://127.0.0.1:$APort/admin/characters")
        Write-Host "    -> login HTTP $($m3l.Trim()) ; characters HTTP $($m3.Code)  (body $($m3.Body.Length) chars)"
        if ($m3l.Trim() -eq '303' -and $m3.Code -eq '200' -and $m3.Body -match 'solo') { Pass 'monolith session login + /admin/characters renders LOCAL items (admin portal parity)' } else { Fail "monolith admin parity expected login 303 + characters 200 containing solo, got login=$($m3l.Trim()) characters=$($m3.Code)" }

        # [M3b] LOCAL form-submit durable trail: in the monolith the apikeys page is LOCAL
        # (editable form present). Fetch it, extract the session's _csrf + the current field
        # values, resubmit them unchanged WITH _csrf (a valid no-op edit), and assert a NEW
        # admin.action{form-submit} row landed on the shared log. Remote forms in the split
        # are read-only, so this parity leg is the only place form-submit is exercised.
        Write-Host '[M3b] submit the LOCAL apikeys edit form WITH _csrf -> a new admin.action form-submit row'
        $m3bBefore = [int](("" + (Invoke-Sql "SELECT count(*) FROM asyncevents.events WHERE topic='admin.action' AND payload->>'action'='form-submit';")).Trim())
        $m3bPage = Invoke-Curl @('-b', $MonoJar, "http://127.0.0.1:$APort/admin/api-keys")
        $m3bCsrf = if ($m3bPage.Body -match 'name="_csrf" value="([^"]*)"') { $Matches[1] } else { '' }
        $m3bArgs = @('-X', 'POST', '-b', $MonoJar, "http://127.0.0.1:$APort/admin/api-keys", '--data-urlencode', "_csrf=$m3bCsrf")
        foreach ($m in [regex]::Matches($m3bPage.Body, '<input type="text" name="([^"]*)" value="([^"]*)">')) {
            $m3bArgs += @('--data-urlencode', "$($m.Groups[1].Value)=$($m.Groups[2].Value)")
        }
        $m3b = Invoke-Curl $m3bArgs
        $m3bAfter = [int](("" + (Invoke-Sql "SELECT count(*) FROM asyncevents.events WHERE topic='admin.action' AND payload->>'action'='form-submit';")).Trim())
        Write-Host "    -> csrf=$($m3bCsrf.Substring(0, [Math]::Min(8, $m3bCsrf.Length)))... submit HTTP $($m3b.Code) ; form-submit rows $m3bBefore -> $m3bAfter"
        if (($m3b.Code -eq '303' -or $m3b.Code -eq '200') -and $m3bCsrf -ne '' -and $m3bAfter -gt $m3bBefore) {
            Pass 'monolith LOCAL apikeys form-submit -> new admin.action{form-submit} on the shared log (durable trail in the co-hosting process)'
        } else {
            Fail "monolith form-submit expected a new admin.action row (submit 303/200 + count up), got HTTP $($m3b.Code) csrf='$m3bCsrf' $m3bBefore->$m3bAfter"
        }
    } else {
        Fail "monolith (server) never became healthy on :$APort"
    }

    Teardown $true 'W2 monolith graceful shutdown'
    Write-Host '============================================'

    if ($script:Fails -eq 0) {
        Write-Host 'SPLIT PROOF: PASS (all assertions held on the twelve-process split + monolith parity)'
        $script:ExitCode = 0
    } else {
        Write-Host "SPLIT PROOF: FAIL ($($script:Fails) assertion(s) failed)"
        $script:ExitCode = 1
    }
}
finally {
    Teardown
    Stop-Transcript | Out-Null
}

exit $script:ExitCode
