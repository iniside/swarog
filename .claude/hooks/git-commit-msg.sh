#!/bin/sh
if command -v python >/dev/null 2>&1; then
    py=python
elif command -v python3 >/dev/null 2>&1; then
    py=python3
else
    echo "commit-msg: python not found, commit subject format not checked" >&2
    exit 0
fi
"$py" "$(git rev-parse --show-toplevel)/.claude/hooks/commit_format.py" "$1"
