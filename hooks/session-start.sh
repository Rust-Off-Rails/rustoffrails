#!/usr/bin/env bash
# SessionStart: orient the agent. stdout becomes session context.
source "$(dirname "$0")/lib.sh"
in_offrails_app || exit 0

root="$(cd "$(dirname "$0")/.." && pwd)"
plugin_version="$(head -n1 "$root/VERSION" 2>/dev/null)"
app_version="$(head -n1 "$PROJECT_DIR/.offrails/VERSION" 2>/dev/null)"

echo "This is a Rust off Rails app. Before your first change, read AGENTS.md and follow .offrails/AGENTS.md (load order, FRONTEND_MODE, Definition of Done). The human writes no code: specs live in product/."

# Compare content, not just version numbers: docs can change without a bump.
plugin_fp="$(bash "$root/hooks/fingerprint.sh" "$root" 2>/dev/null)"
app_fp="$(bash "$root/hooks/fingerprint.sh" "$PROJECT_DIR/.offrails" 2>/dev/null)"
if [ -n "$plugin_fp" ] && [ -n "$app_fp" ] && [ "$plugin_fp" != "$app_fp" ]; then
  if [ "$plugin_version" != "$app_version" ]; then
    echo "Framework drift: the app vendors .offrails/ $app_version and the installed plugin is $plugin_version."
  else
    echo "Framework drift: .offrails/ and the installed plugin are both $app_version, but their docs differ (content $app_fp vs $plugin_fp)."
  fi
  echo "Mention once to the human that /offrails:upgrade can bring the app up to date. Never upgrade unprompted."
fi
exit 0
