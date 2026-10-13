#!/usr/bin/env bash
# Shared helpers for Rust off Rails hooks. Hooks get JSON on stdin.
# They are no-ops outside an Off Rails app (no .offrails/ in the project dir).

INPUT="$(cat)"

json_get() { # json_get <jq path>, e.g. json_get .tool_input.file_path
  if command -v jq >/dev/null 2>&1; then
    printf '%s' "$INPUT" | jq -r "$1 // empty"
  else
    local key="${1##*.}"
    printf '%s' "$INPUT" | sed -n "s/.*\"$key\" *: *\"\{0,1\}\([^\",}]*\)\"\{0,1\}.*/\1/p" | head -n1
  fi
}

PROJECT_DIR="${CLAUDE_PROJECT_DIR:-$(json_get .cwd)}"
PROJECT_DIR="${PROJECT_DIR:-$PWD}"

in_offrails_app() { [ -d "$PROJECT_DIR/.offrails" ]; }

# Path of $1 relative to the project dir (unchanged if outside it).
rel_path() {
  case "$1" in
    "$PROJECT_DIR"/*) printf '%s' "${1#"$PROJECT_DIR"/}" ;;
    *) printf '%s' "$1" ;;
  esac
}
