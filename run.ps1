# Temporary argument-preserving forwarder to devctl.
Set-Location -LiteralPath $PSScriptRoot
& cargo run -q -p devctl -- @args
exit $LASTEXITCODE
