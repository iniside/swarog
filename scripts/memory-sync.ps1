# memory-sync.ps1 — keep Claude Code's project memory in sync with git.
#
# The live memory lives OUTSIDE the repo, under the user's home:
#   $HOME/.claude/projects/<mangled-repo-path>/memory/
# where <mangled-repo-path> is the repo's absolute path with every
# non-alphanumeric character replaced by '-' (Claude Code's own scheme).
# That path differs per machine, so we derive it instead of hardcoding.
#
# The in-repo mirror is  <repo>/memory/  — that is what git tracks.
#
# Usage:
#   ./scripts/memory-sync.ps1 push [-Push]   live -> repo/memory, git add + commit
#   ./scripts/memory-sync.ps1 pull           repo/memory -> live (after git pull)
#   ./scripts/memory-sync.ps1 path           print resolved live memory dir
#
# Override the live dir with $env:CLAUDE_MEMORY_DIR if detection is wrong.
param(
    [Parameter(Mandatory = $true)][ValidateSet('push', 'pull', 'path')][string]$Command,
    [switch]$Push
)
$ErrorActionPreference = 'Stop'

$Repo = (git rev-parse --show-toplevel).Trim()
$Mirror = Join-Path $Repo 'memory'

function Resolve-Live {
    if ($env:CLAUDE_MEMORY_DIR) { return $env:CLAUDE_MEMORY_DIR }
    $mangled = ($Repo -replace '[^A-Za-z0-9]', '-')
    return (Join-Path $HOME ".claude/projects/$mangled/memory")
}
$Live = Resolve-Live

function Sync-Dir([string]$Src, [string]$Dst) {
    if (-not (Test-Path $Src)) { throw "source memory dir not found: $Src" }
    New-Item -ItemType Directory -Force -Path $Dst | Out-Null
    Get-ChildItem -Path $Dst -Filter *.md -File | Remove-Item -Force
    Get-ChildItem -Path $Src -Filter *.md -File | Copy-Item -Destination $Dst -Force
}

switch ($Command) {
    'path' { Write-Output $Live }
    'push' {
        Sync-Dir $Live $Mirror
        git -C $Repo add memory
        git -C $Repo diff --cached --quiet -- memory
        if ($LASTEXITCODE -ne 0) {
            git -C $Repo commit -m 'chore(memory): sync agent memory backup' | Out-Null
            Write-Output 'memory: committed backup'
        }
        else { Write-Output 'memory: no changes to commit' }
        if ($Push) { git -C $Repo push; Write-Output 'memory: pushed' }
    }
    'pull' {
        Sync-Dir $Mirror $Live
        Write-Output "memory: restored $Mirror -> $Live"
    }
}
