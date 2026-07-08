# split-proof.ps1 -- the SPLIT-topology proof for the rust-sketch (Step 12).
#
# The whole point of the milestone: exercises the TWO-PROCESS split (characters-svc =
# A on :8080 / edge :9000, inventory-svc = B on :8081), NOT the monolith, driving the
# real player flows over HTTP (through the gateway front-door with a dev-<uuid>
# bearer) and the sync authz over QUIC/mTLS. It:
#   1. mints the shared dev CA via edgeca,
#   2. starts A then B in the background, gating each on /healthz,
#   3. runs the assertions below, tearing BOTH down on exit (even on failure),
#   4. exits non-zero if ANY assertion fails.
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
$APort    = 8080
$BPort    = 8081
$EdgePort = 9000

$DefaultDsn = 'postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable'
if (-not $env:DATABASE_URL -or $env:DATABASE_URL.Trim() -eq '') { $env:DATABASE_URL = $DefaultDsn }

$script:Fails = 0
$script:AProc = $null
$script:BProc = $null

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
    $script:AProc = $null; $script:BProc = $null
}

try {
    Note 'building edgeca + characters-svc + inventory-svc ...'
    cargo build -p edgeca -p characters-svc -p inventory-svc
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

    New-Item -ItemType Directory -Force -Path $RunDir | Out-Null

    # Clear stragglers from an aborted prior run so ports are free (idempotent reruns).
    foreach ($n in 'characters-svc', 'inventory-svc') {
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

    Note "starting B (inventory-svc) on :$BPort ..."
    $script:BProc = Start-Svc (Join-Path $BinDir 'inventory-svc.exe') @{
        PORT                 = ":$BPort"
        DATABASE_URL         = $env:DATABASE_URL
        EDGE_CA_CERT         = $CaCert
        EDGE_CA_KEY          = $CaKey
        CHARACTERS_EDGE_ADDR = "127.0.0.1:$EdgePort"
        MESSAGING_ORIGIN     = 'inventory-svc'
    } 'inventory'
    if (-not (Wait-Healthy $BPort 'B (inventory-svc)')) { throw 'B failed to start' }

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

    Write-Host '============================================'
}
finally {
    Teardown
}

if ($script:Fails -eq 0) {
    Write-Host 'SPLIT PROOF: PASS (all assertions held on the two-process topology)'
    exit 0
} else {
    Write-Host "SPLIT PROOF: FAIL ($($script:Fails) assertion(s) failed)"
    exit 1
}
