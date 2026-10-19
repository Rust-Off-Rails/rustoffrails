#!/bin/sh
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
#   BASE=… sh .offrails/tools/browser-smoke/run.sh boosted   # every page after a boosted link (from the
#                                                       app root; no install: Node >= 18 + Chrome)
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
# The boosted smoke has no dependencies: run it in place (it reads docs/routes.md from here).
if [ "${1:-}" = boosted ]; then
  exec node "$here/smoke-boosted.mjs"
fi
cache="${XDG_CACHE_HOME:-$HOME/.cache}/offrails/browser-smoke"
version="25.13.0"      # puppeteer-core, verified 2026-10-12
axe_version="4.13.0"   # axe-core, verified 2026-10-13
# Node >= 22.12 (puppeteer-core 25's floor): major*100 + minor, so 22.0-22.11 are refused too.
node_level="$(node -p 'const [a, b] = process.versions.node.split(".").map(Number); a * 100 + b' 2>/dev/null || echo 0)"
# Too old (or no Node) is a skip, reported as one (exit 77, like "no Chrome"), never a pass.
if [ "$node_level" -lt 2212 ]; then
  echo "skip: browser smoke needs Node >= 22.12 (Node 24 LTS recommended); found $(node --version 2>/dev/null || echo none)" >&2
  exit 77
fi
mkdir -p "$cache"
if [ "$(cat "$cache/.version" 2>/dev/null)" != "$version+$axe_version" ]; then
  echo "installing puppeteer-core@$version and axe-core@$axe_version into $cache (once)…"
  npm install --prefix "$cache" --no-audit --no-fund --silent "puppeteer-core@$version" "axe-core@$axe_version"
  echo "$version+$axe_version" > "$cache/.version"
fi
# ESM resolves imports next to the script, so run a copy from under the cache dir: its own
# directory per run (smokes run in parallel; a shared copy was overwritten while another read it),
# whose parent holds node_modules.
case "${1:-dev-tools}" in
  dev-tools) script=smoke-dev-tools.mjs ;;
  live-tabs) script=smoke-live-tabs.mjs ;;
  login) script=smoke-login.mjs ;;
  a11y) script=smoke-a11y.mjs ;;
  *) echo "unknown smoke '$1' (dev-tools | live-tabs | login | a11y | boosted)" >&2; exit 2 ;;
esac
run="$(mktemp -d "$cache/run.XXXXXX")"
trap 'rm -rf "$run"' EXIT INT TERM
cp "$here/lib.mjs" "$here/cdp.mjs" "$here/$script" "$run/"
status=0
node "$run/$script" || status=$?
exit "$status"
