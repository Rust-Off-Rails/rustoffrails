#!/usr/bin/env bash
# SUPERSEDED in 0.7.3 by brand.toml + `cargo offrails brand` (guides/views.md#brand), which renders these
# files and the theme CSS from one file with contrast checks; `new` runs it. Kept for apps without a
# brand.toml (`cargo offrails brand --init` writes one).
#
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
# Renders with Google Chrome (or Chromium) at generation time; outputs are committed, never rendered at runtime.
# CHROME=/path/to/chrome overrides the default location. Re-running overwrites the outputs.
# Without Chrome: rsvg-convert (librsvg: brew install librsvg / apt install librsvg2-bin) rasterizes the SVGs (the social card
# becomes a plain SVG card); with none of them, the neutral set in tools/brand/default/ is copied
# and favicon.svg still carries the app's initial. Re-run with Chrome later for the full og.png.
# RENDERER=chrome|rsvg|default forces one path. (ImageMagick isn't used: its SVG text needs local font setup.)
set -euo pipefail

NAME="${1:?app name}"; ACCENT="${2:?accent color, e.g. #4f46e5}"; TAGLINE="${3:-}"; OUT="${4:-static}"
CHROME="${CHROME:-}"
RENDERER="${RENDERER:-auto}"   # auto | chrome | rsvg | default (force one, e.g. to test a fallback)
if [ -z "$CHROME" ] && { [ "$RENDERER" = auto ] || [ "$RENDERER" = chrome ]; }; then
  for c in "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
           "/Applications/Chromium.app/Contents/MacOS/Chromium" \
           "$(command -v google-chrome || true)" "$(command -v chromium || true)" "$(command -v chromium-browser || true)"; do
    if [ -n "$c" ] && [ -x "$c" ]; then CHROME="$c"; break; fi
  done
fi
RASTER=""
case "$RENDERER" in
  rsvg) CHROME=""; RASTER=rsvg ;;
  default) CHROME="" ;;
  chrome) [ -n "$CHROME" ] || { echo "RENDERER=chrome but no Chrome/Chromium found: set CHROME=…" >&2; exit 1; } ;;
esac
if [ -z "$CHROME" ] && [ "$RENDERER" = auto ]; then
  if command -v rsvg-convert >/dev/null 2>&1; then RASTER=rsvg; fi
fi

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
raster() { # raster <svg> <w> <h> <out.png>   (the no-Chrome path)
  rsvg-convert -w "$2" -h "$3" -o "$4" "$1"
  [ -s "$4" ] || { echo "render failed: $4" >&2; exit 1; }
}
icon_page() { # icon_page <size> → html that shows favicon.svg at that size
  printf '<!doctype html><html><head><style>html,body{margin:0;background:transparent}img{display:block;width:%spx;height:%spx}</style></head><body><img src="file://%s"></body></html>' "$1" "$1" "$OUT/favicon.svg" > "$WORK/icon-$1.html"
  echo "$WORK/icon-$1.html"
}
if [ -n "$CHROME" ]; then
  shot "$(icon_page 48)" 48 48 "$WORK/favicon-48.png"
  shot "$(icon_page 180)" 180 180 "$OUT/apple-touch-icon.png"
  shot "$(icon_page 512)" 512 512 "$OUT/icon-512.png"
  sed "s|FAVICON_SRC|file://$OUT/favicon.svg|" "$SRC/og.html" > "$WORK/og.html"
  shot "$WORK/og.html" 1200 630 "$OUT/og.png"
elif [ -n "$RASTER" ]; then
  echo "no Chrome/Chromium: rasterizing with rsvg-convert (og.png is a plain card; re-run with Chrome for og.html's design)" >&2
  raster "$OUT/favicon.svg" 48 48 "$WORK/favicon-48.png"
  raster "$OUT/favicon.svg" 180 180 "$OUT/apple-touch-icon.png"
  raster "$OUT/favicon.svg" 512 512 "$OUT/icon-512.png"
  cat > "$WORK/og.svg" <<SVG
<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630" viewBox="0 0 1200 630"><rect width="1200" height="630" fill="#0f1115"/><rect x="96" y="150" width="112" height="112" rx="24" fill="$ACCENT"/><text x="152" y="232" text-anchor="middle" font-family="Helvetica,Arial,sans-serif" font-size="66" font-weight="800" fill="#fff">$(esc "$INITIAL")</text><text x="96" y="390" font-family="Helvetica,Arial,sans-serif" font-size="88" font-weight="800" fill="#fff">$(esc "$NAME")</text><text x="96" y="460" font-family="Helvetica,Arial,sans-serif" font-size="38" fill="#c9ced8">$(esc "$TAGLINE")</text><rect y="616" width="1200" height="14" fill="$ACCENT"/></svg>
SVG
  raster "$WORK/og.svg" 1200 630 "$OUT/og.png"
else
  DEFAULTS="$(cd "$(dirname "$0")" && pwd)/default"
  echo "no Chrome/Chromium or rsvg-convert: copying the neutral icon set from $DEFAULTS." >&2
  echo "favicon.svg has the app's initial; log it in docs/decisions.md and re-run render.sh where Chrome exists." >&2
  cp "$DEFAULTS/apple-touch-icon.png" "$DEFAULTS/icon-512.png" "$DEFAULTS/og.png" "$DEFAULTS/favicon.ico" "$OUT/"
  echo "wrote $OUT/favicon.svg; copied the neutral favicon.ico, apple-touch-icon.png, icon-512.png, og.png"
  exit 0
fi

# favicon.ico: one 48×48 PNG in an ICO container (valid since Windows Vista; every current browser).
# shellcheck disable=SC2059  # the format is built from hex escapes on purpose
le32() { printf "\\x$(printf %02x $(($1 & 255)))\\x$(printf %02x $((($1 >> 8) & 255)))\\x$(printf %02x $((($1 >> 16) & 255)))\\x$(printf %02x $((($1 >> 24) & 255)))"; }
SIZE=$(wc -c < "$WORK/favicon-48.png" | tr -d ' ')
{ printf '\x00\x00\x01\x00\x01\x00'; printf '\x30\x30\x00\x00\x01\x00\x20\x00'; le32 "$SIZE"; le32 22; cat "$WORK/favicon-48.png"; } > "$OUT/favicon.ico"

echo "wrote $OUT/{favicon.svg,favicon.ico,apple-touch-icon.png,icon-512.png,og.png} (source: $SRC/og.html)"
