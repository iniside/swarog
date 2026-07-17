# db-provision.ps1 -- seed script that provisions the dev Postgres role + database
# this repo assumes but never creates (CLAUDE.md: "the answer is a seed script
# minting fake data ... not a migration"). Idempotent: safe to run repeatedly.
# Paired with db-provision.sh (bash/macOS/Linux). PowerShell 5.1 compatible.
#
# Creates ONLY:
#   - login role  gamebackend / gamebackend
#   - database    gamebackend  owned by that role
# It does NOT create module schemas or the asyncevents plane -- those are owned
# by each module's `migrate` step and the app-owned event plane, run at process
# boot (devctl up / cargo test / cargo run -p <svc>).
#
# Connects as a Postgres SUPERUSER (default: the current OS user, against the
# `postgres` maintenance database) to do the provisioning -- NOT as `gamebackend`,
# which does not exist yet on a fresh machine. Override with PGSUPERUSER /
# PGSUPERDB / PGSUPERHOST / PGSUPERPORT if the local superuser differs.
#
# Target role/db/password/host/port are read from $env:DATABASE_URL if set,
# else fall back to this repo's documented default:
#   postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable
#
# Usage: .\scripts\db-provision.ps1

$ErrorActionPreference = 'Stop'

# ---- locate psql ------------------------------------------------------------

function Find-Psql {
    $cmd = Get-Command psql -ErrorAction SilentlyContinue
    if ($cmd) {
        return $cmd.Source
    }
    $candidates = @(
        "$env:ProgramFiles\PostgreSQL\18\bin\psql.exe",
        "$env:ProgramFiles\PostgreSQL\17\bin\psql.exe",
        "$env:ProgramFiles\PostgreSQL\16\bin\psql.exe",
        "C:\Program Files\PostgreSQL\18\bin\psql.exe",
        "C:\Program Files\PostgreSQL\17\bin\psql.exe",
        "C:\Program Files\PostgreSQL\16\bin\psql.exe"
    )
    foreach ($c in $candidates) {
        if (Test-Path -LiteralPath $c) {
            return $c
        }
    }
    Write-Host "error: psql not found on PATH or in known install locations"
    Write-Host "  install PostgreSQL or add psql.exe's directory to PATH"
    exit 1
}

$Psql = Find-Psql

# ---- parse DATABASE_URL (or fall back to the repo default) -----------------
# Expected form: postgres://USER:PASS@HOST:PORT/DBNAME?params

$Dsn = $env:DATABASE_URL
if ([string]::IsNullOrEmpty($Dsn)) {
    $Dsn = 'postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable'
}

function Parse-Dsn {
    param([string]$Dsn)
    $rest = $Dsn -replace '^[a-zA-Z][a-zA-Z0-9+.-]*://', ''   # strip scheme://
    $atIdx = $rest.IndexOf('@')
    $userpass = $rest.Substring(0, $atIdx)
    $rest = $rest.Substring($atIdx + 1)                       # HOST:PORT/DBNAME?params
    $slashIdx = $rest.IndexOf('/')
    $hostport = $rest.Substring(0, $slashIdx)
    $rest = $rest.Substring($slashIdx + 1)                    # DBNAME?params
    $qIdx = $rest.IndexOf('?')
    if ($qIdx -ge 0) { $dbname = $rest.Substring(0, $qIdx) } else { $dbname = $rest }

    $colonIdx = $userpass.IndexOf(':')
    if ($colonIdx -ge 0) {
        $user = $userpass.Substring(0, $colonIdx)
        $pass = $userpass.Substring($colonIdx + 1)
    } else {
        $user = $userpass
        $pass = ''
    }
    $hpColonIdx = $hostport.IndexOf(':')
    if ($hpColonIdx -ge 0) {
        $dbhost = $hostport.Substring(0, $hpColonIdx)
        $dbport = $hostport.Substring($hpColonIdx + 1)
    } else {
        $dbhost = $hostport
        $dbport = ''
    }

    if ([string]::IsNullOrEmpty($dbhost)) { $dbhost = 'localhost' }
    if ([string]::IsNullOrEmpty($dbport)) { $dbport = '5432' }
    if ([string]::IsNullOrEmpty($user))   { $user = 'gamebackend' }
    if ([string]::IsNullOrEmpty($pass))   { $pass = 'gamebackend' }
    if ([string]::IsNullOrEmpty($dbname)) { $dbname = 'gamebackend' }

    return @{ User = $user; Pass = $pass; Host = $dbhost; Port = $dbport; Name = $dbname }
}

$parsed = Parse-Dsn -Dsn $Dsn
$DbUser = $parsed.User
$DbPass = $parsed.Pass
$DbHost = $parsed.Host
$DbPort = $parsed.Port
$DbName = $parsed.Name

# ---- superuser connection used to provision ---------------------------------
# Default: current OS user (a Postgres superuser on this machine), against the
# `postgres` maintenance database on the same host/port as the target DSN.

$SuperUser = $env:PGSUPERUSER
if ([string]::IsNullOrEmpty($SuperUser)) { $SuperUser = [Environment]::UserName }
$SuperDb = $env:PGSUPERDB
if ([string]::IsNullOrEmpty($SuperDb)) { $SuperDb = 'postgres' }
$SuperHost = $env:PGSUPERHOST
if ([string]::IsNullOrEmpty($SuperHost)) { $SuperHost = $DbHost }
$SuperPort = $env:PGSUPERPORT
if ([string]::IsNullOrEmpty($SuperPort)) { $SuperPort = $DbPort }

function Invoke-PsqlSuper {
    param([string[]]$PsqlArgs)
    $baseArgs = @('-v', 'ON_ERROR_STOP=1', '-X', '-q', '-U', $SuperUser, '-h', $SuperHost, '-p', $SuperPort, '-d', $SuperDb)
    & $Psql @baseArgs @PsqlArgs
    if ($LASTEXITCODE -ne 0) {
        throw "psql failed with exit code $LASTEXITCODE"
    }
}

function Invoke-PsqlSuperCapture {
    param([string[]]$PsqlArgs)
    $baseArgs = @('-v', 'ON_ERROR_STOP=1', '-X', '-q', '-t', '-A', '-U', $SuperUser, '-h', $SuperHost, '-p', $SuperPort, '-d', $SuperDb)
    $out = & $Psql @baseArgs @PsqlArgs
    if ($LASTEXITCODE -ne 0) {
        throw "psql failed with exit code $LASTEXITCODE"
    }
    return ($out -join "`n").Trim()
}

Write-Host "db-provision: using psql at $Psql"
Write-Host "db-provision: connecting as superuser '$SuperUser' to $($SuperHost):$($SuperPort)/$SuperDb"
Write-Host "db-provision: target role/database '$DbUser'/'$DbName'"

# ---- role: create only if absent --------------------------------------------
# The password is embedded as a SQL string literal (doubled single quotes
# escape a literal quote per SQL syntax -- not a general injection defense,
# but this is a dev tool for a trusted local operator per CLAUDE.md's
# dev-tooling scope, and the value is this script's own DSN-derived password,
# never external input). `-v name=value` variable interpolation with `-c` is
# unreliable across psql builds, so the literal is built directly instead of
# via `:varname`.

$dbPassLiteral = $DbPass.Replace("'", "''")

$roleExists = Invoke-PsqlSuperCapture -PsqlArgs @('-c', "SELECT 1 FROM pg_roles WHERE rolname = '$DbUser'")
if ($roleExists -eq '1') {
    # "provisioned" means the role exists WITH the DSN's password, not merely
    # that the row exists -- an existing role whose password has drifted from
    # the DSN must still be re-idempotent. ALTER ROLE converges both LOGIN (in
    # case an existing role lacks it) and PASSWORD unconditionally; this is
    # cheap and always correct to re-apply.
    Invoke-PsqlSuper -PsqlArgs @('-c', "ALTER ROLE ""$DbUser"" LOGIN PASSWORD '$dbPassLiteral'")
    Write-Host "db-provision: role '$DbUser' already exists -- converged password"
} else {
    Invoke-PsqlSuper -PsqlArgs @('-c', "CREATE ROLE ""$DbUser"" LOGIN PASSWORD '$dbPassLiteral'")
    Write-Host "db-provision: created role '$DbUser'"
}

# ---- database: create only if absent ----------------------------------------
# CREATE DATABASE cannot run inside a transaction block or a DO $$ ... $$
# block, so check first from the shell and issue it directly, conditionally.

$dbExists = Invoke-PsqlSuperCapture -PsqlArgs @('-c', "SELECT 1 FROM pg_database WHERE datname = '$DbName'")
if ($dbExists -eq '1') {
    Write-Host "db-provision: database '$DbName' already exists -- skipping create"
} else {
    Invoke-PsqlSuper -PsqlArgs @('-c', "CREATE DATABASE ""$DbName"" OWNER ""$DbUser""")
    Write-Host "db-provision: created database '$DbName' owned by '$DbUser'"
}

# ---- grants ------------------------------------------------------------------
# The role owns the database (via CREATE DATABASE ... OWNER above), which
# already lets it CREATE SCHEMA inside its own database on modern Postgres
# (schema-per-module + the asyncevents plane both create schemas at boot).
# Explicitly (re-)grant CONNECT + CREATE on the database for idempotent
# clarity, and ensure it owns the default `public` schema too. No SUPERUSER,
# no elevated role membership -- ownership of its own database is sufficient.

Invoke-PsqlSuper -PsqlArgs @('-c', "GRANT CONNECT, CREATE ON DATABASE ""$DbName"" TO ""$DbUser""")
try {
    $baseArgs = @('-v', 'ON_ERROR_STOP=1', '-X', '-q', '-U', $SuperUser, '-h', $SuperHost, '-p', $SuperPort, '-d', $DbName)
    & $Psql @baseArgs -c "ALTER SCHEMA public OWNER TO ""$DbUser"""
} catch {
    # non-fatal: some Postgres versions/policies may reject this; ownership
    # via CREATE DATABASE ... OWNER already covers schema creation rights.
}

Write-Host "db-provision: done. Verify with:"
Write-Host "  `$env:PGPASSWORD='$DbPass'; & '$Psql' -U $DbUser -h $DbHost -p $DbPort -d $DbName -c 'select current_user, current_database()'"
