# CLI: the `app` binary is `bin/rails`

Every app compiles to one binary with `clap` subcommands. The binary is named after the crate. These docs call it `app`.

## Subcommands to generate in `src/main.rs`

```
app serve                       # rails server       (default when no subcommand)
app worker [--queues default:10,mailers:5]  # bin/jobs  (queue producers + Stager/Lifeline/Pruner/Cron)
app db migrate                  # rails db:migrate   (embedded Migrator::up)
app db rollback [--steps N]     # rails db:rollback  (Migrator::down)
app db status                   # rails db:migrate:status (Migrator::get_migration_with_status; print `{:?}` of `m.status()`: "Applied/Pending <name>". MigrationStatus isn't in the prelude, and Debug needs no import)
app db seed                     # rails db:seed      (src/tasks/seed.rs; idempotent)
app db setup                    # rails db:setup     (migrate + seed)
app db reset                    # Migrator::fresh (drop all tables, migrate) + seed; refuses unless APP_ENV=development
app db console                  # rails dbconsole    (runs sqlite3 <path> or psql <url>, based on DATABASE_URL; no pool opened)
app routes                      # rails routes       (prints ROUTES with access levels, rewrites docs/routes.md; no DB needed)
app task <name> [args…]         # rails runner / rake tasks (src/tasks/<name>.rs)
app jobs stats|list|show|retry|cancel|delete|run|pause|resume|prune   # Mission Control / Oban Web, in a terminal
app health                      # container healthcheck: GET /up on 127.0.0.1:$PORT, exit 0 on 200 (no DB, no curl in the image)
app task tokens create <email> [name] | list <email> | revoke <id>   # API tokens (apps with an API; guides/api.md)
```

Verified (reference app): `Migrator::up(&db, None)`, `Migrator::down(&db, Some(steps))`, `Migrator::fresh(&db)` and `Migrator::get_migration_with_status(&db)` from `sea-orm-migration` 2.0.4. `main` parses the CLI, loads `Config::from_env()`, initializes tracing, then connects. `routes` and `db console` run before connecting.

`serve` binds `0.0.0.0:$PORT`, serves `into_make_service_with_connect_info::<SocketAddr>()` (rate limiting needs the client IP), and shuts down gracefully on Ctrl-C/SIGTERM. It also runs the worker in-process when `APP_INLINE_WORKER=true`, like `SOLID_QUEUE_IN_PUMA`. That's the default in development, and in production on SQLite, so a single process does everything.

Every subcommand reads `DATABASE_URL`, so **any command works against any database**:

```sh
cargo run -- db migrate                                   # local SQLite
DATABASE_URL=postgres://… cargo run -- db status          # a remote Postgres, from your laptop
```

## Migrations on deploy

The binary embeds the `migration` crate. The Docker entrypoint (`bin/docker-entrypoint`, generated) runs `app db migrate` and then `exec`s the given command, like Rails 8's `db:prepare` on boot. Postgres migrations take an advisory lock, so concurrent boots are safe. On SQLite there is one server by definition. Platform-specific "release command" alternatives are in `guides/deployment.md`.

## Command cheat sheet

| Rails                         | Off Rails |
|-------------------------------|-----------|
| `rails new`                   | `interview.md`, then `generators/new.md` |
| `bin/setup`                   | `bin/setup` (installs tools, creates `.env` from `.env.example`, `db setup`) |
| `bin/dev`                     | `bin/dev`: `cargo watch -x 'run -- serve'` + Tailwind watch (`htmx`) or `pnpm --dir web dev` (`inertia`/`spa`) |
| `rails s`                     | `cargo run -- serve` |
| `rails c`                     | No REPL. Write `src/tasks/<name>.rs` and run `cargo run -- task <name>` |
| `rails dbconsole`             | `cargo run -- db console` |
| `rails db:migrate`            | `cargo run -- db migrate` |
| `rails db:rollback`           | `cargo run -- db rollback` |
| `rails db:migrate:status`     | `cargo run -- db status` |
| `rails g migration X`         | `sea-orm-cli migrate generate <verb>_<noun>` (see `guides/migrations.md`) |
| `rails g …`                   | `generators/<name>.md` |
| `rails routes`                | `cargo run -- routes` (or `/dev/routes` in the browser) |
| Mission Control – Jobs        | `/admin/jobs`, or `cargo run -- jobs stats` / `jobs list --state discarded` ([jobs-dashboard](guides/jobs-dashboard.md)) |
| letter_opener_web             | http://localhost:3000/dev/mailbox ([mailers](guides/mailers.md)) |
| `rails test`                  | `cargo nextest run` (SQLite) / `TEST_DATABASE_URL=postgres://… cargo nextest run` |
| `rubocop`                     | `cargo fmt --check && cargo clippy --all-targets -- -D warnings` |
| `brakeman`                    | `guides/security.md` checklist (+ optional `cargo audit`) |
| `rails assets:precompile`     | `htmx`: `bin/tailwindcss -i assets/app.css -o static/app.css --minify`. `inertia`/`spa`: `pnpm --dir web build` |
| `yarn test`                   | `pnpm --dir web test --run` (Vitest) |
| `kamal deploy` / `fly deploy` | see `guides/deployment.md` |

## Running the app from an agent

- **Start it as a tracked background task** using your harness's mechanism (Claude Code: Bash with `run_in_background`), running `bin/dev` or `cargo run -- serve`. Don't use `nohup … &` from a tool call: the harness can reap it within minutes, and the human then gets "connection refused".
- **Pick a free port** and set `PORT`. In development, `APP_URL` defaults to `http://localhost:{PORT}` ([config](guides/config.md)), so email links and the CSRF origin check follow the port.
- **Prove it's up before telling the human:** `curl -fsS localhost:$PORT/up`. Re-check after any restart.
- **Stop only what you started**, by PID or port (`lsof -ti tcp:$PORT -sTCP:LISTEN`). Never `pkill` by name, which can kill the human's own servers.
- **Read emails through the mailbox JSON** (`/dev/mailbox/latest.json?to=…`), never by grepping raw `.eml` files ([mailers](guides/mailers.md)).

## Tasks (`src/tasks/`)

Each task is `pub async fn run(state: &AppState, args: Vec<String>) -> anyhow::Result<()>`, registered in a `match` in `src/tasks/mod.rs`. Tasks are where data fixes, backfills and admin operations go. They're also how you "use the console": write what you'd have typed in `rails c` as a task, run it, and delete it if it was throwaway. Data-fix tasks get committed so there's a record.
