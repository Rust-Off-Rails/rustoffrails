#!/bin/sh
# Memory budget: boot an app built from the template (release build, production config, SQLite),
# serve ~1000 requests across its public routes, and fail when its resident memory (RSS) is above
# the budget.
#
#   tools/ci/memory-budget.sh <app-dir> [budget-MiB]     (default budget: 40)
#
# Linux and macOS (ps -o rss=). Windows: not measured (known gap; template/README.md#windows).
# MEMORY_PROFILE=debug measures a debug build instead (no budget is enforced then).
set -eu

[ $# -ge 1 ] || { echo "usage: tools/ci/memory-budget.sh <app-dir> [budget-MiB]" >&2; exit 2; }
app=$(cd "$1" && pwd)
budget=${2:-40}
profile=${MEMORY_PROFILE:-release}
crate=$(awk '/^\[package\]/ { p = 1; next } /^\[/ { p = 0 } p && $1 == "name" { gsub(/[" ]/, "", $3); print $3; exit }' "$app/Cargo.toml")
target=${CARGO_TARGET_DIR:-$app/target}

if [ "$profile" = release ]; then
  (cd "$app" && cargo build -q --release --bin "$crate")
  bin="$target/release/$crate"
else
  (cd "$app" && cargo build -q --bin "$crate")
  bin="$target/debug/$crate"
fi

# Production refuses to boot without the CSS it serves.
if [ -f "$app/assets/app.css" ] && [ ! -f "$app/static/app.css" ]; then
  (cd "$app" && bin/setup-tailwind >/dev/null && bin/tailwindcss -i assets/app.css -o static/app.css --minify 2>/dev/null)
fi

work=$(mktemp -d)
port=$(( 20000 + $$ % 20000 ))
pid=""
cleanup() {
  if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi
  rm -rf "$work"
}
trap cleanup EXIT INT TERM

export APP_ENV=production PORT="$port" APP_URL="http://localhost:$port" APP_BIND=127.0.0.1 \
  DATABASE_URL="sqlite://$work/budget.sqlite3?mode=rwc" LOG_FORMAT=json RUST_LOG=warn
# Secrets the parts require in production (auth-core, auth-totp, realtime, mailer): fresh random ones.
export APP_SECRET_KEY="${APP_SECRET_KEY:-$(openssl rand -base64 48 | tr -d '\n')}" \
  APP_TOTP_ENCRYPTION_KEY="${APP_TOTP_ENCRYPTION_KEY:-$(openssl rand -base64 32)}" \
  APP_REALTIME_SECRET="${APP_REALTIME_SECRET:-$(openssl rand -hex 32)}" \
  MAILER="${MAILER:-log}" MAIL_FROM="${MAIL_FROM:-Budget <budget@example.com>}"
(cd "$app" && "$bin" db migrate >/dev/null)
(cd "$app" && exec "$bin" serve) >"$work/serve.log" 2>&1 &
pid=$!
i=0
until curl -fsS -o /dev/null "http://127.0.0.1:$port/up" 2>/dev/null; do
  i=$((i + 1))
  if [ "$i" -gt 100 ]; then cat "$work/serve.log" >&2; echo "memory-budget: the app didn't boot" >&2; exit 1; fi
  sleep 0.1
done
rss_kib() { ps -o rss= -p "$pid" | tr -d ' '; }
boot=$(rss_kib)

# ~1000 requests over a few connections: pages, health, meta, static files, a 404.
: >"$work/urls"
n=0
while [ "$n" -lt 125 ]; do
  for path in / /up /robots.txt /sitemap.xml /favicon.ico /static/app.js /static/og.png /no-such-page; do
    printf 'url = "http://127.0.0.1:%s%s"\noutput = "/dev/null"\n' "$port" "$path" >>"$work/urls"
  done
  n=$((n + 1))
done
split -l 500 "$work/urls" "$work/part."
curls=""
for f in "$work"/part.*; do
  curl -s -K "$f" &
  curls="$curls $!"
done
wait_curl=0
for c in $curls; do wait "$c" || wait_curl=1; done
[ "$wait_curl" = 0 ] || echo "memory-budget: some requests failed (counted anyway)" >&2
after=$(rss_kib)

boot_mib=$(( (boot + 1023) / 1024 ))
after_mib=$(( (after + 1023) / 1024 ))
echo "memory-budget: $crate ($profile): RSS ${boot_mib} MiB after boot, ${after_mib} MiB after 1000 requests (budget ${budget} MiB)"
if [ "$profile" = release ] && [ "$after_mib" -gt "$budget" ]; then
  echo "memory-budget: FAIL: ${after_mib} MiB is over the ${budget} MiB budget" >&2
  exit 1
fi
