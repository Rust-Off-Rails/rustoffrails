#!/usr/bin/env bash
# Optional real-browser smoke for the kernel UIs (/dev, /dev/mailbox, /admin/jobs), for when the
# chrome-devtools MCP can't attach (profile lock) or in a script. Needs Node + an installed Chrome
# on the dev machine only: puppeteer-core and axe-core go to a cache dir, nothing is added to the
# app (htmx apps stay Node-free). Uses a throwaway Chrome profile and NO certificate overrides:
# an untrusted HTTPS certificate fails every smoke (Chrome drops cookies on it, so login bounces).
#
#   BASE=<the URL bin/dev printed> bash .offrails/tools/browser-smoke/run.sh             # kernel pages
#   SCENARIO=tests/e2e/smoke.scenario.mjs BASE=… bash .offrails/tools/browser-smoke/run.sh login   # session sticks
#   SCENARIO=tests/e2e/smoke.scenario.mjs BASE=… bash .offrails/tools/browser-smoke/run.sh a11y    # axe, light + dark
#   SCENARIO=tests/e2e/live-tabs.scenario.mjs BASE=https://localhost:3000 TABS=12 \
#     bash .offrails/tools/browser-smoke/run.sh live-tabs                                 # N tabs, one change
#   CHROME=/usr/bin/google-chrome BASE=… bash .offrails/tools/browser-smoke/run.sh   # Linux
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
cache="${XDG_CACHE_HOME:-$HOME/.cache}/offrails/browser-smoke"
version="25.13.0"      # puppeteer-core, verified 2026-10-12
axe_version="4.13.0"   # axe-core, verified 2026-10-13
node_major="$(node -p 'process.versions.node.split(".")[0]' 2>/dev/null || echo 0)"
if [ "$node_major" -lt 22 ]; then
  echo "browser smoke needs Node >= 22.12 (Node 24 LTS recommended); found $(node --version 2>/dev/null || echo none)" >&2
  exit 1
fi
mkdir -p "$cache"
if [ "$(cat "$cache/.version" 2>/dev/null)" != "$version+$axe_version" ]; then
  echo "installing puppeteer-core@$version and axe-core@$axe_version into $cache (once)…"
  npm install --prefix "$cache" --no-audit --no-fund --silent "puppeteer-core@$version" "axe-core@$axe_version"
  echo "$version+$axe_version" > "$cache/.version"
fi
# ESM resolves imports next to the script, so run a copy from the cache dir.
case "${1:-dev-tools}" in
  dev-tools) script=smoke-dev-tools.mjs ;;
  live-tabs) script=smoke-live-tabs.mjs ;;
  login) script=smoke-login.mjs ;;
  a11y) script=smoke-a11y.mjs ;;
  *) echo "unknown smoke '$1' (dev-tools | live-tabs | login | a11y)" >&2; exit 2 ;;
esac
cp "$here/lib.mjs" "$here/$script" "$cache/"
exec node "$cache/$script"
