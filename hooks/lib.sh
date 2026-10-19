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

# Guard hooks (guard-edits.sh, guard-bash.sh) call this: a sed-parsed command or path can be
# cut at the first , } or " and so smuggle a blocked command past the guard, so without jq
# they fail closed. Non-guard hooks keep the sed fallback above.
require_jq() {
  command -v jq >/dev/null 2>&1 && return 0
  echo "Blocked: the Off Rails guard hooks require jq to parse tool input safely, and jq is not installed. Install jq (e.g. brew install jq) and retry." >&2
  exit 2
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

# Lexically resolve . and .. and make the path absolute (against the hook's cwd).
lex_path() {
  local p="$1" base out="" seg
  case "$p" in
    /*) ;;
    *) base="$(json_get .cwd)"; p="${base:-$PWD}/$p" ;;
  esac
  local IFS=/
  local -a parts=() stack=()
  read -r -a parts <<<"$p"
  for seg in "${parts[@]}"; do
    case "$seg" in
      ""|.) ;;
      ..) [ "${#stack[@]}" -gt 0 ] && unset 'stack[${#stack[@]}-1]' ;;
      *) stack+=("$seg") ;;
    esac
  done
  for seg in "${stack[@]}"; do out="$out/$seg"; done
  printf '%s' "${out:-/}"
}

# Absolute, normalized path with symlinks resolved in the deepest existing ancestor.
canon_path() {
  local p tail="" d
  p="$(lex_path "$1")"
  d="$p"
  while [ "$d" != "/" ] && [ ! -d "$d" ]; do tail="/${d##*/}$tail"; d="${d%/*}"; [ -n "$d" ] || d="/"; done
  d="$(cd -P "$d" 2>/dev/null && pwd -P)" || d="${p%"$tail"}"
  [ "$d" = "/" ] && d=""
  printf '%s' "$d$tail"
}

# Path of $1 relative to the real project root (absolute canonical path if outside it).
rel_path_canon() {
  local root p
  root="$(cd -P "$PROJECT_DIR" 2>/dev/null && pwd -P)" || root="$PROJECT_DIR"
  p="$(canon_path "$1")"
  case "$p" in
    "$root"/*) printf '%s' "${p#"$root"/}" ;;
    *) printf '%s' "$p" ;;
  esac
}
