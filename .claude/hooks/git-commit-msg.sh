#!/bin/sh
if ! command -v python >/dev/null 2>&1; then
    echo "commit-msg: python not found, commit subject format not checked" >&2
    exit 0
fi
python "$(git rev-parse --show-toplevel)/.claude/hooks/commit_format.py" "$1"
