# install.ps1 -- create (or password-reset) a GameOps admin login for the hardened admin
# portal. Thin no-echo-prompt wrapper over `adminctl create-user` (tools/adminctl): the
# CLI never accepts a password on argv, so this script prompts for it silently (or reads
# ADMINCTL_PASSWORD) and pipes it in over stdin. Paired with install.sh (bash).
# PowerShell 5.1 compatible: ASCII only, no em-dashes.
#
# Usage:
#   .\install.ps1 <username>          # prompt for the password (no echo)
#   $env:ADMINCTL_PASSWORD='...'; .\install.ps1 <username>   # non-interactive
#
# Connection: DATABASE_URL (default local dev DSN, same as the services). The admin
# schema is created on the fly if this is a fresh database.
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [string]$Username
)

$ErrorActionPreference = 'Stop'
Set-Location -Path $PSScriptRoot

if ([string]::IsNullOrWhiteSpace($Username)) {
    Write-Host "usage: .\install.ps1 <username>"
    Write-Host "  creates (or resets the password of) an admin portal login."
    exit 1
}

# Password: ADMINCTL_PASSWORD wins (non-interactive); otherwise prompt twice, no echo.
if (-not [string]::IsNullOrEmpty($env:ADMINCTL_PASSWORD)) {
    $Password = $env:ADMINCTL_PASSWORD
} else {
    $secure = Read-Host -AsSecureString -Prompt "Password for admin user '$Username'"
    $confirm = Read-Host -AsSecureString -Prompt "Confirm password"
    $bstr1 = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
    $bstr2 = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($confirm)
    try {
        $Password = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr1)
        $confirmPlain = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr2)
    } finally {
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr1)
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr2)
    }
    if ($Password -ne $confirmPlain) {
        Write-Host "error: passwords do not match"
        exit 1
    }
}
if ([string]::IsNullOrEmpty($Password)) {
    Write-Host "error: password must not be empty"
    exit 1
}

# Pipe the password over stdin -- never on argv, never in the process table.
$Password | cargo run -q -p adminctl -- create-user $Username --password-stdin
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

Write-Host ""
Write-Host "Admin user '$Username' is ready. Next steps for a public deployment:"
Write-Host "  - TLS_MODE=acme ACME_DOMAINS=admin.example.com ACME_CONTACT=you@example.com"
Write-Host "      (or TLS_MODE=files TLS_CERT_PATH=... TLS_KEY_PATH=...) on cmd/gateway-svc."
Write-Host "  - ADMIN_HTTP_ADDR=0.0.0.0:8085   (bind the admin process publicly / behind the proxy)."
Write-Host "  - TRUSTED_PROXY_CIDRS=<proxy-hop-cidr>   so the login lockout sees the real client IP,"
Write-Host "      not the gateway. Required whenever admin runs behind a reverse proxy."
Write-Host "  - Log in at https://<your-domain>/admin with the credentials you just set."
