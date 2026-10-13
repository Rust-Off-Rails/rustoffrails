#!/usr/bin/env bash
# PostToolUse (Edit|Write|MultiEdit): cheap convention checks on the file just written.
# Exit 2 feeds the message back to the agent; it does not undo the edit.
source "$(dirname "$0")/lib.sh"
in_offrails_app || exit 0

file="$(rel_path "$(json_get .tool_input.file_path)")"
path="$PROJECT_DIR/$file"
[ -f "$path" ] || exit 0

problems=()

case "$file" in
  src/kernel/*|migration/*|tests/*) ;;  # raw SQL is allowed here
  src/*.rs|src/**/*.rs)
    if grep -nE 'Statement::from_(string|sql_and_values)|execute_unprepared|sqlx::query|query_as!?\(' "$path" >/dev/null; then
      problems+=("Raw SQL outside src/kernel/ (guides/database.md). Use SeaORM queries, or move it behind a backend match and log the escape hatch in docs/decisions.md.")
    fi
    if grep -nE '\.begin\(\)\.await' "$path" >/dev/null; then
      problems+=("Raw .begin() outside the kernel. Use kernel::db::begin(&db), which takes the SQLite write lock up front (BEGIN IMMEDIATE); a deferred transaction fails with 'database is locked' when the job worker writes concurrently (guides/database.md).")
    fi
    if grep -nE '(lazy_static|once_cell|async_trait|askama_axum|diesel|actix)' "$path" >/dev/null; then
      problems+=("Banned crate referenced (stack.md#banned).")
    fi
    if grep -nE '"/[^"]*:[a-z_]+' "$path" | grep -q 'route('; then
      problems+=("axum 0.8 path params are /{id}, not /:id (guides/routing.md).")
    fi
    ;;
esac

# Security tripwires: patterns that bypass escaping, CSP, CORS or cookie safety.
# Each one needs a docs/decisions.md entry and the human's explicit OK (guides/security.md).
case "$file" in
  templates/*.html|templates/**/*.html)
    if grep -nE '\|[[:space:]]*safe([^_a-z]|$)' "$path" >/dev/null; then
      problems+=("SECURITY: Askama '|safe' disables escaping. Only for HTML produced by kernel helpers that sanitize (ammonia), never for user input. Prefer a typed helper (guides/security.md).")
    fi
    if grep -nE '<script>[^<]|on(click|load|error|submit|change|input)=' "$path" >/dev/null; then
      problems+=("SECURITY: inline <script> or on* handler attribute. The CSP forbids inline script; move it to static/app.js (guides/security.md).")
    fi
    ;;
  web/src/*.tsx|web/src/**/*.tsx|web/src/*.jsx|web/src/**/*.jsx)
    if grep -n 'dangerouslySetInnerHTML' "$path" >/dev/null; then
      problems+=("SECURITY: dangerouslySetInnerHTML. Render text, or pass server-sanitized HTML from a ts-rs type named *Html (guides/security.md).")
    fi
    ;;
  web/src/*.svelte|web/src/**/*.svelte)
    if grep -n '{@html' "$path" >/dev/null; then
      problems+=("SECURITY: Svelte {@html} disables escaping. Only for server-sanitized HTML (guides/security.md).")
    fi
    ;;
esac

case "$file" in
  src/*.rs|src/**/*.rs)
    if grep -nE "unsafe-inline|unsafe-eval" "$path" >/dev/null; then
      problems+=("SECURITY: 'unsafe-inline'/'unsafe-eval' in a CSP. Use nonces or sha256 hashes (guides/security.md).")
    fi
    if grep -nE 'CorsLayer::(permissive|very_permissive)|AllowOrigin::any|Any\)' "$path" | grep -qi cors; then
      problems+=("SECURITY: permissive CORS. Use an explicit origin allowlist from config (guides/security.md).")
    fi
    if grep -nE '(format!|&format)\(.*(SELECT|INSERT|UPDATE|DELETE)' "$path" >/dev/null; then
      problems+=("SECURITY: SQL built with format!. Use SeaORM or bound parameters (guides/database.md).")
    fi
    if grep -nE 'set_(secure|http_only)\(false\)|\.secure\(false\)|\.http_only\(false\)' "$path" >/dev/null; then
      problems+=("SECURITY: a cookie with Secure/HttpOnly turned off (guides/security.md).")
    fi
    ;;
esac

case "$file" in
  tests/*.rs)
    if grep -nE '(tokio::time::sleep|thread::sleep)\(' "$path" >/dev/null; then
      problems+=("sleep in a test (guides/test-standards.md §7). Use drain_jobs, pass now: DateTimeUtc, or await the condition.")
    fi
    ;;
esac

case "$file" in
  web/src/*.ts|web/src/*.tsx|web/src/**/*.ts|web/src/**/*.tsx)
    if grep -nE "from ['\"](axios|ky|redux|zustand|mobx)" "$path" >/dev/null; then
      problems+=("Banned JS package imported (stack.md#banned).")
    fi
    ;;
esac

if [ ${#problems[@]} -gt 0 ]; then
  {
    echo "Rust off Rails convention check on $file:"
    printf ' - %s\n' "${problems[@]}"
  } >&2
  exit 2
fi
exit 0
