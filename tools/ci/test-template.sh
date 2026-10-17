#!/bin/sh
# Prove the app template and its parts the way an app gets them: generate with offrails-gen, then
# the Definition of Done on SQLite and Postgres.
#
#   tools/ci/test-template.sh                 gen, bare, all, incremental
#   tools/ci/test-template.sh bare all        just these
#   tools/ci/test-template.sh --no-postgres   skip the Postgres runs (said so in the summary)
#
#   gen          offrails-gen's own tests and clippy
#   bare         `new` with no parts: fmt, clippy, tests (SQLite + Postgres), offrails-check, memory budget
#   all          `new --parts all` (every part, so every sample's anchors are proven on the template)
#   incremental  `new` bare, then `add-part` one part at a time into the generated app, then every
#                add-part again (must change nothing), then the same gates as `all`
#
# Postgres: TEST_DATABASE_URL names a server where the suite may create databases (one per test).
# Apps are generated in $OFFRAILS_CI_DIR (default: a temp dir), sharing one CARGO_TARGET_DIR.
# Exit status: 0 when every run passed.
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
postgres=1
targets=""
for arg in "$@"; do
  case "$arg" in
    --no-postgres) postgres=0 ;;
    -h | --help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) echo "test-template: unknown option $arg" >&2; exit 2 ;;
    *) targets="$targets $arg" ;;
  esac
done
[ -n "$targets" ] || targets="gen bare all incremental"
if [ "$postgres" = 1 ] && [ -z "${TEST_DATABASE_URL:-}" ]; then
  echo "test-template: TEST_DATABASE_URL is not set (--no-postgres skips the Postgres runs)" >&2
  exit 2
fi
pg_url="${TEST_DATABASE_URL:-}"
unset TEST_DATABASE_URL

work=${OFFRAILS_CI_DIR:-$(mktemp -d)}
mkdir -p "$work"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$work/target}"
export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-never}"
gen_manifest="$root/tools/offrails-gen/Cargo.toml"
gen() { cargo run -q --manifest-path "$gen_manifest" --target-dir "$work/gen-target" -- "$@"; }

name=""
app=""
step() {
  label=$1
  shift
  printf '\n== %s: %s\n' "$name" "$label"
  if (cd "$app" && "$@"); then return 0; fi
  echo "FAIL $name: $label" >&2
  return 1
}

# No `samples:` anchor trails code after cargo fmt (guides/samples.md#anchors).
anchors_alone() {
  trailing=$(grep -rnE '[^[:space:]].*(//|[{]#|#) samples: ' src templates migration tests | grep -vE ':[[:space:]]*(//|[{]#|#) samples: ' || true)
  [ -z "$trailing" ] || { printf '%s\n' "$trailing"; return 1; }
}

# Parts' tests known to be flaky under a parallel run (each passes alone): they share the
# dev dashboard's on-disk state (storage/dashboard/) across test processes. A samples bug, to be
# fixed upstream and synced; until then one retry is allowed for these names only, and any other
# flaky test fails the run.
KNOWN_FLAKY="dev_dashboard_test::run_history_is_capped_redacted_and_reopened dev_dashboard_test::bench_drives_a_running_server"

# suite [TEST_DATABASE_URL]: the test suite, failing on any flaky test not in KNOWN_FLAKY.
suite() {
  out=$(mktemp)
  if [ -n "${1:-}" ]; then
    TEST_DATABASE_URL=$1 cargo nextest run --no-fail-fast --retries 1 --status-level fail --final-status-level flaky >"$out" 2>&1
  else
    cargo nextest run --no-fail-fast --retries 1 --status-level fail --final-status-level flaky >"$out" 2>&1
  fi
  status=$?
  tail -5 "$out"
  unknown=$(grep -E '^[[:space:]]*FLAKY' "$out" | awk '{ print $NF }' | while IFS= read -r t; do
    case " $KNOWN_FLAKY " in (*" $t "*) ;; (*) echo "$t" ;; esac
  done)
  rm -f "$out"
  [ "$status" = 0 ] || return 1
  [ -z "$unknown" ] || { echo "flaky, and not a known samples flake: $unknown" >&2; return 1; }
}

# The Definition of Done on a generated app. $1: memory budget in MiB.
gates() {
  step "fmt --check" cargo fmt --all --check || return 1
  step "anchors stand alone" anchors_alone || return 1
  step "css" sh -c 'bin/setup-tailwind >/dev/null && bin/tailwindcss -i assets/app.css -o static/app.css --minify 2>&1 | tail -1' || return 1
  step "routes" sh -c 'cargo run -q -- routes > /dev/null' || return 1
  step "clippy" cargo clippy -q --all-targets -- -D warnings || return 1
  step "clippy (no unwrap/expect)" cargo clippy -q --lib --bins -- -D warnings -D clippy::unwrap_used -D clippy::expect_used || return 1
  step "tests (SQLite)" suite || return 1
  if [ "$postgres" = 1 ]; then
    step "tests (Postgres)" suite "$pg_url" || return 1
  fi
  step "offrails-check" sh .offrails/tools/offrails-check || return 1
  step "memory budget" sh "$root/tools/ci/memory-budget.sh" . "$1" || return 1
}

fresh() {
  app="$work/$name/$1"
  rm -rf "${work:?}/${name:?}"
  mkdir -p "$work/$name"
}

run_one() {
  name=$1
  case "$name" in
    gen)
      app="$root/tools/offrails-gen"
      step "unit tests" cargo test -q --target-dir "$work/gen-target" || return 1
      step "clippy" cargo clippy -q --all-targets --target-dir "$work/gen-target" -- -D warnings || return 1
      step "fmt --check" cargo fmt --check || return 1
      ;;
    bare)
      fresh bare_app
      (cd "$work/$name" && gen new bare_app bare_app "Bare App") || return 1
      gates 24 || return 1
      ;;
    all)
      fresh all_app
      (cd "$work/$name" && gen new all_app all_app "All Parts" --parts all) || return 1
      gates 32 || return 1
      ;;
    incremental)
      fresh inc_app
      (cd "$work/$name" && gen new inc_app inc_app "Incremental App") || return 1
      step "baseline commit" sh -c 'git add -A && git -c user.name=ci -c user.email=ci@example.invalid commit -qm baseline' || return 1
      parts=$(gen list-parts | awk 'NR > 1 && NF > 2 && $1 !~ /^Add|^Which/ { print $1 }')
      for p in $parts; do
        step "add-part $p" cargo offrails add-part "$p" || return 1
      done
      step "commit parts" sh -c 'git add -A && git -c user.name=ci -c user.email=ci@example.invalid commit -qm parts' || return 1
      for p in $parts; do
        step "add-part $p again (no-op)" cargo offrails add-part "$p" || return 1
      done
      # shellcheck disable=SC2016 # expanded by the inner shell
      step "re-running add-part changed nothing" sh -c 'test -z "$(git status --porcelain)" || { git status --short; exit 1; }' || return 1
      step "list-parts shows them installed" sh -c '! cargo offrails list-parts | grep -q "^available"' || return 1
      gates 32 || return 1
      ;;
    *) echo "test-template: unknown target $name" >&2; return 1 ;;
  esac
}

summary=""
failed=0
for t in $targets; do
  if run_one "$t"; then result="ok  "; else result="FAIL"; failed=$((failed + 1)); fi
  if [ "$postgres" = 1 ]; then dbs="SQLite + Postgres"; else dbs="SQLite only (Postgres skipped)"; fi
  [ "$t" = gen ] && dbs="no database"
  summary="$summary
$result $t ($dbs)"
done
printf '\n== summary (apps in %s)%s\n' "$work" "$summary"
[ "$failed" = 0 ]
