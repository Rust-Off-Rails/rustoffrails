#!/bin/sh
# shellcheck disable=SC2016  # backticks in expected messages are literal
# Tests tools/offrails-check against generated app trees: one that must pass, and one per
# failure class that must fail with its message. Run from the repo root: sh tools/ci/test-offrails-check.sh
set -eu

ROOT=$(cd "$(dirname "$0")/../.." && pwd -P)
CHECK="$ROOT/tools/offrails-check"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
failures=0

# A minimal app that satisfies every gate.
make_app() {
  d="$WORK/$1"
  mkdir -p "$d/.offrails" "$d/product/features" "$d/tests/app" "$d/src/controllers" "$d/docs"
  echo "0.6.0" > "$d/.offrails/VERSION"
  cat > "$d/AGENTS.md" <<'EOF'
# AGENTS.md

```
APP:            fixture
FRAMEWORK:      Rust off Rails 0.6.0 (.offrails/, vendored 2026-10-13)
FRONTEND_MODE:  htmx
ISLANDS:        no
ACCOUNTS:       password  ROLES: none  TENANCY: per-user
```
EOF
  cat > "$d/product/features/notes.md" <<'EOF'
# Feature: notes

## Acceptance criteria

- [x] AC-notes-1: A reader can add a note
- [x] AC-notes-2: Only the author sees a note
- [ ] ~~AC-notes-3: dropped by the human~~

## Security (agent-written threat model)

| STRIDE | Threat | Control | Test |
|--------|--------|---------|------|
| Information disclosure | another reader's note | `note::find_for` | `notes::another_readers_note_is_404` |

## Assumptions (agent-written)
EOF
  cat > "$d/tests/app/main.rs" <<'EOF'
mod guard_test;
mod notes_test;
mod tenancy_test;
EOF
  cat > "$d/tests/app/notes_test.rs" <<'EOF'
/// AC-notes-1
#[tokio::test]
async fn a_reader_can_add_a_note() {}
/// AC-notes-2
#[tokio::test]
async fn another_readers_note_is_404() {}
EOF
  echo '// for (method, template, handler, access) in ROUTES {}' > "$d/tests/app/tenancy_test.rs"
  echo '// guard' > "$d/tests/app/guard_test.rs"
  cat > "$d/src/controllers/notes.rs" <<'EOF'
pub async fn show(RequireUser(user): RequireUser, Path(id): Path<i64>) -> Result<Response, AppError> {
    let n = note::find_for(&s.db, &user, id).await?;
    // a comment mentioning note::find(&db, id) is fine
    let total = note::Entity::find().count(&s.db).await?; // unscoped: AC-notes-9 public counter
}
EOF
  cat > "$d/src/routes.rs" <<'EOF'
pub const ROUTES: &[(&str, &str, &str, Access)] = &[
    ("GET", "/notes/{id}", "notes::show", Access::User),
];
EOF
  echo '| GET | /notes/{id} | notes::show | user |' > "$d/docs/routes.md"
  mkdir -p "$d/migration/src"
  cat > "$d/migration/src/m20261013_000001_create_notes.rs" <<'EOF'
        m.create_table(Table::create().table(Notes::Table)
                .col(big_pk_auto(Notes::Id))
                .col(big_integer(Notes::AuthorId))
                .col(big_integer_null(Notes::ParentId))
                .col(integer(Notes::Stars))
                .col(ColumnDef::new(Notes::Position).integer().not_null())
                .col(string(Notes::Paid))
                .to_owned()).await
EOF
  cat > "$d/tests/app/support.rs" <<'EOF'
let url = std::env::var("TEST_DATABASE_URL").ok();
EOF
  printf '%s\n' "$d"
}

# expect <name> <dir> <0|1> [message fragment…]
expect() {
  name=$1 dir=$2 want=$3; shift 3
  set +e
  out=$(sh "$CHECK" "$dir" 2>&1)
  got=$?
  set -e
  ok=1
  [ "$got" = "$want" ] || ok=0
  for frag in "$@"; do
    printf '%s\n' "$out" | grep -qF -- "$frag" || { ok=0; printf '  missing: %s\n' "$frag"; }
  done
  if [ "$ok" = 1 ]; then
    printf 'ok   %s\n' "$name"
  else
    failures=$((failures + 1))
    printf 'FAIL %s (exit %s, want %s)\n%s\n\n' "$name" "$got" "$want" "$out"
  fi
}

expect "a complete app passes" "$(make_app pass)" 0 "0 error(s)" "1 migrations, every id and *_id column 64-bit" "reads TEST_DATABASE_URL"

d=$(make_app no-agents); rm "$d/AGENTS.md"
expect "a directory without AGENTS.md fails" "$d" 1 "no AGENTS.md here"

d=$(make_app version); echo "0.5.0" > "$d/.offrails/VERSION"
expect "FRAMEWORK line vs .offrails/VERSION" "$d" 1 "says FRAMEWORK 0.6.0, but .offrails/VERSION is 0.5.0"

d=$(make_app islands); sed 's/ISLANDS:        no/ISLANDS:        yes/' "$d/AGENTS.md" > "$d/a" && mv "$d/a" "$d/AGENTS.md"
expect "ISLANDS: yes without UI_LIBRARY or web/" "$d" 1 "needs UI_LIBRARY: react | svelte" "needs a web/ directory"

d=$(make_app ac); echo "- [ ] AC-notes-4: Notes can be searched" >> "$d/product/features/notes.md"
expect "an uncited criterion fails (ticked or not)" "$d" 1 "AC-notes-4 (product/features/notes.md) is not cited by any test"

d=$(make_app ac-prefix); sed 's|/// AC-notes-1$|/// AC-notes-10|' "$d/tests/app/notes_test.rs" > "$d/t" && mv "$d/t" "$d/tests/app/notes_test.rs"
expect "AC-notes-10 doesn't count as citing AC-notes-1" "$d" 1 "AC-notes-1 (product/features/notes.md) is not cited"

d=$(make_app sec-missing); awk '/^## Security/{ skip = 1 } /^## Assumptions/{ skip = 0 } !skip' "$d/product/features/notes.md" > "$d/n" && mv "$d/n" "$d/product/features/notes.md"
expect "a feature without a Security section fails" "$d" 1 "has no '## Security' section"

d=$(make_app sec-prose); sed 's/^| STRIDE.*/- Spoofing: handled (test: matrix)/' "$d/product/features/notes.md" > "$d/n" && mv "$d/n" "$d/product/features/notes.md"
expect "a prose Security section fails" "$d" 1 "isn't the STRIDE table"

d=$(make_app sec-name); sed 's/another_readers_note_is_404`/note_ids_are_unguessable`/' "$d/product/features/notes.md" > "$d/n" && mv "$d/n" "$d/product/features/notes.md"
expect "a Security test that doesn't exist fails" "$d" 1 'names test `note_ids_are_unguessable`'

d=$(make_app sec-na)
sed 's/^| Information disclosure.*/&\
| Repudiation | n\/a: notes have no admin actions to trace | — | — |/' "$d/product/features/notes.md" > "$d/n" && mv "$d/n" "$d/product/features/notes.md"
expect "an n/a threat row with a reason and — as its test passes" "$d" 0 "1 n/a rows" "0 error(s)"

d=$(make_app sec-na-bare)
sed 's/^| Information disclosure.*/&\
| Repudiation | n\/a | — | — |/' "$d/product/features/notes.md" > "$d/n" && mv "$d/n" "$d/product/features/notes.md"
expect "a bare n/a threat row (no reason) fails" "$d" 1 'says "n/a" with no reason'

d=$(make_app sec-dash)
sed 's/^| Information disclosure.*/&\
| Denial of service | flooding the form | rate limit | — |/' "$d/product/features/notes.md" > "$d/n" && mv "$d/n" "$d/product/features/notes.md"
expect "a real threat with — as its test still fails" "$d" 1 'threat "flooding the form" names no test'

# Env vars: read through any helper (std::env::var, a var() closure, parse("X", default)).
make_config() {
  cat > "$1/src/config.rs" <<'EOF'
let var = |k: &str| std::env::var(k).ok();
let env = match std::env::var("APP_ENV").ok().as_deref() { _ => () };
let database_url = var("DATABASE_URL");
let port = parse("PORT", "3000");
let pool = parse("DATABASE_POOL", "10");
let mode = "GET_NOT_ENV"; // not an env var
EOF
  printf 'APP_ENV=development\nPORT=3000\n# DATABASE_URL=sqlite://x\nDATABASE_POOL=10\n' > "$1/.env.example"
}
d=$(make_app env-ok); make_config "$d"
expect "every config.rs variable in .env.example passes" "$d" 0 "4 env vars in src/config.rs"

d=$(make_app env-missing); make_config "$d"
grep -v -e '^DATABASE_POOL=' -e 'DATABASE_URL=' "$d/.env.example" > "$d/e" && mv "$d/e" "$d/.env.example"
expect "a variable read via parse() or a closure but missing from .env.example fails" "$d" 1 \
  "missing variable(s) src/config.rs reads: DATABASE_POOL DATABASE_URL"

d=$(make_app env-none); make_config "$d"; rm "$d/.env.example"
expect "config.rs without .env.example fails" "$d" 1 "there is no .env.example"

d=$(make_app tenancy); rm "$d/tests/app/tenancy_test.rs"
expect "TENANCY without tenancy_test.rs fails" "$d" 1 "TENANCY: per-user but tests/app/tenancy_test.rs is missing"

d=$(make_app unregistered); sed '/guard_test/d' "$d/tests/app/main.rs" > "$d/m" && mv "$d/m" "$d/tests/app/main.rs"
expect "an unregistered guard_test.rs fails" "$d" 1 "has no 'mod guard_test;'"

d=$(make_app none); rm "$d/tests/app/tenancy_test.rs" "$d/tests/app/guard_test.rs"
sed 's/TENANCY: per-user/TENANCY: none/' "$d/AGENTS.md" > "$d/a" && mv "$d/a" "$d/AGENTS.md"
grep -v -e guard_test -e tenancy_test "$d/tests/app/main.rs" > "$d/m" && mv "$d/m" "$d/tests/app/main.rs"
expect "TENANCY: none needs no tenancy suites" "$d" 0 "0 error(s)"

d=$(make_app guard)
cat >> "$d/src/controllers/notes.rs" <<'EOF'
pub async fn index(RequireUser(_user): RequireUser) -> Result<Response, AppError> {
    let n = note::find(&s.db, id).await?;
    note::destroy(&s.db, n).await?;
    let all = note::Entity::find().all(&s.db).await?;
}
EOF
expect "unscoped lookups in handlers fail" "$d" 1 \
  'notes.rs:6: `RequireUser(_`' 'notes.rs:7: `::find(`' 'notes.rs:8: `::destroy(`' 'notes.rs:9: `Entity::find`'

# 64-bit ids: INT4 on Postgres can't decode as i64 (SQLite's INTEGER is 64-bit, so only Postgres fails).
M=migration/src/m20261013_000002_create_cards.rs
d=$(make_app ids-helper)
printf '.col(big_pk_auto(Cards::Id))\n.col(integer(Cards::ColumnId))\n.col(small_integer_null(Cards::TeamId))\n.col(big_integer(Cards::BoardId))\n' > "$d/$M"
expect "integer()/small_integer_null() on *_id columns fails" "$d" 1 \
  "$M: Cards::ColumnId is a 32-bit integer column" "$M: Cards::TeamId is a 32-bit integer column"

d=$(make_app ids-coldef)
cat > "$d/$M" <<'EOF'
.col(ColumnDef::new(Cards::Id).integer().not_null().auto_increment().primary_key())
.col(
    ColumnDef::new(Cards::ColumnId)
        .not_null()
        .integer(),
)
.col(ColumnDef::new(Alias::new("team_id")).small_integer())
.col(ColumnDef::new(Cards::BoardId).big_integer().not_null())
EOF
expect "ColumnDef ids with .integer() fail, across lines and with Alias" "$d" 1 \
  "$M: Cards::Id is a 32-bit" "$M: Cards::ColumnId is a 32-bit" 'Alias::new("team_id") is a 32-bit'

d=$(make_app ids-pk); printf '.col(pk_auto(Cards::Id))\n' > "$d/$M"
expect "pk_auto (serial, INT4 on Postgres) fails" "$d" 1 "$M: Cards::Id is a 32-bit integer column"

d=$(make_app ids-widened); printf '// offrails-check: ids widened in m20261014_000001_widen_ids\n.col(integer(Cards::ColumnId))\n' > "$d/$M"
expect "a waiver naming a missing widening migration fails" "$d" 1 "doesn't exist"
echo '// widen' > "$d/migration/src/m20261014_000001_widen_ids.rs"
expect "a migration widened by a later one passes" "$d" 0 "0 error(s)"

d=$(make_app ids-entity); mkdir -p "$d/src/models"
printf 'pub struct Model {\n    pub id: i32,\n    pub team_id: Option<i32>,\n    pub votes: i32,\n    pub page_id: i32, // not a db id\n}\n' > "$d/src/models/card.rs"
expect "an entity id typed i32 fails" "$d" 1 "src/models/card.rs:2: an id field typed i32" "src/models/card.rs:3: an id field typed i32"

d=$(make_app pg); echo 'let db = connect("sqlite::memory:").await;' > "$d/tests/app/support.rs"
expect "a support.rs that never reads TEST_DATABASE_URL fails" "$d" 1 "never reads TEST_DATABASE_URL"

# Development is this machine only: no 0.0.0.0 literal bind, APP_BIND read, a Host check mounted.
make_server() {
  cat > "$1/src/main.rs" <<'EOF2'
let addr = SocketAddr::new(config.bind, config.port);
// a comment about [0, 0, 0, 0] is fine
let probe = "0.0.0.0:9"; // not the server bind
EOF2
  printf 'let bind = bind_address(env, get("APP_BIND").as_deref());\nlet any = Ipv4Addr::UNSPECIFIED;\n' > "$1/src/config.rs"
  printf '# APP_BIND=0.0.0.0\n' > "$1/.env.example"
  printf '        .layer(from_fn_with_state(hosts::AllowedHosts::new(&config), hosts::middleware))\n' >> "$1/src/routes.rs"
}
d=$(make_app bind-ok); make_server "$d"
expect "a loopback-by-default server with a Host check passes" "$d" 0 "serve binds APP_BIND" "0 error(s)"

d=$(make_app bind-literal); make_server "$d"
printf 'let plain = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port))).await?;\nlet s = "0.0.0.0:3000";\n' >> "$d/src/main.rs"
expect "a literal 0.0.0.0 bind fails" "$d" 1 "src/main.rs:4: a literal 0.0.0.0 bind" "src/main.rs:5: a literal 0.0.0.0 bind"

d=$(make_app bind-env); make_server "$d"; printf 'let port = parse("PORT", "3000");\n' > "$d/src/config.rs"; printf 'PORT=3000\n' > "$d/.env.example"
expect "a config.rs without APP_BIND fails" "$d" 1 "src/config.rs never reads APP_BIND"

d=$(make_app bind-hosts); make_server "$d"; grep -v hosts:: "$d/src/routes.rs" > "$d/r" && mv "$d/r" "$d/src/routes.rs"
expect "no Host check fails" "$d" 1 "no Host check: development answers any Host name"

d=$(make_app routes); : > "$d/docs/routes.md"
expect "a stale docs/routes.md only warns" "$d" 0 "docs/routes.md is missing ROUTES path(s): /notes/{id}"
d=$(make_app feature-routes); mkdir -p "$d/src/routes"
printf '#[rustfmt::skip]\npub const ROUTES: &[Route] = &[\n    ("GET",  "/posts", "posts::index", Access::User),\n];\n' > "$d/src/routes/posts.rs"
expect "a feature routes file's rows are checked against docs/routes.md too" "$d" 0 "docs/routes.md is missing ROUTES path(s): /posts"

# Long ad-hoc class lists on buttons and inputs: a warning, never an error; silenced per line.
d=$(make_app classes); mkdir -p "$d/templates/posts" "$d/templates/components"
cat > "$d/templates/posts/index.html" <<'EOF2'
<button class="btn">Save</button>
<button type="submit" class="inline-flex items-center rounded-md bg-indigo-600 px-4 py-2 text-sm font-medium text-white hover:bg-indigo-500">Go</button>
<a href="/x" class="rounded-md bg-indigo-600 px-3 py-1 font-medium text-white hover:bg-indigo-500 focus:outline-none focus-visible:ring-2">Sign up</a>
<a href="/y" class="text-sm text-gray-700 dark:text-gray-300 underline underline-offset-2 hover:text-gray-900 dark:hover:text-white focus:outline-none">a plain link</a>
<input name="q" class="mt-1 block w-full rounded-md border border-gray-300 px-3 py-2 text-sm {{ extra }}">
<button class="inline-flex items-center rounded-md bg-white px-4 py-2 text-sm font-medium text-gray-900 shadow-sm">Ok</button>{# offrails-check: classes ok (the hero's white button) #}
EOF2
cp "$d/templates/posts/index.html" "$d/templates/components/ui.html"
expect "long class lists on buttons and inputs only warn, and can be silenced" "$d" 0 "0 error(s)" \
  "WARN [classes] templates/posts/index.html:2: <button> with 10 classes" \
  "WARN [classes] templates/posts/index.html:3: <a> with 9 classes" \
  "WARN [classes] templates/posts/index.html:5: <input> with 9 classes" "3 warning(s)"
out=$(sh "$CHECK" "$d" 2>&1 || true)
if printf '%s\n' "$out" | grep -qE 'index.html:(1|4|6):|\[classes\] templates/components'; then
  failures=$((failures + 1)); printf 'FAIL classes: flagged a short list, a plain link, a silenced line or the kit itself\n%s\n' "$out"
else
  printf 'ok   classes: short lists, plain links, silenced lines and the kit itself are not flagged\n'
fi

# A line a part ships as it is (its files/ copy, or its sample.toml wiring) is the part's: skipped by
# [classes]; a line the app wrote or edited is checked like any other.
d=$(make_app classes-part); mkdir -p "$d/templates/sessions" "$d/.offrails/template/parts/auth-x/files/templates/sessions"
printf '[package]\nname = "fixture_app"\n' > "$d/Cargo.toml"
cat > "$d/.offrails/template/parts/auth-x/files/templates/sessions/new.html" <<'EOF2'
{# offrails_app::sessions #}
<button type="submit" class="inline-flex items-center rounded-md bg-indigo-600 px-4 py-2 text-sm font-medium text-white hover:bg-indigo-500">Sign in</button>
EOF2
sed 's/offrails_app/fixture_app/' "$d/.offrails/template/parts/auth-x/files/templates/sessions/new.html" > "$d/templates/sessions/new.html"
cp "$d/templates/sessions/new.html" "$d/templates/sessions/edited.html"
cp "$d/.offrails/template/parts/auth-x/files/templates/sessions/new.html" "$d/.offrails/template/parts/auth-x/files/templates/sessions/edited.html"
perl -pi -e 's/>Sign in</>Log in</' "$d/templates/sessions/edited.html"
# A line another part wires into a file (its sample.toml's text) is the part's too.
mkdir -p "$d/templates/shared"
printf '[[wire]]\nfile = "templates/shared/_nav.html"\ntext = \047\047\047\n<a href="/x" class="rounded-md bg-indigo-600 px-3 py-1 font-medium text-white hover:bg-indigo-500 focus:outline-none focus-visible:ring-2">Join</a>\n\047\047\047\n' > "$d/.offrails/template/parts/auth-x/sample.toml"
printf '<nav>\n  <a href="/x" class="rounded-md bg-indigo-600 px-3 py-1 font-medium text-white hover:bg-indigo-500 focus:outline-none focus-visible:ring-2">Join</a>\n</nav>\n' > "$d/templates/shared/_nav.html"
expect "a part's unmodified line is skipped by [classes], an edited one is checked" "$d" 0 "0 error(s)" \
  "WARN [classes] templates/sessions/edited.html:2: <button> with 10 classes" \
  "skipped 2 line(s) the installed parts ship" "1 warning(s)"
out=$(sh "$CHECK" "$d" 2>&1 || true)
if printf '%s\n' "$out" | grep -qE 'sessions/new.html|shared/_nav.html'; then
  failures=$((failures + 1)); printf 'FAIL classes-part: flagged a line the part ships\n%s\n' "$out"
else
  printf 'ok   classes-part: the line the part ships is not flagged\n'
fi

# hx-trigger filters need eval (allowEval: false): a warning.
d=$(make_app htmx-filter); mkdir -p "$d/templates/posts"
cat > "$d/templates/posts/search.html" <<'EOF2'
<input name="q" hx-get="/posts" hx-trigger="keyup changed delay:300ms">
<input name="r" hx-get="/posts" hx-trigger="keyup[key=='Enter']">
EOF2
expect "an hx-trigger filter only warns" "$d" 0 "0 error(s)" \
  "WARN [htmx] templates/posts/search.html:2: an hx-trigger filter" "1 warning(s)"

echo
if [ "$failures" = 0 ]; then
  echo "offrails-check: all fixture cases behave"
else
  echo "offrails-check: $failures fixture case(s) misbehave"
  exit 1
fi
