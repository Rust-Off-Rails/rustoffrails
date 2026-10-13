#!/usr/bin/env bash
# Stop: don't let the agent end its turn with a Rust tree that doesn't format or compile.
# This is a cheap floor (fmt + check), not the full Definition of Done, which the skills run.
source "$(dirname "$0")/lib.sh"
in_offrails_app || exit 0
[ -f "$PROJECT_DIR/Cargo.toml" ] || exit 0

# Already blocked once this turn: let it stop, so we never loop.
[ "$(json_get .stop_hook_active)" = "true" ] && exit 0

cd "$PROJECT_DIR" || exit 0

# Only gate when Rust files changed since the last commit.
if git rev-parse --git-dir >/dev/null 2>&1; then
  git status --porcelain -- '*.rs' 'Cargo.toml' 'migration/' | grep -q . || exit 0
fi

if ! out="$(cargo fmt --all --check 2>&1)"; then
  echo "Stop gate: cargo fmt --check fails. Run cargo fmt before finishing." >&2
  exit 2
fi

if ! out="$(cargo check --all-targets --quiet 2>&1)"; then
  {
    echo "Stop gate: cargo check fails. Never end a turn with the tree red (workflow.md). Fix these, or tell the human exactly why you can't:"
    printf '%s\n' "$out" | grep -E '^(error|warning: unused)' -A6 | head -60
  } >&2
  exit 2
fi
exit 0
