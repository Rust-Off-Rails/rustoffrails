# Generator: `authentication` (`rails generate authentication` / `phx.gen.auth`)

> **TL;DR**
> - Implements `guides/auth.md` for the `ACCOUNTS` variant (`signup`, `invite`, `magic-link`, `magic-link-invite`); write `product/features/accounts.md` first.
> - Steps in order, compiling at each one: crates → migrations → kernel (`session.rs`, rate limits) → models → extractors → error shaping → jobs that mint tokens → mailer → controllers → routes → views → dev seed → tests.
> - Tests: one per `AC-accounts-<n>`, plus cookie flags, no token in job args, idle-session expiry, the dev mailbox hint (dev only), the magic-link list from auth.md, and the app's first real-browser smoke (`tests/e2e/`).
>
> Read it all when adding accounts to an app.

Implements the design in [`guides/auth.md`](../guides/auth.md). Read that first. This recipe is the file checklist, in an order that compiles at every step. The `signup` variant was executed end to end in the reference app (htmx) on 2026-10-12, green on SQLite and Postgres.

## Inputs (from the interview, recorded in the app's `AGENTS.md`)

- `ACCOUNTS`: `magic-link` | `magic-link-invite` | `signup` | `invite` (table in [auth.md#variants](../guides/auth.md#variants-accounts-in-the-apps-agentsmd))
- `ROLES`: none | admin+member | custom
- `TENANCY`: per-user | teams | shared (never `none` here: that means no accounts)

## 0. Spec first

Write `product/features/accounts.md` from [`blueprints/feature-spec.md`](../blueprints/feature-spec.md), with `AC-accounts-<n>` criteria for each behavior below that applies (sign-up, duplicate email, password length, login, same message for bad email and bad password, logout ends the session, rate limit, reset request is enumeration-safe, link single-use and expiry, reset logs out other sessions, return-to). Record assumptions, e.g. "no email confirmation while EMAIL is not yet".

## Steps

1. **Crates:**
   - password variants: `cargo add argon2` (0.6; default features);
   - all variants: nothing else (`rand`, `sha2`, `base64` come with `new`).
2. **Migrations**, one each (`sea-orm-cli migrate generate …`), in this order because of the foreign keys:
   - `create_users`: `big_pk_auto`, `email` + unique `users_email_key`, `password_digest` (`string`; `string_null` for magic-link), `role` if roles, timestamps;
   - `create_sessions`: FK cascade + `sessions_user_id_idx`, unique `sessions_token_digest_key`, `ip`/`user_agent` as `string_null`, `created_at`, `last_seen_at`;
   - then `create_password_resets` (password variants) **or** `create_login_links` (magic-link: `email`, unique digest, `expires_at`, `created_at`, index on `email`);
   - teams: `create_accounts`, `create_memberships` (`account_id`, `user_id`, `role`, unique pair).

   Run `cargo run -- db migrate`, then prove `db rollback --steps N && db migrate`.
3. **Kernel:**
   - `kernel/session.rs`: `generate_token`, `digest`, `read`/`read_cookie`, and the hand-formatted `cookie`/`clear_cookie`/`removal`;
   - `kernel/return_to.rs`: `cookie`, `read` (validated), `clear`, `is_safe`.

   Both get unit tests (token length and uniqueness, cookie flags, `//` and `/\` rejected).
4. **Models:**
   - `user.rs`: entity without `Serialize`; `SignupParams` + `validate`; `normalize_email`; `check_password`; `hash_password`/`verify_password` on `spawn_blocking`; `find`; `find_by_email`; `create` (unique violation → "has already been taken"); `authenticate` with the `LazyLock` dummy digest; `update_password`;
   - `session.rs`: `start → (Model, raw)`, `find_by_token(db, raw, now)` (one `find_also_related` query, idle-expiry filter), `touch` (hourly), `destroy`, `destroy_all_for_user`, `delete_expired`;
   - `password_reset.rs`: `issue(db, user_id, now) → raw`, `find_valid`, `consume` (validate the password, then one transaction: update, delete resets and sessions), `delete_stale`;
   - **or** `login_link.rs`: `issue(db, email, now) → raw`, `find_valid`, `consume(db, raw, now) → Option<(User, Session, raw_token)>` (one transaction: delete the row, find or create the user with `ON CONFLICT DO NOTHING`, start the session), `delete_expired`.

   Register them in `models/mod.rs` with `pub type User = user::Model;`.
5. **Extractors** in `controllers/application.rs`:
   - `load_session` with the `LoadedSession` extension cache;
   - `Ctx` gains `current_user` and `dev_mailbox`;
   - `CurrentUser`, `RequireUser`, `CurrentSession`, `UserAgent`;
   - `RequireAdmin` if roles (and switch `AdminAccess` to it);
   - `CurrentAccount` if teams.
6. **Error shaping:** in `error::shape_errors`, capture the GET path (not `/api`) before running the request. On `Unauthorized`, append `return_to::cookie(path, secure)` to the redirect.
7. **Jobs:**
   - `jobs/password_reset_email.rs` (`PasswordResetEmailJob { user_id }`) **or** `jobs/login_link_email.rs` (`LoginLinkEmailJob { email }`). Each issues the token in `perform`, renders the mail, calls `state.mailer.deliver`. `MAX_ATTEMPTS = 5`; `Outcome::Cancel` when the user is gone.
   - Register the job in `dispatch` and `worker_config`.
   - Add `session::delete_expired` and `password_reset::delete_stale` / `login_link::delete_expired` to `CleanupJob`.
8. **Mailer:** `mailers/passwords.rs::reset` (or `mailers/login_links.rs::login`) with `templates/mailers/<mailer>/<action>.{html,txt}` extending the mail layout. Use absolute links built from `config.app_url` + `paths::`. Add a preview to `PREVIEWS` in `src/mailers/previews.rs` with a fake user and a placeholder token, never a real one ([mailer generator](mailer.md) step 4; shown in the mailbox's Previews tab at `/dev/mailbox/previews`). The reference app shipped without one, and the dev index showed "0 mailer previews". A test renders every `PREVIEWS` entry.
9. **Controllers:**
   - `sessions.rs`: `new`, `create` (password variants), `destroy`;
   - `registrations.rs`: `new`, `create` (`signup` only; user + session in one transaction);
   - `passwords.rs`: `new`, `create`, `edit`, `update`;
   - **or** `login_links.rs`: `create`, `sent`, `show` (confirm page), `consume`;
   - `spa`: `controllers/api/…` returning JSON, plus `GET /api/session`.

   Rate-limit login, reset and link requests (10 per 3 minutes per IP + action; magic links also per email). Re-render the form with **429** and "Try again later."
10. **Routes:** the table in [auth.md](../guides/auth.md#routes-password-variants) (magic-link: [its flow table](../guides/auth.md#flow)):
    - `paths::` helpers, including `dev_mailbox()` if the dev tools haven't added it;
    - `ROUTES` rows: `Access::Public`, except log-out (`Access::User`).
11. **Views / pages:**
    - htmx: `views/{sessions,registrations,passwords}.rs` (or `login_links.rs`) and their templates;
    - `templates/shared/_dev_mailbox_hint.html`, included at the top of every auth screen (mandatory on `/login_links/sent`);
    - nav: "Log in" + "Sign up", or the reader's email + a "Log out" form;
    - inertia/spa: the matching pages plus a `DevMailboxHint` component fed by the `dev_mailbox` prop.
12. **Seed** (development only; `tasks/seed.rs` refuses to run in production): create `reader@example.com` (`admin@example.com` if roles) with a random password printed once. Skip it if the account already exists. Magic-link: no password; log in through `/dev/mailbox`.
13. **Tests:**
    - `tests/app/support.rs` gains `login_as(&user)` (real `session::start` + jar cookie), `cookie(name)`, `set_cookie`, `logout_locally()`, `assert_text`, `without_fetch_metadata()`/`with_origin()` ([testing](../guides/testing.md));
    - `tests/app/factories.rs` gains `create_user` with the shared one-time digest;
    - `tests/app/accounts_test.rs` has one test per `AC-accounts-<n>`, plus:
      - cookie flags (`Secure` only with production config);
      - logged-in readers are redirected away from the login and sign-up forms;
      - the job args hold no token;
      - an idle session expires;
      - the dev mailbox hint appears in development and is absent in production and test configs;
    - magic-link: the required list in [auth.md](../guides/auth.md#required-tests).
    - **the app's first real-browser smoke** (it's the first critical flow): `tests/e2e/` for htmx (`web/e2e/` in TS modes) per [test-standards §9](../guides/test-standards.md#9-real-browser-smoke), with one spec: sign up or request a link → open it from `/dev/mailbox/latest.json` → logged in → log out. `page.on('dialog', d => d.accept())` in the setup, so a `data-confirm` never hangs it.
14. Log decisions in `docs/decisions.md` (variant, any deviation). Run the [security checklist](../guides/security.md#checklist), then the Definition of Done. Smoke the flows in a real browser: sign up, log out, log in, and request a reset and open it from `/dev/mailbox` (see [test-standards §9](../guides/test-standards.md#9-real-browser-smoke)).
