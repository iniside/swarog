# run-dev.ps1 - load .env.local and start the backend.
# Serves the account-linking web UI at http://localhost:8080
#
# Usage:  .\run-dev.ps1
# Secrets live in .env.local (gitignored); this script never contains them.

$ErrorActionPreference = "Stop"

$envFile = Join-Path $PSScriptRoot ".env.local"
if (Test-Path $envFile) {
    Get-Content $envFile | ForEach-Object {
        $line = $_.Trim()
        if ($line -and -not $line.StartsWith("#")) {
            $kv = $line -split "=", 2
            if ($kv.Length -eq 2) {
                [Environment]::SetEnvironmentVariable($kv[0].Trim(), $kv[1].Trim())
            }
        }
    }
    Write-Host "Loaded .env.local" -ForegroundColor Green
    if ($env:EPIC_CLIENT_ID) {
        Write-Host ("Epic OAuth: client {0}, redirect {1}" -f $env:EPIC_CLIENT_ID, $env:EPIC_REDIRECT_URI) -ForegroundColor Cyan
    }
} else {
    Write-Host "No .env.local found - running without Epic OAuth" -ForegroundColor Yellow
}

Write-Host "Open http://localhost:8080" -ForegroundColor Green
go run ./cmd/server
