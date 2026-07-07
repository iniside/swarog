# verify.ps1 - one-shot local verification gate. Runs every verification stage,
# keeps going after a failure, prints a summary table, and exits non-zero iff a
# BLOCKING stage failed (or ANY stage failed under -Strict).
#
# Usage:
#   .\verify.ps1                 # -Fast: blocking stages only (default)
#   .\verify.ps1 -Fast           # same as default
#   .\verify.ps1 -All            # + advisory: test-race, fuzz, apidiff, topiccheck, rpcgen
#   .\verify.ps1 -Slow           # + gremlins mutation testing (very slow)
#   .\verify.ps1 -All -Strict    # advisory failures ALSO flip the exit code
#   .\verify.ps1 -All -NoInstall # never auto-install a missing CLI (it SKIPs)
#
# Behavioural twin of verify.sh. Blocking stages: build, vet, golangci-lint,
# go-arch-lint, test, govulncheck. Advisory (-All): test-race, fuzz, apidiff,
# topiccheck, rpcgen. Slow (-Slow): gremlins. Per-stage output -> run/verify/<name>.log.

param(
    [switch]$Fast,
    [switch]$All,
    [switch]$Slow,
    [switch]$Strict,
    [switch]$NoInstall
)

# Deliberately NOT 'Stop' during the run phase: a failing stage must not abort
# the runner. Each stage records PASS/FAIL/SKIP; the summary decides the exit code.
$ErrorActionPreference = 'Continue'

$root = $PSScriptRoot
Set-Location $root
$runDir = Join-Path $root 'run'
$verifyDir = Join-Path $runDir 'verify'
New-Item -ItemType Directory -Force -Path $verifyDir | Out-Null

$RunAdvisory = $All.IsPresent -or $Slow.IsPresent
$RunGremlins = $Slow.IsPresent
$Install = -not $NoInstall.IsPresent
$StrictOn = $Strict.IsPresent

$contractPkgs = @(
    'gamebackend/modules/accounts/accountsevents',
    'gamebackend/modules/accounts/accountsapi',
    'gamebackend/modules/characters/charactersevents',
    'gamebackend/modules/characters/charactersapi',
    'gamebackend/modules/match/matchevents',
    'gamebackend/modules/scheduler/schedulerevents',
    'gamebackend/modules/admin/adminapi'
)

# --- Result accumulation ----------------------------------------------------
$script:results = @()
function Add-Result {
    param([string]$Name, [string]$Status, [bool]$Blocking)
    $script:results += [pscustomobject]@{ Name = $Name; Status = $Status; Blocking = $Blocking }
}

# Ensure-Tool BIN SPEC — $true if the tool is available (installing it if missing
# and installs are enabled), $false if unavailable (stage SKIPs).
function Ensure-Tool {
    param([string]$Bin, [string]$Spec)
    if (Get-Command $Bin -ErrorAction SilentlyContinue) { return $true }
    if (-not $Install) { return $false }
    Write-Host "installing $Spec ..." -ForegroundColor Yellow
    & go install $Spec
    return [bool](Get-Command $Bin -ErrorAction SilentlyContinue)
}

# Invoke-SimpleStage NAME BLOCKING EXE ARGS — runs EXE, logging to
# run/verify/NAME.log, recording PASS on exit 0 else FAIL.
function Invoke-SimpleStage {
    param([string]$Name, [bool]$Blocking, [string]$Exe, [string[]]$Arguments)
    $log = Join-Path $verifyDir "$Name.log"
    Write-Host "== $Name ==" -ForegroundColor Cyan
    & $Exe @Arguments *> $log
    if ($LASTEXITCODE -eq 0) {
        Write-Host "  PASS" -ForegroundColor Green
        Add-Result $Name 'PASS' $Blocking
    } else {
        Write-Host "  FAIL (see run/verify/$Name.log)" -ForegroundColor Red
        Add-Result $Name 'FAIL' $Blocking
    }
}

# --- Blocking stage: govulncheck (TEXT mode; exit 3 on vuln = FAIL) ----------
function Invoke-GovulncheckStage {
    $log = Join-Path $verifyDir 'govulncheck.log'
    Write-Host "== govulncheck ==" -ForegroundColor Cyan
    if (-not (Ensure-Tool 'govulncheck' 'golang.org/x/vuln/cmd/govulncheck@v1.5.0')) {
        Write-Host "  SKIP (govulncheck unavailable)" -ForegroundColor Yellow
        'govulncheck unavailable (missing and -NoInstall, or install failed)' | Out-File $log
        Add-Result 'govulncheck' 'SKIP' $true
        return
    }
    & govulncheck ./... *> $log
    if ($LASTEXITCODE -eq 0) {
        Write-Host "  PASS" -ForegroundColor Green; Add-Result 'govulncheck' 'PASS' $true
    } else {
        Write-Host "  FAIL (see run/verify/govulncheck.log)" -ForegroundColor Red; Add-Result 'govulncheck' 'FAIL' $true
    }
}

# --- Advisory stage: test-race (probe cgo+gcc first) ------------------------
function Invoke-RaceStage {
    $log = Join-Path $verifyDir 'test-race.log'
    Write-Host "== test-race ==" -ForegroundColor Cyan
    $cgo = (& go env CGO_ENABLED).Trim()
    $gcc = Get-Command gcc -ErrorAction SilentlyContinue
    if ($cgo -eq '1' -and $gcc) {
        & go test ./... -race *> $log
        if ($LASTEXITCODE -eq 0) {
            Write-Host "  PASS" -ForegroundColor Green; Add-Result 'test-race' 'PASS' $false
        } else {
            Write-Host "  FAIL (see run/verify/test-race.log)" -ForegroundColor Red; Add-Result 'test-race' 'FAIL' $false
        }
    } else {
        Write-Host "  SKIP (no cgo/gcc)" -ForegroundColor Yellow
        "skipped: CGO_ENABLED=$cgo, gcc present=$([bool]$gcc)" | Out-File $log
        Add-Result 'test-race' 'SKIP' $false
    }
}

# --- Advisory stage: fuzz (discover every func Fuzz*, run each 10s) ----------
function Invoke-FuzzStage {
    $log = Join-Path $verifyDir 'fuzz.log'
    '' | Out-File $log
    Write-Host "== fuzz ==" -ForegroundColor Cyan
    $targets = @()
    foreach ($f in Get-ChildItem -Path $root -Recurse -Filter '*_test.go') {
        foreach ($m in (Select-String -Path $f.FullName -Pattern 'func (Fuzz[A-Za-z0-9_]+)\(')) {
            $targets += [pscustomobject]@{ Name = $m.Matches[0].Groups[1].Value; Dir = $f.Directory.FullName }
        }
    }
    if ($targets.Count -eq 0) {
        Write-Host "  SKIP (no fuzz targets)" -ForegroundColor Yellow; Add-Result 'fuzz' 'SKIP' $false; return
    }
    $anyfail = $false
    foreach ($t in $targets) {
        "--- $($t.Dir) $($t.Name) ---" | Out-File -Append $log
        & go test $t.Dir -run '^$' -fuzz "^$($t.Name)$" -fuzztime=10s *>> $log
        if ($LASTEXITCODE -ne 0) {
            Write-Host "  $($t.Name): FAIL" -ForegroundColor Red
            $anyfail = $true
        } else {
            Write-Host "  $($t.Name): ok"
        }
    }
    if (-not $anyfail) {
        Write-Host "  PASS" -ForegroundColor Green; Add-Result 'fuzz' 'PASS' $false
    } else {
        Write-Host "  FAIL (see run/verify/fuzz.log)" -ForegroundColor Red; Add-Result 'fuzz' 'FAIL' $false
    }
}

# --- Advisory stage: apidiff (base = HEAD via a detached worktree) -----------
function Invoke-ApidiffStage {
    $log = Join-Path $verifyDir 'apidiff.log'
    '' | Out-File $log
    Write-Host "== apidiff ==" -ForegroundColor Cyan
    if (-not (Ensure-Tool 'apidiff' 'golang.org/x/exp/cmd/apidiff@latest')) {
        Write-Host "  SKIP (apidiff unavailable)" -ForegroundColor Yellow
        'apidiff unavailable' | Out-File $log; Add-Result 'apidiff' 'SKIP' $false; return
    }
    $wt = Join-Path ([System.IO.Path]::GetTempPath()) ("verify-apidiff-" + [guid]::NewGuid().ToString('N'))
    & git worktree add --detach $wt HEAD *>> $log
    if ($LASTEXITCODE -ne 0) {
        Write-Host "  FAIL (git worktree add failed, see run/verify/apidiff.log)" -ForegroundColor Red
        Add-Result 'apidiff' 'FAIL' $false; return
    }
    $incompat = $false
    try {
        $i = 0
        foreach ($pkg in $contractPkgs) {
            $i++
            $snap = Join-Path $verifyDir "apidiff-$i.api"
            # -w writes the BASE (worktree = HEAD) API snapshot from inside the worktree;
            # -incompatible then compares that base against the CURRENT tree from repo root.
            Push-Location $wt
            & apidiff -w $snap $pkg *>> $log
            Pop-Location
            $out = & apidiff -incompatible $snap $pkg 2>> $log
            if ($out) { $out | Out-File -Append $log; $incompat = $true }
            Remove-Item -Force $snap -ErrorAction SilentlyContinue
        }
    } finally {
        # ALWAYS clean up the worktree, even if a comparison above threw.
        & git worktree remove --force $wt *>> $log
    }
    if (-not $incompat) {
        Write-Host "  PASS" -ForegroundColor Green; Add-Result 'apidiff' 'PASS' $false
    } else {
        Write-Host "  FAIL (incompatible changes, see run/verify/apidiff.log)" -ForegroundColor Red; Add-Result 'apidiff' 'FAIL' $false
    }
}

# --- Advisory stage: topiccheck (-Strict makes it able to FAIL) -------------
function Invoke-TopiccheckStage {
    $log = Join-Path $verifyDir 'topiccheck.log'
    Write-Host "== topiccheck ==" -ForegroundColor Cyan
    if ($StrictOn) {
        & go run ./tools/topiccheck ./... --strict *> $log
    } else {
        & go run ./tools/topiccheck ./... *> $log
    }
    if ($LASTEXITCODE -eq 0) {
        Write-Host "  PASS" -ForegroundColor Green; Add-Result 'topiccheck' 'PASS' $false
    } else {
        Write-Host "  FAIL (see run/verify/topiccheck.log)" -ForegroundColor Red; Add-Result 'topiccheck' 'FAIL' $false
    }
}

# --- Advisory stage: rpcgen -check (regen-diff the generated <module>rpc glue) ---
# Discovers every `//go:generate ... rpcgen ...` directive and re-runs each with
# -check (regenerate to memory, gofmt-normalize, diff — never write). Drift FAILs.
# Advisory by default, blocking under -Strict (via the generic summary rule). With
# no real <module>api packages yet (they land in Phase A1), the only directive is
# rpcgen's own testdata golden, so this is a live no-op-safe pass.
function Invoke-RpcgenStage {
    $log = Join-Path $verifyDir 'rpcgen.log'
    '' | Out-File $log
    Write-Host "== rpcgen ==" -ForegroundColor Cyan
    $found = $false
    $anyfail = $false
    foreach ($f in Get-ChildItem -Path $root -Recurse -Filter '*.go') {
        foreach ($m in (Select-String -Path $f.FullName -Pattern '//go:generate\s+(.*rpcgen.*)$')) {
            $found = $true
            # insert -check as the first rpcgen flag so it diffs instead of writing
            $cmd = $m.Matches[0].Groups[1].Value -replace 'rpcgen ', 'rpcgen -check '
            "--- $($f.Directory.FullName): $cmd ---" | Out-File -Append $log
            Push-Location $f.Directory.FullName
            Invoke-Expression $cmd *>> $log
            $ec = $LASTEXITCODE
            Pop-Location
            if ($ec -ne 0) {
                Write-Host "  $($f.Directory.FullName): FAIL" -ForegroundColor Red
                $anyfail = $true
            } else {
                Write-Host "  $($f.Directory.FullName): ok"
            }
        }
    }
    if (-not $found) {
        Write-Host "  SKIP (no rpcgen directives)" -ForegroundColor Yellow; Add-Result 'rpcgen' 'SKIP' $false; return
    }
    if (-not $anyfail) {
        Write-Host "  PASS" -ForegroundColor Green; Add-Result 'rpcgen' 'PASS' $false
    } else {
        Write-Host "  FAIL (see run/verify/rpcgen.log)" -ForegroundColor Red; Add-Result 'rpcgen' 'FAIL' $false
    }
}

# --- Slow stage: gremlins mutation testing ----------------------------------
function Invoke-GremlinsStage {
    $log = Join-Path $verifyDir 'gremlins.log'
    '' | Out-File $log
    Write-Host "== gremlins ==" -ForegroundColor Cyan
    if (-not (Ensure-Tool 'gremlins' 'github.com/go-gremlins/gremlins/cmd/gremlins@v0.6.0')) {
        Write-Host "  SKIP (gremlins unavailable)" -ForegroundColor Yellow; Add-Result 'gremlins' 'SKIP' $false; return
    }
    $anyfail = $false
    foreach ($p in @('edge', 'gateway', 'outbox', 'registry', 'bus')) {
        "--- ./$p/... ---" | Out-File -Append $log
        & gremlins unleash "./$p/..." *>> $log
        if ($LASTEXITCODE -ne 0) { $anyfail = $true }
    }
    if (-not $anyfail) {
        Write-Host "  PASS" -ForegroundColor Green; Add-Result 'gremlins' 'PASS' $false
    } else {
        Write-Host "  FAIL (see run/verify/gremlins.log)" -ForegroundColor Red; Add-Result 'gremlins' 'FAIL' $false
    }
}

# --- Run --------------------------------------------------------------------
Invoke-SimpleStage 'build'         $true 'go'            @('build', './...')
Invoke-SimpleStage 'vet'           $true 'go'            @('vet', './...')
Invoke-SimpleStage 'golangci-lint' $true 'golangci-lint' @('run', './...')
Invoke-SimpleStage 'go-arch-lint'  $true 'go-arch-lint'  @('check')
Invoke-SimpleStage 'test'          $true 'go'            @('test', './...')
Invoke-GovulncheckStage

if ($RunAdvisory) {
    Invoke-RaceStage
    Invoke-FuzzStage
    Invoke-ApidiffStage
    Invoke-TopiccheckStage
    Invoke-RpcgenStage
}
if ($RunGremlins) {
    Invoke-GremlinsStage
}

# --- Summary ----------------------------------------------------------------
Write-Host ""
Write-Host "=== verify summary ===" -ForegroundColor Cyan
Write-Host ('{0,-16} | {1,-6} | {2,-8}' -f 'Stage', 'Status', 'Blocking')
Write-Host ('{0,-16} | {1,-6} | {2,-8}' -f '----------------', '------', '--------')
$fail = $false
foreach ($r in $script:results) {
    $color = switch ($r.Status) { 'PASS' { 'Green' } 'FAIL' { 'Red' } default { 'Yellow' } }
    $blk = if ($r.Blocking) { 'true' } else { 'false' }
    Write-Host ('{0,-16} | {1,-6} | {2,-8}' -f $r.Name, $r.Status, $blk) -ForegroundColor $color
    if ($r.Status -eq 'FAIL' -and ($r.Blocking -or $StrictOn)) { $fail = $true }
}
Write-Host ""
if ($fail) {
    Write-Host "VERIFY FAILED" -ForegroundColor Red
    exit 1
} else {
    Write-Host "VERIFY OK" -ForegroundColor Green
    exit 0
}
