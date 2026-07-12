# Temporary argument-preserving forwarder to verifyctl.
Set-Location -LiteralPath $PSScriptRoot
& cargo run -q -p verifyctl -- @args
exit $LASTEXITCODE
