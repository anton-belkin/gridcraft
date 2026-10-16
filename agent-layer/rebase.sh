#!/usr/bin/env bash
# Rebase the agent-layer fork onto upstream/main and prove it still works.
# Usage: agent-layer/rebase.sh [upstream-branch]   (default: main)
#        agent-layer/rebase.sh --check             (only the checks, e.g. after resolving a conflict)
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
check_only=false
if [ "${1:-}" = "--check" ]; then check_only=true; shift; fi
branch="${1:-main}"

if ! $check_only; then
if [ -n "$(git status --porcelain)" ]; then
  echo "Working tree not clean: commit or stash first." >&2
  exit 1
fi

before=$(git grep -c "fork:agent" -- ':!FORK.md' ':!agent-layer' | awk -F: '{s+=$2} END {print s+0}')
git fetch upstream
echo "Rebasing $(git rev-parse --abbrev-ref HEAD) onto upstream/$branch…"
# Cargo.lock conflicts on nearly every rebase (upstream bumps dependencies): take upstream's lock
# and let cargo add our two crates to it, without updating anything else.
resolve_lock() {
  local files
  files=$(git diff --name-only --diff-filter=U)
  [ "$files" = "Cargo.lock" ] || return 1
  git checkout --ours Cargo.lock   # during a rebase, "ours" is upstream
  cargo metadata --format-version 1 >/dev/null
  git add Cargo.lock
  GIT_EDITOR=true git rebase --continue || true   # a later commit may stop again: the loop sees it
}
rebased=true
git rebase "upstream/$branch" || rebased=false
while ! $rebased; do
  if resolve_lock; then
    rebased=true
    [ -d "$(git rev-parse --git-path rebase-merge)" ] && rebased=false
  else
    break
  fi
done
if ! $rebased; then
  cat >&2 <<'MSG'

Conflict. It can only be in a line marked `fork:agent` (see FORK.md, "The patch to upstream files").
Keep upstream's version of the surrounding code, put the marked line back, then:
  git add <file> && git rebase --continue && agent-layer/rebase.sh --check
MSG
  exit 1
fi
fi

after=$(git grep -c "fork:agent" -- ':!FORK.md' ':!agent-layer' | awk -F: '{s+=$2} END {print s+0}')
if ! $check_only && [ "$after" -lt "$before" ]; then
  echo "Only $after of $before fork:agent markers left after the rebase: a hook line was lost." >&2
  exit 1
fi

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p gridcraft-agent -p gridcraft-agent-ui -p gridcraft-engine -p gridcraft-mcp
cargo xtask layers
cargo xtask wasm
cargo build --release -p gridcraft
echo "Rebased and checked: $after fork:agent markers, all gates green."
