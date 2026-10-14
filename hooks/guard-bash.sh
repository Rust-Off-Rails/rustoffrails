#!/usr/bin/env bash
# PreToolUse (Bash): block process-killing by name or pattern. On a shared dev machine,
# `pkill -f "myapp serve"` also kills other sessions' apps with the same name.
source "$(dirname "$0")/lib.sh"
in_offrails_app || exit 0

cmd="$(json_get .tool_input.command)"
if printf '%s' "$cmd" | grep -qE '(^|[;&|[:space:]])(pkill|killall)([[:space:]]|$)'; then
  echo "Blocked: never kill processes by name or pattern (pkill/killall); it can kill other sessions' servers. Stop the PID file you started: kill \"\$(cat storage/pids/dev-\$PORT.pid)\" for bin/dev (it stops its whole tree), storage/pids/serve-\$PORT.pid for a plain serve (cli.md, 'Running the app from an agent')." >&2
  exit 2
fi
exit 0
