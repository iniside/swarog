#!/usr/bin/env bash
# memory-sync.sh — keep Claude Code's project memory in sync with git.
#
# The live memory lives OUTSIDE the repo, under the user's home:
#   $HOME/.claude/projects/<mangled-repo-path>/memory/
# where <mangled-repo-path> is the repo's absolute path with every
# non-alphanumeric character replaced by '-' (Claude Code's own scheme).
# That path differs per machine, so we derive it instead of hardcoding.
#
# The in-repo mirror is  <repo>/memory/  — that is what git tracks.
#
# Subcommands:
#   push [--push]   live memory -> repo/memory, then git add + commit
#                   (pass --push to also `git push`)
#   pull            repo/memory -> live memory (run after a git pull)
#   path            print the resolved live memory dir and exit
#
# Override the live dir with CLAUDE_MEMORY_DIR if auto-detection is wrong.
set -euo pipefail

REPO="$(git rev-parse --show-toplevel)"
MIRROR="$REPO/memory"

resolve_live() {
  if [ -n "${CLAUDE_MEMORY_DIR:-}" ]; then
    echo "$CLAUDE_MEMORY_DIR"
    return
  fi
  local mangled
  mangled="$(printf '%s' "$REPO" | sed 's/[^A-Za-z0-9]/-/g')"
  echo "$HOME/.claude/projects/$mangled/memory"
}

LIVE="$(resolve_live)"

# mirror_dir SRC DST — make DST an exact copy of SRC's *.md files.
mirror_dir() {
  local src="$1" dst="$2"
  [ -d "$src" ] || { echo "source memory dir not found: $src" >&2; exit 1; }
  mkdir -p "$dst"
  # drop files present in dst but gone from src
  find "$dst" -maxdepth 1 -type f -name '*.md' -exec rm -f {} +
  # copy everything (md files); -n false so we overwrite
  find "$src" -maxdepth 1 -type f -name '*.md' -exec cp -f {} "$dst"/ \;
}

cmd="${1:-}"
case "$cmd" in
  path)
    echo "$LIVE"
    ;;
  push)
    mirror_dir "$LIVE" "$MIRROR"
    git -C "$REPO" add memory
    if git -C "$REPO" diff --cached --quiet -- memory; then
      echo "memory: no changes to commit"
    else
      git -C "$REPO" commit -m "chore(memory): sync agent memory backup" >/dev/null
      echo "memory: committed backup"
    fi
    if [ "${2:-}" = "--push" ]; then
      git -C "$REPO" push
      echo "memory: pushed"
    fi
    ;;
  pull)
    mirror_dir "$MIRROR" "$LIVE"
    echo "memory: restored $MIRROR -> $LIVE"
    ;;
  *)
    echo "usage: $0 {push [--push]|pull|path}" >&2
    exit 2
    ;;
esac
