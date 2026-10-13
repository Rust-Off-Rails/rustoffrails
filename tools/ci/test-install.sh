#!/bin/sh
# shellcheck disable=SC2015 # pass() never fails, so A && pass || bad is safe here
# Installer tests: builds the release assets from the working tree, then installs them
# (OFFRAILS_SOURCE, --yes) into a temp HOME on this machine and into clean Linux containers
# as a non-root user. Asserts the files land, the `offrails` command works, re-running is a
# no-op, and a tampered checksum is refused.
#
#   tools/ci/test-install.sh            local + containers (skips containers without Docker)
#   tools/ci/test-install.sh --full     also runs the real toolchain path (rustup + cargo-binstall)
#                                       in ubuntu:24.04: slow, needs network
set -eu
cd "$(dirname "$0")/../.."
FULL=0; [ "${1:-}" = "--full" ] && FULL=1
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT INT TERM
fail=0
pass() { echo "ok   $*"; }
bad()  { echo "FAIL $*"; fail=1; }

tools/release/release.sh --build-only --worktree --out "$T/dist" >/dev/null
VERSION=$(head -n1 VERSION)

# ── local (this machine), with every POSIX shell available ──
for shell in sh dash bash zsh; do
  command -v "$shell" >/dev/null 2>&1 || continue
  H="$T/home-$shell"; mkdir -p "$H"
  if HOME="$H" OFFRAILS_SOURCE="$T/dist" NO_COLOR=1 "$shell" install.sh --yes --no-toolchain --no-agents >"$T/out-$shell" 2>&1; then
    [ -f "$H/.offrails/$VERSION/AGENTS.md" ] && [ -L "$H/.offrails/current" ] && [ -x "$H/.local/bin/offrails" ] \
      && pass "local/$shell: install lands framework, current symlink and command" || bad "local/$shell: files missing"
    HOME="$H" "$H/.local/bin/offrails" version | grep -q "Rust off Rails $VERSION" && pass "local/$shell: offrails version" || bad "local/$shell: offrails version"
    HOME="$H" OFFRAILS_SOURCE="$T/dist" NO_COLOR=1 "$shell" install.sh --yes --no-toolchain --no-agents 2>&1 | grep -q "already installed" \
      && pass "local/$shell: re-run is a no-op" || bad "local/$shell: re-run not idempotent"
  else
    bad "local/$shell: install failed"; cat "$T/out-$shell"
  fi
done

H="$T/home-doctor"; mkdir -p "$H"
HOME="$H" OFFRAILS_SOURCE="$T/dist" sh install.sh --yes --no-toolchain --no-agents >/dev/null 2>&1
HOME="$H" "$H/.local/bin/offrails" doctor >"$T/doctor" 2>&1 || true
grep -q "framework      ok" "$T/doctor" && pass "local: offrails doctor reports the framework" || { bad "local: offrails doctor"; cat "$T/doctor"; }
HOME="$H" "$H/.local/bin/offrails" new Bad_Name >/dev/null 2>&1 && bad "local: invalid app name accepted" || pass "local: invalid app name rejected"
HOME="$H" "$H/.local/bin/offrails" uninstall -y >/dev/null 2>&1; [ ! -e "$H/.offrails" ] && [ ! -e "$H/.local/bin/offrails" ] && pass "local: uninstall removes everything" || bad "local: uninstall"

mkdir -p "$T/tampered"; cp "$T/dist/"* "$T/tampered/"
awk '$2=="rustoffrails.tar.gz"{$1="0000000000000000000000000000000000000000000000000000000000000000"} {print $1"  "$2}' "$T/dist/checksums.txt" >"$T/tampered/checksums.txt"
H="$T/home-tamper"; mkdir -p "$H"
if HOME="$H" OFFRAILS_SOURCE="$T/tampered" sh install.sh --yes --no-toolchain --no-agents >"$T/tamper" 2>&1; then bad "local: tampered checksum was installed"
else grep -q "checksum mismatch" "$T/tamper" && [ ! -e "$H/.offrails/$VERSION" ] && pass "local: tampered checksum refused, nothing installed" || bad "local: tamper message"; fi

HOME="$T/home-dry" OFFRAILS_SOURCE="$T/dist" sh install.sh --dry-run --no-agents >/dev/null 2>&1 && [ ! -e "$T/home-dry/.offrails" ] && pass "local: --dry-run changes nothing" || bad "local: --dry-run"

# ── clean Linux containers, non-root ──
if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  for img in ubuntu:24.04 debian:trixie-slim; do
    if docker run --rm -v "$T/dist:/dist:ro" -v "$PWD/install.sh:/install.sh:ro" "$img" sh -ec '
        useradd -m dev
        su dev -c "OFFRAILS_SOURCE=/dist NO_COLOR=1 sh /install.sh --yes --no-toolchain --no-agents >/tmp/1 2>&1 || { cat /tmp/1; exit 1; }
                   ~/.local/bin/offrails version | grep -q \"Rust off Rails\"
                   ~/.local/bin/offrails doctor >/tmp/d 2>&1 || true; grep -q \"framework      ok\" /tmp/d
                   OFFRAILS_SOURCE=/dist sh /install.sh --yes --no-toolchain --no-agents 2>&1 | grep -q \"already installed\""
      ' >"$T/c" 2>&1; then pass "container/$img: install, command, doctor, idempotent re-run (non-root)"
    else bad "container/$img"; cat "$T/c"; fi
  done
  if [ "$FULL" = 1 ]; then
    if docker run --rm -v "$T/dist:/dist:ro" -v "$PWD/install.sh:/install.sh:ro" ubuntu:24.04 sh -ec '
        apt-get update -qq && apt-get install -y -qq curl ca-certificates build-essential bash >/dev/null
        useradd -m dev
        su dev -c "OFFRAILS_SOURCE=/dist NO_COLOR=1 sh /install.sh --yes --no-agents && . ~/.cargo/env && sea-orm-cli --version && cargo nextest --version && cargo watch --version && ~/.local/bin/offrails doctor"
      ' >"$T/full" 2>&1; then pass "container/ubuntu:24.04 --full: rustup + cargo-binstall toolchain"; tail -20 "$T/full"
    else bad "container/ubuntu:24.04 --full"; tail -40 "$T/full"; fi
  fi
else
  echo "skip containers: Docker not available"
fi

exit $fail
