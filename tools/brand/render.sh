#!/usr/bin/env bash
# Day-one brand assets for any app with pages (generators/new.md step 5, guides/views.md#meta-favicon-and-social-cards).
# Writes, into <static dir>:
#   favicon.svg            the app's initial on its accent color (editable SVG, the source of truth)
#   favicon.ico            48×48 PNG wrapped in an ICO container (browsers that ask for /favicon.ico)
#   apple-touch-icon.png   180×180
#   icon-512.png           512×512 (PWA manifests, app stores)
#   og.png                 1200×630 social card (Open Graph / Twitter), rendered from og.html
# and keeps the HTML source at <static dir>/../assets/brand/og.html so agents can restyle and re-run.
#
#   bash .offrails/tools/brand/render.sh "Notes" "#4f46e5" "Private notes, anywhere." static
#
# Needs only Google Chrome (or Chromium) at generation time; outputs are committed, never rendered at runtime.
# CHROME=/path/to/chrome overrides the default location. Re-running overwrites the outputs.
set -euo pipefail

NAME="${1:?app name}"; ACCENT="${2:?accent color, e.g. #4f46e5}"; TAGLINE="${3:-}"; OUT="${4:-static}"
CHROME="${CHROME:-}"
if [ -z "$CHROME" ]; then
  for c in "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
           "/Applications/Chromium.app/Contents/MacOS/Chromium" \
           "$(command -v google-chrome || true)" "$(command -v chromium || true)" "$(command -v chromium-browser || true)"; do
    if [ -n "$c" ] && [ -x "$c" ]; then CHROME="$c"; break; fi
  done
fi
[ -n "$CHROME" ] || { echo "no Chrome/Chromium found: set CHROME=… (or ship favicon.svg only and log it in docs/decisions.md)" >&2; exit 1; }

INITIAL="$(printf '%s' "$NAME" | sed 's/^[^[:alnum:]]*//' | cut -c1 | tr '[:lower:]' '[:upper:]')"
esc() { printf '%s' "$1" | sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g; s/"/\&quot;/g'; }
mkdir -p "$OUT" "$OUT/../assets/brand"
SRC="$(cd "$OUT/../assets/brand" && pwd)"; OUT="$(cd "$OUT" && pwd)"
WORK="$(mktemp -d)"; PROFILE="$(mktemp -d)"; trap 'rm -rf "$WORK" "$PROFILE"' EXIT

cat > "$OUT/favicon.svg" <<SVG
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="14" fill="$ACCENT"/><text x="32" y="44" text-anchor="middle" font-family="-apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif" font-size="38" font-weight="800" fill="#fff">$(esc "$INITIAL")</text></svg>
SVG

[ -f "$SRC/og.html" ] || cat > "$SRC/og.html" <<HTML
<!doctype html><html><head><meta charset="utf-8"><style>
  html,body{margin:0;width:1200px;height:630px}
  body{display:flex;flex-direction:column;justify-content:center;padding:0 96px;box-sizing:border-box;
       background:#0f1115;color:#fff;font-family:-apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif}
  .mark{width:112px;height:112px;margin-bottom:48px}
  h1{font-size:88px;line-height:1;margin:0 0 28px;letter-spacing:-.02em}
  p{font-size:38px;line-height:1.3;margin:0;color:#c9ced8;max-width:960px}
  .bar{position:absolute;left:0;bottom:0;height:14px;width:100%;background:$ACCENT}
</style></head><body>
  <img class="mark" src="FAVICON_SRC" alt="">
  <h1>$(esc "$NAME")</h1>
  <p>$(esc "$TAGLINE")</p>
  <div class="bar"></div>
</body></html>
HTML

shot() { # shot <html file> <w> <h> <out.png>   (headless Chrome hangs below ~48px windows: never shoot 32×32)
  perl -e 'alarm 90; exec @ARGV' "$CHROME" --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
    --user-data-dir="$PROFILE" --window-size="$2,$3" --screenshot="$4" "file://$1" >/dev/null 2>&1
  [ -s "$4" ] || { echo "render failed: $4" >&2; exit 1; }
}
icon_page() { # icon_page <size> → html that shows favicon.svg at that size
  printf '<!doctype html><html><head><style>html,body{margin:0;background:transparent}img{display:block;width:%spx;height:%spx}</style></head><body><img src="file://%s"></body></html>' "$1" "$1" "$OUT/favicon.svg" > "$WORK/icon-$1.html"
  echo "$WORK/icon-$1.html"
}
shot "$(icon_page 48)" 48 48 "$WORK/favicon-48.png"
shot "$(icon_page 180)" 180 180 "$OUT/apple-touch-icon.png"
shot "$(icon_page 512)" 512 512 "$OUT/icon-512.png"
sed "s|FAVICON_SRC|file://$OUT/favicon.svg|" "$SRC/og.html" > "$WORK/og.html"
shot "$WORK/og.html" 1200 630 "$OUT/og.png"

# favicon.ico: one 48×48 PNG in an ICO container (valid since Windows Vista; every current browser).
le32() { printf "\\x$(printf %02x $(($1 & 255)))\\x$(printf %02x $((($1 >> 8) & 255)))\\x$(printf %02x $((($1 >> 16) & 255)))\\x$(printf %02x $((($1 >> 24) & 255)))"; }
SIZE=$(wc -c < "$WORK/favicon-48.png" | tr -d ' ')
{ printf '\x00\x00\x01\x00\x01\x00'; printf '\x30\x30\x00\x00\x01\x00\x20\x00'; le32 "$SIZE"; le32 22; cat "$WORK/favicon-48.png"; } > "$OUT/favicon.ico"

echo "wrote $OUT/{favicon.svg,favicon.ico,apple-touch-icon.png,icon-512.png,og.png} (source: $SRC/og.html)"
