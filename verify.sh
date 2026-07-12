#!/usr/bin/env bash
# Temporary argument-preserving forwarder to verifyctl.
set -euo pipefail
cd "$(dirname "$0")"
exec cargo run -q -p verifyctl -- "$@"
