#!/usr/bin/env bash
# PreToolUse (Bash): block process-killing by name or pattern. On a shared dev machine,
# `pkill -f "myapp serve"` also kills other sessions' apps with the same name.
source "$(dirname "$0")/lib.sh"
in_offrails_app || exit 0
require_jq

# NOTE: this is a guardrail against accidents, not a security boundary. A determined caller
# can always evade a regex (eval, base64, scripts). It catches the usual honest forms.
cmd="$(json_get .tool_input.command)"
# Strip backslashes (p\kill) and quotes so quoted/escaped names still tokenize as words.
flat="$(printf '%s' "$cmd" | tr -d '\\"'"'"'')"
# pkill/killall as a word token anywhere (optionally behind a path: /usr/bin/pkill);
# `kill ... $(pgrep` / backtick pgrep; `pgrep ... | xargs kill`.
tok='(^|[^A-Za-z0-9_.-])'
if printf '%s' "$flat" | grep -qE "${tok}([^[:space:]]*/)?(pkill|killall)([^A-Za-z0-9_.-]|$)" \
  || printf '%s' "$flat" | grep -qE "${tok}kill[[:space:]].*(\\$\\(|\`)[[:space:]]*pgrep" \
  || printf '%s' "$flat" | grep -qE "${tok}(pgrep|ps)[[:space:]].*\\|[[:space:]]*xargs[[:space:]]+(-[^[:space:]]+[[:space:]]+)*kill"; then
  echo "Blocked: never kill processes by name or pattern (pkill/killall); it can kill other sessions' servers. Stop the PID file you started: kill \"\$(cat storage/pids/dev-\$PORT.pid)\" for bin/dev (it stops its whole tree), storage/pids/serve-\$PORT.pid for a plain serve (cli.md, 'Running the app from an agent')." >&2
  exit 2
fi
exit 0
