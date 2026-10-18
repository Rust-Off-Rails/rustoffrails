# Accounts (`rails generate authentication` / `phx.gen.auth`): the auth parts

> **TL;DR**
> - Accounts are **parts**, not a recipe to type: `cargo offrails add auth-core auth-password` (email + password), `auth-core auth-magic-link` (passwordless), both, and `auth-totp` for two-factor. They bring users, database sessions, sign-in, the account security page, security emails, the audit trail, throttling and their own tests ([the parts](../template/README.md#choosing-parts), [samples](https://github.com/Rust-Off-Rails/samples)).
> - Before: write `product/features/accounts.md`, and pick the variant from the interview (`ACCOUNTS: signup | magic-link | magic-link-invite | invite`). After: the header (`ACCOUNTS`, `TENANCY`), `APP_SECRET_KEY`, where people land after sign-in, your own handlers scoped to the user, the tenancy suites (the first scoped `g resource` writes them), and the browser smoke.
> - What's left is product, not plumbing: invite-only sign-up, roles, teams, the spec's own acceptance tests. Each is listed below with where it goes.
>
> Read it all when adding accounts to an app.

Design: [`guides/auth.md`](../guides/auth.md) (the why, the flows, the threat model) and [`guides/security.md`](../guides/security.md). The parts implement it, tested; this page is the order of work and what the parts leave to you. Before 0.7.0 this page was a 14-step checklist for writing it all by hand; don't.

## 0. Spec and variant

Write `product/features/accounts.md` from [`blueprints/feature-spec.md`](../blueprints/feature-spec.md): one `AC-accounts-<n>` per behavior the product promises (who may sign up, how people sign in, where they land, what an admin can do), and the STRIDE `## Security` table. The parts' suites already prove the generic contract (cookie flags, rotation, throttling, enumeration safety, single-use links); your criteria cite their tests where they cover one, and get a test of their own where they don't.

| `ACCOUNTS` (interview) | Parts | Then |
|------------------------|-------|------|
| `signup` | `auth-core auth-password` | done: sign-up, verification, reset, sign-in |
| `magic-link` | `auth-core auth-magic-link` | done: sign-up on first link |
| `invite` | `auth-core auth-password` | make sign-up invite-only ([below](#invite-only)) |
| `magic-link-invite` | `auth-core auth-magic-link` | make sign-up invite-only ([below](#invite-only)) |
| any, plus two-factor | `+ auth-totp` | optional per user; required for admins is yours ([below](#roles-and-teams)) |

`auth-core` needs `mailer` and `dev-mailbox`; `add` installs them first.

## 1. Add the parts

```sh
cargo offrails add auth-core auth-password --dry-run   # what it will do
cargo offrails add auth-core auth-password
cargo run -q -- db migrate                             # then prove it: db rollback --steps <n> && db migrate
```

`add` updates the `ACCOUNTS` header line; set `TENANCY` yourself (`per-user` for records people own, `teams` with teams). Read each part's README, "After installing" and "Adapting it", and its CHECKS.md.

## 2. What the parts leave to you

1. **Configuration.** `APP_SECRET_KEY` (≥ 32 characters, `openssl rand -base64 48`) in the deploy config: production refuses to boot without it. `SMTP_URL` for real email (verification, resets and links are emails). Schedule `app task auth_cleanup` daily (the jobs part's `recurring.rs`, or cron).
2. **Where people land.** `paths::after_sign_in()` (`// APP:` in `src/routes.rs`): the app's home for signed-in people.
3. **Your handlers.** `RequireUser(user)` on every private page, `RequireRecentAuth` on anything that changes credentials or money, and an owner-scoped query for every record (`<model>::find_for(db, &user, id)`). With accounts installed, `cargo offrails g resource …` writes exactly that, plus `tests/app/guard_test.rs` and `tests/app/tenancy_test.rs` on the first scoped resource ([scaffold](scaffold.md)); hand-written handlers follow the same shape ([models](../guides/models.md#scoped-lookups-tenancy)).
4. **The nav.** The parts add "Sign in" and the account link at the `samples: nav links` anchor; the rest of the signed-in nav is yours.
5. **The browser smoke.** `tests/e2e/smoke.scenario.mjs` with `login(page, base)` (the real form; magic links: read the link from `/dev/mailbox/latest.json`) and `loggedIn(page)`; then `SCENARIO=tests/e2e/smoke.scenario.mjs BASE=<the URL bin/dev printed> bash .offrails/tools/browser-smoke/run.sh login` with no certificate overrides, and `… run.sh a11y` in light and dark ([test-standards §9](../guides/test-standards.md#9-real-browser-smoke)). `verify --full` signs in as `demo@example.com` from `cargo run -- task auth_demo`, or as `SMOKE_EMAIL`/`SMOKE_PASSWORD`.
6. **Seeds.** `cargo run -- task auth_demo` makes the development demo account; your own seed accounts go in `src/tasks/seed.rs`, development only (refuse in production).

### Invite-only

`ACCOUNTS: invite` (or `magic-link-invite`): no public sign-up. With `auth-password`, remove the `/registration*` routes and the "Sign up" links (the part's `auth nav links` and `sign in links` wiring), create users from an admin screen or a task (`user::find_or_create` + `password_credential::set`), and send them a reset link (`passwords::send_reset`) to choose a password. With `auth-magic-link`, refuse a link for an email that has no account yet (the part creates accounts on first use). Either way: an `AC-accounts-<n>` that a stranger can't create an account, with its test, and the part's tests that sign up (they assume open sign-up) adapted, not deleted. Check the part's README first: a newer version may ship invite-only as an option.

### Roles and teams

Not in the parts (they're product decisions):

- **Roles:** a `role` column on `users` (a migration), `RequireAdmin` next to `RequireUser` in `src/controllers/auth.rs`, and `AdminAccess` (the kernel's admin pages: `/admin/jobs`, the dashboard) switched to it ([auth](../guides/auth.md), [security](../guides/security.md)). Two-factor for admins: refuse admin pages until `totp_credential::enabled_for` is true.
- **Teams:** `accounts` and `memberships` tables, `CurrentAccount`, every query scoped to the account instead of the user, and the tenancy suite's `Tenant` holding the account ([test-standards §4](../guides/test-standards.md#4-tenant-isolation-suite-every-app-with-accounts)).

## 3. Done

Log the variant and any deviation in `docs/decisions.md` (`add` already logged the parts). Run the [security checklist](../guides/security.md#checklist), then the [Definition of Done](../workflow.md#definition-of-done) on SQLite and Postgres, and smoke the flows in a real browser: sign up (or request a link), sign out, sign in, a reset opened from `/dev/mailbox`.
