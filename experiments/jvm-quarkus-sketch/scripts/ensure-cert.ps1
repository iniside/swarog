<#
.SYNOPSIS
    Idempotently ensures a self-signed TLS certificate exists for the edge QUIC
    server (characters' "characters.ownerOf" listener in split-microservices mode).

.DESCRIPTION
    msquic/schannel on Windows needs the cert to live in the Windows certificate
    store (CERTIFICATE_HASH), not a PEM file on disk, so this looks up a cert by
    FriendlyName "GameBackend-Edge" in cert:\CurrentUser\My and reuses it if
    present; otherwise it creates one.

    NOTE (same-user caveat): the private key is only reachable when the JVM
    process runs as the SAME Windows user that created the cert — CurrentUser
    store, not LocalMachine. This is fine for local dev / same-user split
    processes, but would need a different store (or a service account) for a
    real multi-user deployment.

.OUTPUTS
    Prints ONLY the cert thumbprint to stdout as the last line, so a caller can
    capture it with:
        $tp = (& ./scripts/ensure-cert.ps1)
    All status/progress messages go to Write-Host (stderr-ish, human-readable),
    never stdout, so they don't pollute the captured value.
#>

$ErrorActionPreference = 'Stop'

$friendlyName = 'GameBackend-Edge'
$storeLocation = 'cert:\CurrentUser\My'

$existing = Get-ChildItem -Path $storeLocation |
    Where-Object { $_.FriendlyName -eq $friendlyName } |
    Select-Object -First 1

if ($existing) {
    Write-Host "Reusing existing certificate '$friendlyName' (thumbprint $($existing.Thumbprint))"
    $cert = $existing
} else {
    Write-Host "No certificate named '$friendlyName' found in $storeLocation; creating one..."
    $cert = New-SelfSignedCertificate `
        -DnsName localhost `
        -FriendlyName $friendlyName `
        -CertStoreLocation $storeLocation `
        -KeyUsage DigitalSignature `
        -KeyUsageProperty Sign `
        -HashAlgorithm SHA256
    Write-Host "Created certificate '$friendlyName' (thumbprint $($cert.Thumbprint))"
}

$cert.Thumbprint
