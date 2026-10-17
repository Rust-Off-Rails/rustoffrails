#!/bin/sh
# Convenience wrapper (Linux, macOS, Git Bash): a new app from the framework template.
#
#   tools/new-app.sh <dir> <crate_name> "<Display Name>" [--pitch "<one line>"] [--parts a,b|all] [--mode htmx]
#
# The same as (and on Windows use directly):
#   cargo run -q --manifest-path <framework>/tools/offrails-gen/Cargo.toml --target-dir <dir>/target/offrails-gen -- new <dir> …
set -eu
[ $# -ge 1 ] || { echo "usage: tools/new-app.sh <dir> <crate_name> \"<Display Name>\" [options]  (--help for all)" >&2; exit 2; }
fw=$(cd "$(dirname "$0")/.." && pwd)
case "$1" in
  -h | --help) exec cargo run -q --manifest-path "$fw/tools/offrails-gen/Cargo.toml" --target-dir "${TMPDIR:-/tmp}/offrails-gen-target" -- --help ;;
esac
dir=$1
exec cargo run -q --manifest-path "$fw/tools/offrails-gen/Cargo.toml" --target-dir "$dir/target/offrails-gen" -- new "$@"
