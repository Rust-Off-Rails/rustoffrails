#!/bin/sh
# Maintainers: refresh template/parts/ from a checkout of the samples repo
# (https://github.com/rust-off-rails/samples), at a tag or commit.
#
#   tools/sync-parts.sh <samples-checkout> [<ref>]       (default ref: HEAD)
#
# Copies each samples/<name>/{sample.toml,README.md,CHECKS.md,files/} verbatim (the samples are
# the source of truth for parts; never edit template/parts/ by hand) and records the commit in
# template/parts/SOURCE. Then run tools/ci/test-template.sh. The base app (template/app) flows the
# other way: it is the framework's, and the samples repo's host/ is generated from it.
set -eu

[ $# -ge 1 ] || { echo "usage: tools/sync-parts.sh <samples-checkout> [<ref>]" >&2; exit 2; }
src=$1
ref=${2:-HEAD}
root=$(cd "$(dirname "$0")/.." && pwd)
commit=$(git -C "$src" rev-parse "$ref^{commit}")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
git -C "$src" archive "$commit" samples | tar -x -C "$tmp"

dest="$root/template/parts"
for d in "$dest"/*/; do
  [ -f "$d/sample.toml" ] && rm -rf "$d"
done
for d in "$tmp"/samples/*/; do
  [ -f "$d/sample.toml" ] || continue
  name=$(basename "$d")
  mkdir -p "$dest/$name"
  for f in sample.toml README.md CHECKS.md files; do
    [ -e "$d/$f" ] && cp -R "$d/$f" "$dest/$name/"
  done
done
printf 'rust-off-rails/samples %s (%s)\n' "$commit" "$(git -C "$src" describe --tags --always "$commit")" >"$dest/SOURCE"
echo "template/parts synced from samples $commit; now: tools/ci/test-template.sh"
