#!/bin/sh
# Prove the app template and its parts the way an app gets them: generate with offrails-gen, then
# the Definition of Done on SQLite and Postgres.
#
#   tools/ci/test-template.sh                 gen, bare, all, incremental, generators, update-part
#   tools/ci/test-template.sh bare all        just these
#   tools/ci/test-template.sh --no-postgres   skip the Postgres runs (said so in the summary)
#
#   gen          offrails-gen's own tests and clippy
#   bare         `new` with no parts: `cargo offrails verify` (fmt, clippy, tests on SQLite + Postgres),
#                the no-unwrap lint, offrails-check, memory budget
#   all          `new --parts all` (every part, so every sample's anchors are proven on the template)
#   incremental  `new` bare, then `add-part` one part at a time into the generated app, then every
#                add-part again (must change nothing), then the same gates as `all`
#   generators   `cargo offrails generate` every kind into a bare app (--public: a references chain, a
#                resource with every field type, page, controller, migrations) and into an app with
#                every part (scoped resources, api, job, mailer, channel, model, test), the gates on
#                both databases, re-running each generator (a no-op), then `destroy` of all of it in
#                reverse: the app must be byte-for-byte what it was
#   update-part  a synthetic newer part (a changed file and a changed wire) merged into an app: a
#                clean merge with an app edit elsewhere, then a conflict with an app edit on the
#                same line (markers, exit 1, nothing else touched)
#
# Postgres: TEST_DATABASE_URL names a server where the suite may create databases (a template and
# one reusable slot per concurrent test, testing.md#postgres-test-databases).
# Apps are generated in $OFFRAILS_CI_DIR (default: a temp dir), sharing one CARGO_TARGET_DIR.
# Exit status: 0 when every run passed.
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
postgres=1
targets=""
for arg in "$@"; do
  case "$arg" in
    --no-postgres) postgres=0 ;;
    -h | --help) sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) echo "test-template: unknown option $arg" >&2; exit 2 ;;
    *) targets="$targets $arg" ;;
  esac
done
[ -n "$targets" ] || targets="gen bare all incremental generators update-part"
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

# verify: `cargo offrails verify --fast` (fmt, CSS, clippy, one build, SQLite + Postgres at once),
# failing on any flaky test not in KNOWN_FLAKY (nextest retries once: NEXTEST_RETRIES).
verify() {
  if [ "$postgres" = 1 ]; then
    TEST_DATABASE_URL=$pg_url NEXTEST_RETRIES=1 cargo offrails verify --fast || return 1
  else
    NEXTEST_RETRIES=1 cargo offrails verify --fast --no-postgres || return 1
  fi
  unknown=$(cat target/verify/tests-*.log 2>/dev/null | grep -E '^[[:space:]]*FLAKY' | awk '{ print $NF }' | while IFS= read -r t; do
    case " $KNOWN_FLAKY " in (*" $t "*) ;; (*) echo "$t" ;; esac
  done)
  [ -z "$unknown" ] || { echo "flaky, and not a known samples flake: $unknown" >&2; return 1; }
}

# The Definition of Done on a generated app. $1: memory budget in MiB (none: skip that step).
gates() {
  step "anchors stand alone" anchors_alone || return 1
  step "tailwind" sh -c 'bin/setup-tailwind >/dev/null' || return 1
  step "verify (fmt, css, clippy, tests on SQLite + Postgres)" verify || return 1
  step "routes" sh -c 'cargo run -q -- routes > /dev/null' || return 1
  step "clippy (no unwrap/expect)" env CARGO_TARGET_DIR="$CARGO_TARGET_DIR/clippy" cargo clippy -q --lib --bins -- -D warnings -D clippy::unwrap_used -D clippy::expect_used || return 1
  step "offrails-check" sh .offrails/tools/offrails-check || return 1
  if [ "$1" != none ]; then
    step "memory budget" sh "$root/tools/ci/memory-budget.sh" . "$1" || return 1
  fi
}

commit_all() {
  # shellcheck disable=SC2016 # $0 is the inner shell's
  step "commit: $1" sh -c 'git add -A && git -c user.name=ci -c user.email=ci@example.invalid commit -qm "$0" --allow-empty' "$1"
}

# generate_all <file>: one `cargo offrails generate` per line of <file>, then each again (a no-op).
generate_all() {
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    # shellcheck disable=SC2086 # one generator's words
    step "generate $line" cargo offrails generate $line || return 1
  done < "$1"
  commit_all "generated" || return 1
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    # shellcheck disable=SC2086
    step "generate $line again (no-op)" sh -c 'cargo offrails generate "$@" | grep -q "nothing to do"' sh $line || return 1
  done < "$1"
  # shellcheck disable=SC2016 # expanded by the inner shell
  step "re-running generators changed nothing" sh -c 'test -z "$(git status --porcelain)" || { git status --short; exit 1; }'
}

# destroy_all <file>: `cargo offrails destroy <kind> <name>` for each line, last first; then the app
# must be exactly the baseline commit.
destroy_all() {
  lines=$(awk 'NF { print }' "$1" | sed '1!G;h;$!d')
  printf '%s\n' "$lines" | while IFS= read -r line; do
    # shellcheck disable=SC2086 # one generator's words
    set -- $line
    kind=$1
    if [ "$kind" = test ]; then gname="$2 $3"; else gname=$2; fi
    # shellcheck disable=SC2086
    step "destroy $kind $gname" cargo offrails destroy $kind $gname || exit 1
  done || return 1
  step "destroy left the app as it was" sh -c 'git diff --quiet baseline -- . && test -z "$(git status --porcelain --untracked-files=all)" || { git status --short; git diff --stat baseline; exit 1; }'
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
    generators)
      # Bare: no accounts, so every resource is --public. Crates the generators add are added
      # first, so the destroyed app matches the baseline byte for byte.
      fresh gen_bare
      (cd "$work/$name" && gen new gen_bare gen_bare "Generated Bare") || return 1
      step "crates the generators add" sh -c 'cargo add -q rust_decimal && cargo add -q sea-orm --features with-rust_decimal' || return 1
      commit_all "baseline" && step "tag baseline" git tag baseline || return 1
      cat > "$work/$name/bare.gens" <<'GENS'
resource authors name:string --public
resource books title:string isbn:string:unique author:references --public
resource chapters title:string number:integer book:references --public
resource items name:string notes:text:optional count:integer big:bigint flag:boolean active:boolean:optional weight:decimal{8.3} price:money cost:money:optional at:datetime day:date:optional code:uuid:unique meta:json:optional book:references:optional --public
page about --nav
controller dashboard index summary
migration add_subtitle_to_books subtitle:string:optional:index
migration backfill_isbns
test unit isbn_checksum
GENS
      generate_all "$work/$name/bare.gens" || return 1
      gates none || return 1
      destroy_all "$work/$name/bare.gens" || return 1
      # Every part: accounts, so resources are scoped to their owner (and the tenancy suites).
      fresh gen_all
      (cd "$work/$name" && gen new gen_all gen_all "Generated All" --parts all) || return 1
      step "crates the generators add" sh -c 'cargo add -q rust_decimal && cargo add -q sea-orm --features with-rust_decimal && cargo add -q utoipa --features axum_extras,chrono' || return 1
      commit_all "baseline" && step "tag baseline" git tag baseline || return 1
      cat > "$work/$name/all.gens" <<'GENS'
resource posts title:string slug:string:unique body:text published_at:datetime:optional
resource comments body:text post:references
resource items name:string notes:text:optional count:integer big:bigint flag:boolean active:boolean:optional weight:decimal{8.3} price:money cost:money:optional at:datetime day:date:optional code:uuid:unique meta:json:optional post:references:optional
model tag name:string:unique
api notes title:string body:text done:boolean
api posts
job notify_post_published post_id:bigint --queue mailers
mailer post_mailer published digest
channel post
page about
controller reports index
migration add_pinned_to_posts pinned:boolean
test request checkout
GENS
      generate_all "$work/$name/all.gens" || return 1
      gates none || return 1
      destroy_all "$work/$name/all.gens" || return 1
      ;;
    update-part)
      fresh up_app
      (cd "$work/$name" && gen new up_app up_app "Update App" --parts mailer) || return 1
      commit_all "baseline" || return 1
      # A newer framework whose mailer changed one line of a file and one wire.
      newer="$work/$name/newer-fw"
      mkdir -p "$newer" && cp -R "$root/VERSION" "$root/template" "$newer/" || return 1
      perl -pi -e 's/^\/\/! `TestMailer`: one harmless/\/\/! `TestMailer` (newer): one harmless/' \
        "$newer/template/parts/mailer/files/src/mailers/test_mailer.rs" || return 1
      perl -pi -e 's/^mod mailer_test;$/mod mailer_test; \/\/ the newer wire/; s/^version = "(.*)"$/version = "$1-newer"/' \
        "$newer/template/parts/mailer/sample.toml" || return 1
      # Clean: the app changed another line of the same file.
      step "app edit elsewhere" perl -pi -e 's/Test email from \{app_name\}/Test email (app) from {app_name}/' src/mailers/test_mailer.rs || return 1
      step "update-part merges cleanly" cargo offrails update-part mailer --framework "$newer" || return 1
      step "both changes are in, no markers" sh -c 'grep -q "(newer): one harmless" src/mailers/test_mailer.rs && grep -q "Test email (app) from" src/mailers/test_mailer.rs && grep -q "mod mailer_test; // the newer wire" tests/app/main.rs && ! grep -rq "<<<<<<<" src tests' || return 1
      step "clippy after the merge" cargo clippy -q --all-targets -- -D warnings || return 1
      # Conflict: the app changed the same line.
      step "reset" sh -c 'git checkout -q . && git clean -qfd' || return 1
      step "app edit on the same line" perl -pi -e 's/^\/\/! `TestMailer`: one harmless/\/\/! `TestMailer` (app): one harmless/' src/mailers/test_mailer.rs || return 1
      step "update-part reports the conflict (exit 1)" sh -c '! cargo offrails update-part mailer --framework "$0"' "$newer" || return 1
      step "markers hold both sides" sh -c 'grep -q "^<<<<<<< app" src/mailers/test_mailer.rs && grep -q "(app): one harmless" src/mailers/test_mailer.rs && grep -q "(newer): one harmless" src/mailers/test_mailer.rs && grep -q "^>>>>>>> mailer" src/mailers/test_mailer.rs' || return 1
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
  [ "$t" = update-part ] && dbs="no tests run"
  summary="$summary
$result $t ($dbs)"
done
printf '\n== summary (apps in %s)%s\n' "$work" "$summary"
[ "$failed" = 0 ]
