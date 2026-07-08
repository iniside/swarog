# split-proof.ps1 -- the SPLIT-topology proof for the rust-sketch (Steps 12 + 8).
#
# The whole point of the milestone: exercises the FOUR-PROCESS split (characters-svc =
# A on :8080 / edge :9000, inventory-svc = B on :8081 / edge :9001, gateway-svc = G on
# :8082 / player QUIC :9100, config-svc = C on :8083 / edge :9002), NOT the monolith,
# driving the real player flows over HTTP
# (through the gateway front-door with a dev-<uuid> bearer), the sync authz over
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
$EdgePort  = 9000
$BEdgePort = 9001
$CEdgePort = 9002
$PlayerPort = 9100
$PlayerCli = Join-Path $BinDir 'playercli.exe'

$DefaultDsn = 'postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable'
if (-not $env:DATABASE_URL -or $env:DATABASE_URL.Trim() -eq '') { $env:DATABASE_URL = $DefaultDsn }

$script:Fails = 0
$script:AProc = $null
$script:BProc = $null
$script:GProc = $null
$script:CProc = $null
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
    if ($script:MProc -and -not $script:MProc.HasExited) { Stop-Process -Id $script:MProc.Id -Force -ErrorAction SilentlyContinue; Note "stopped monolith (pid $($script:MProc.Id))" }
    $script:AProc = $null; $script:BProc = $null; $script:GProc = $null; $script:CProc = $null; $script:MProc = $null
}

# Runs playercli, capturing stdout (joined) and the process exit code. Returns a
# pscustomobject { Rc; Out }. playercli exits 0 iff transport ok AND status=="Ok".
function Invoke-PlayerCli([string[]]$CliArgs) {
    $out = & $PlayerCli @CliArgs 2>$null
    $rc = $LASTEXITCODE
    return [pscustomobject]@{ Rc = $rc; Out = (($out | Out-String)).Trim() }
}

try {
    Note 'building edgeca + characters-svc + inventory-svc + gateway-svc + config-svc + playercli + server ...'
    cargo build -p edgeca -p characters-svc -p inventory-svc -p gateway-svc -p config-svc -p playercli -p server
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

    New-Item -ItemType Directory -Force -Path $RunDir | Out-Null

    # Clear stragglers from an aborted prior run so ports are free (idempotent reruns).
    foreach ($n in 'characters-svc', 'inventory-svc', 'gateway-svc', 'config-svc', 'server') {
        Get-Process -Name $n -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Milliseconds 500

    Note "minting shared edge dev CA -> $CaCert"
    & (Join-Path $BinDir 'edgeca.exe') --cert $CaCert --key $CaKey
    if ($LASTEXITCODE -ne 0) { throw 'edgeca failed' }

    Note "starting A (characters-svc) on :$APort, edge :$EdgePort ..."
    $script:AProc = Start-Svc (Join-Path $BinDir 'characters-svc.exe') @{
        PORT               = ":$APort"
        DATABASE_URL       = $env:DATABASE_URL
        EDGE_ADDR          = ":$EdgePort"
        EDGE_CA_CERT       = $CaCert
        EDGE_CA_KEY        = $CaKey
        MESSAGING_ORIGIN   = 'characters-svc'
        EVENTS_SUBSCRIBERS = "character.created=http://127.0.0.1:$BPort/events;character.deleted=http://127.0.0.1:$BPort/events"
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
    # MESSAGING_ORIGIN or messaging's origin-collision guard would reject it.
    Note "starting C (config-svc) on :$CPort, edge :$CEdgePort ..."
    $script:CProc = Start-Svc (Join-Path $BinDir 'config-svc.exe') @{
        PORT               = ":$CPort"
        DATABASE_URL       = $env:DATABASE_URL
        EDGE_ADDR          = ":$CEdgePort"
        EDGE_CA_CERT       = $CaCert
        EDGE_CA_KEY        = $CaKey
        MESSAGING_ORIGIN   = 'config-svc'
        EVENTS_SUBSCRIBERS = "config.changed=http://127.0.0.1:$BPort/events"
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
        MESSAGING_ORIGIN     = 'inventory-svc'
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
    } 'gateway'
    if (-not (Wait-Healthy $GPort 'G (gateway-svc)')) { throw 'G failed to start' }

    $PID_ = [guid]::NewGuid().ToString()
    $Other = [guid]::NewGuid().ToString()
    Note "player PID=$PID_  (other player=$Other)"

    Write-Host ''
    Write-Host '================ SPLIT PROOF ================'

    # --- 1. CREATE on A ---
    Write-Host "[1] POST http://127.0.0.1:$APort/characters (Bearer dev-`$PID)"
    $c = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$APort/characters",
        '-H', "Authorization: Bearer dev-$PID_", '-H', 'Content-Type: application/json',
        '-d', '{"name":"Aria","class":"mage"}')
    Write-Host "    -> HTTP $($c.Code)  $($c.Body)"
    $cid = $null
    if ($c.Body -match '"id":"([^"]+)"') { $cid = $Matches[1] }
    if ($c.Code -eq '201' -and $cid) { Pass "create -> 201, id=$cid" } else { Fail 'create expected 201 with id' }

    # --- 2. ASYNC event A->B + SYNC authz B->A over QUIC ---
    Write-Host "[2] poll GET http://127.0.0.1:$BPort/inventory/character/$cid until starter appears"
    $starterOk = $false
    for ($i = 1; $i -le 30; $i++) {
        $r = Invoke-Curl @("http://127.0.0.1:$BPort/inventory/character/$cid", '-H', "Authorization: Bearer dev-$PID_")
        if ($r.Code -eq '200' -and $r.Body -match 'starter_sword') {
            Write-Host "    attempt $i -> HTTP 200 $($r.Body)"
            Pass 'starter_sword materialized in B (async event A->B) AND 200 proves owner_of over QUIC/mTLS B->A'
            $starterOk = $true; break
        }
        Start-Sleep -Milliseconds 500
    }
    if (-not $starterOk) { Fail 'starter never appeared in B (async cross-process grant / QUIC authz)' }

    # --- 3. NEGATIVE authz ---
    Write-Host "[3] GET /inventory/character/$cid as a DIFFERENT player (Bearer dev-`$OTHER)"
    $n = Invoke-Curl @("http://127.0.0.1:$BPort/inventory/character/$cid", '-H', "Authorization: Bearer dev-$Other")
    Write-Host "    -> HTTP $($n.Code)  $($n.Body)"
    if ($n.Code -eq '403' -or $n.Code -eq '404') { Pass "other player -> $($n.Code) (owner_of over QUIC gates)" } else { Fail "negative authz expected 403/404, got $($n.Code)" }

    # --- 4. DELETE on A ---
    Write-Host "[4] DELETE http://127.0.0.1:$APort/characters/$cid (Bearer dev-`$PID)"
    $d = Invoke-Curl @('-X', 'DELETE', "http://127.0.0.1:$APort/characters/$cid", '-H', "Authorization: Bearer dev-$PID_")
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
        $w = Invoke-Curl @("http://127.0.0.1:$BPort/inventory/character/$cid", '-H', "Authorization: Bearer dev-$PID_")
        Write-Host "    -> HTTP $($w.Code)"
        if ($w.Code -eq '404') { Pass 'post-delete GET -> 404 (character gone; DB wipe unverified, psql missing)' } else { Fail "post-delete expected 404, got $($w.Code)" }
    }

    Write-Host "[5b] post-delete GET /inventory/character/$cid (Bearer dev-`$PID)"
    $w2 = Invoke-Curl @("http://127.0.0.1:$BPort/inventory/character/$cid", '-H', "Authorization: Bearer dev-$PID_")
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
        Write-Host '[C1] baseline: create a character -> starter should be the DEFAULT starter_sword'
        $bc = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$APort/characters",
            '-H', "Authorization: Bearer dev-$PID_", '-H', 'Content-Type: application/json',
            '-d', '{"name":"Baseline","class":"mage"}')
        $bcid = $null
        if ($bc.Body -match '"id":"([^"]+)"') { $bcid = $Matches[1] }
        $baseOk = $false
        for ($i = 1; $i -le 30; $i++) {
            $r = Invoke-Curl @("http://127.0.0.1:$BPort/inventory/character/$bcid", '-H', "Authorization: Bearer dev-$PID_")
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
            $nc = Invoke-Curl @('-X', 'POST', "http://127.0.0.1:$APort/characters",
                '-H', "Authorization: Bearer dev-$PID_", '-H', 'Content-Type: application/json',
                '-d', '{"name":"Reloaded","class":"mage"}')
            $ncid = $null
            if ($nc.Body -match '"id":"([^"]+)"') { $ncid = $Matches[1] }
            $r = $null
            for ($j = 1; $j -le 10; $j++) {
                $r = Invoke-Curl @("http://127.0.0.1:$BPort/inventory/character/$ncid", '-H', "Authorization: Bearer dev-$PID_")
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
    Write-Host "========= PLAYER QUIC FRONT (via gateway-svc :$PlayerPort) ========="

    # --- P1. player QUIC create -> G -> mTLS edge -> A ---
    # A FRESH character owned by dev-$PID, created THROUGH the QUIC player front (the
    # original cid from [1] was deleted in [4]). playercli exits 0 iff transport ok AND
    # the payload's status=="Ok".
    Write-Host "[P1] playercli characters.create over QUIC :$PlayerPort (--token dev-`$PID)"
    $p1 = Invoke-PlayerCli @('--addr', "127.0.0.1:$PlayerPort", '--ca', $CaCert, '--token', "dev-$PID_", 'characters.create', '{"name":"hero","class":""}')
    Write-Host "    -> rc=$($p1.Rc)  $($p1.Out)"
    $pcid = $null
    if ($p1.Out -match '"id":"([^"]+)"') { $pcid = $Matches[1] }
    if ($p1.Rc -eq 0 -and $pcid) { Pass "player create -> exit 0, id=$pcid (player QUIC -> G -> mTLS edge -> A)" } else { Fail "player create expected exit 0 with id, got rc=$($p1.Rc)" }

    # --- P2. player QUIC inventory list -> G -> Remote -> B's NEW :9001 edge ---
    # The newest composition: P1 alone only proves the G->A leg; this proves player QUIC
    # -> G -> Remote -> B, and B in turn calls owner_of over QUIC/mTLS to A.
    Write-Host "[P2] playercli inventory.listCharacter over QUIC :$PlayerPort (player QUIC -> G -> Remote -> B :$BEdgePort)"
    $p2 = Invoke-PlayerCli @('--addr', "127.0.0.1:$PlayerPort", '--ca', $CaCert, '--token', "dev-$PID_", 'inventory.listCharacter', "{`"character_id`":`"$pcid`"}")
    Write-Host "    -> rc=$($p2.Rc)  $($p2.Out)"
    if ($p2.Rc -eq 0) { Pass "player inventory list -> exit 0 (player QUIC -> G -> Remote -> B :$BEdgePort -> owner_of QUIC -> A)" } else { Fail "player inventory list expected exit 0, got rc=$($p2.Rc)" }

    # --- P3. gateway-svc HTTP front still routes cross-provider inventory.* -> B ---
    Write-Host "[P3] GET http://127.0.0.1:$GPort/inventory/character/$pcid through gateway-svc HTTP front (Bearer dev-`$PID)"
    $p3 = Invoke-Curl @("http://127.0.0.1:$GPort/inventory/character/$pcid", '-H', "Authorization: Bearer dev-$PID_")
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
    $p5 = Invoke-PlayerCli @('--addr', "127.0.0.1:$PlayerPort", '--ca', $CaCert, '--token', "dev-$PID_", 'characters.ownerOf', "{`"character_id`":`"$pcid`"}")
    Write-Host "    -> rc=$($p5.Rc)  $($p5.Out)"
    if ($p5.Rc -ne 0 -and $p5.Out -match 'NotFound') { Pass 'wire-only characters.ownerOf -> exit 1 + NotFound (allow-list gate live)' } else { Fail "ownerOf expected exit 1 + NotFound, got rc=$($p5.Rc) $($p5.Out)" }

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
    foreach ($n in 'characters-svc', 'inventory-svc', 'gateway-svc', 'config-svc', 'server') {
        Get-Process -Name $n -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Seconds 2

    Note "starting monolith (cmd/server) on :$APort, player QUIC :$PlayerPort ..."
    $script:MProc = Start-Svc (Join-Path $BinDir 'server.exe') @{
        PORT             = ":$APort"
        DATABASE_URL     = $env:DATABASE_URL
        PLAYER_EDGE_ADDR = ":$PlayerPort"
        EDGE_CA_CERT     = $CaCert
        EDGE_CA_KEY      = $CaKey
    } 'monolith'
    if (Wait-Healthy $APort 'monolith (server)') {
        $mpid = [guid]::NewGuid().ToString()
        Write-Host "[M1] playercli characters.create over QUIC :$PlayerPort against the monolith (--token dev-`$MPID)"
        $m1 = Invoke-PlayerCli @('--addr', "127.0.0.1:$PlayerPort", '--ca', $CaCert, '--token', "dev-$mpid", 'characters.create', '{"name":"solo","class":""}')
        Write-Host "    -> rc=$($m1.Rc)  $($m1.Out)"
        if ($m1.Rc -eq 0) { Pass 'monolith player QUIC front -> exit 0 (all ops Local, parity proven)' } else { Fail "monolith player create expected exit 0, got rc=$($m1.Rc)" }
    } else {
        Fail "monolith (server) never became healthy on :$APort"
    }

    Write-Host '============================================'
}
finally {
    Teardown
}

if ($script:Fails -eq 0) {
    Write-Host 'SPLIT PROOF: PASS (all assertions held on the four-process split + monolith parity)'
    exit 0
} else {
    Write-Host "SPLIT PROOF: FAIL ($($script:Fails) assertion(s) failed)"
    exit 1
}
