---
name: deploy
description: Make a Rust off Rails app deployable and ship it (Docker, Fly.io, Kamal, Railway, Render), including production database setup and migrations on deploy. Use when the user asks to deploy, ship, host, or put an Off Rails app in production.
argument-hint: "[fly|kamal|railway|render]"
---

# /offrails:deploy

Only for Rust off Rails apps (a `.offrails/` directory exists).

Target: $ARGUMENTS (otherwise `DEPLOY_TARGET` from the app's `AGENTS.md`, otherwise ask Round 2 Q3 of `.offrails/interview.md`).

1. Read `.offrails/guides/deployment.md`, `.offrails/guides/config.md` and `.offrails/cli.md#migrations-on-deploy`.
2. If the target changed, update `DEPLOY_TARGET` in `AGENTS.md` and log it in `docs/decisions.md`.
3. Generate or update the `Dockerfile`, `bin/docker-entrypoint` and the target's config file only, per the guide. Every `Config` env var must appear in the deploy config, with secrets referenced rather than inlined.
4. Verify locally: `/offrails:check` is green and `docker build .` succeeds (if Docker is available). Run the image against SQLite and `curl /up`.
5. **Stop and confirm before anything outward-facing.** List the exact commands (creating the app or database, setting secrets, deploying), what each one costs or changes, and the secrets the human must supply. Never invent or echo secret values. Run them only after an explicit yes.
6. After deploying, `curl https://<host>/up`, check the deploy logs for `db migrate`, and report the URL.
