# auth-core

The accounts layer every auth sample shares: users, database sessions, the account security page and the safety machinery around them. It has **no sign-in form of its own**: install it together with [auth-password](../auth-password/), [auth-magic-link](../auth-magic-link/) or both, and optionally [auth-totp](../auth-totp/) on top. `tools/install.sh` installs it automatically as their dependency.

What you get (the Rails 8 generator, `phx.gen.auth` and Devise each cover part of this; none covers all of it):

- **Database sessions** (Rails 8 style): a random 256-bit cookie token, only its SHA-256 stored; 30 days idle and 90 days absolute expiry; the token **rotates** on every sign-in and every re-authentication; `__Host-` prefixed, `Secure`, `HttpOnly`, `SameSite=Lax` cookies in production.
- **An account security page**: the email address (verified or not), a card per installed method (password, two-factor), **where you're signed in** (device, browser, IP, sign-in method, "active 5 minutes ago"), revoke one device or **sign out everywhere else**, and **recent security activity** from the audit trail.
- **Re-authentication ("sudo mode")** before sensitive actions: `RequireRecentAuth` sends people to "Confirm it's you" when their last proof is older than 10 minutes. With two-factor on, only a code can confirm.
- **Email change** that needs re-auth, a confirmation link to the new address, and a warning to the old one.
- **Security emails** (new sign-in from a new device, password changed, email changed, two-factor on/off, recovery code used, sign-in paused), each with a **"This wasn't me" link** that signs the account out everywhere and forgets trusted devices.
- **An audit trail** (`auth_events`): sign-ins, failures, changes; never a secret.
- **Progressive throttling** (`auth_throttles`): database-backed, counted up front under a row lock, refused attempts never extend the pause, keyed by an HMAC so unknown and known accounts behave identically.
- **Return-to** that only ever redirects to a local path, a dev-mailbox hint on auth screens in development, and a demo task with obviously fake credentials.

Extracted from Rust off Rails apps and improved: the originals had sessions and return-to, but no rotation on re-auth, no absolute expiry, no device list, no audit trail, no security emails and no shared throttle.

Guides: [`.offrails/guides/auth.md`](https://rustoffrails.org/docs/guides/auth), [`.offrails/guides/security.md`](https://rustoffrails.org/docs/guides/security). Depends on: [mailer](../mailer/) (the security emails) and [dev-mailbox](../dev-mailbox/) (the "check your mailbox" hint in development).

## Screens

<a href="../../docs/screenshots/auth-core/account-light-desktop.png"><picture><source media="(prefers-color-scheme: dark)" srcset="../../docs/screenshots/auth-core/account-dark-desktop.png"><img src="../../docs/screenshots/auth-core/account-light-desktop.png" alt="The account security page: email, methods, devices, recent activity" width="860"></picture></a>

<table>
<tr><td valign="top"><a href="../../docs/screenshots/auth-core/sign-in-light-desktop.png"><picture><source media="(prefers-color-scheme: dark)" srcset="../../docs/screenshots/auth-core/sign-in-dark-desktop.png"><img src="../../docs/screenshots/auth-core/sign-in-light-desktop.png" alt="Sign in with every method installed" width="420"></picture></a><br><sub>Sign in (every method installed)</sub></td><td valign="top"><a href="../../docs/screenshots/auth-core/confirm-light-desktop.png"><picture><source media="(prefers-color-scheme: dark)" srcset="../../docs/screenshots/auth-core/confirm-dark-desktop.png"><img src="../../docs/screenshots/auth-core/confirm-light-desktop.png" alt="Confirm it's you before a sensitive action" width="420"></picture></a><br><sub>Confirm it's you (re-auth)</sub></td></tr>
</table>

<sub>Light or dark follows your GitHub theme; click an image for full size.</sub>

| Screen | Light | Dark |
|--------|-------|------|
| Account security | [desktop](../../docs/screenshots/auth-core/account-light-desktop.png) · [mobile](../../docs/screenshots/auth-core/account-light-mobile.png) | [desktop](../../docs/screenshots/auth-core/account-dark-desktop.png) · [mobile](../../docs/screenshots/auth-core/account-dark-mobile.png) |
| Sign in (with every method installed) | [desktop](../../docs/screenshots/auth-core/sign-in-light-desktop.png) · [mobile](../../docs/screenshots/auth-core/sign-in-light-mobile.png) | [desktop](../../docs/screenshots/auth-core/sign-in-dark-desktop.png) · [mobile](../../docs/screenshots/auth-core/sign-in-dark-mobile.png) |
| Confirm it's you | [desktop](../../docs/screenshots/auth-core/confirm-light-desktop.png) · [mobile](../../docs/screenshots/auth-core/confirm-light-mobile.png) | [desktop](../../docs/screenshots/auth-core/confirm-dark-desktop.png) · [mobile](../../docs/screenshots/auth-core/confirm-dark-mobile.png) |
| Change email | [desktop](../../docs/screenshots/auth-core/change-email-light-desktop.png) · [mobile](../../docs/screenshots/auth-core/change-email-light-mobile.png) | [desktop](../../docs/screenshots/auth-core/change-email-dark-desktop.png) · [mobile](../../docs/screenshots/auth-core/change-email-dark-mobile.png) |
| "This wasn't me" | [desktop](../../docs/screenshots/auth-core/not-me-light-desktop.png) · [mobile](../../docs/screenshots/auth-core/not-me-light-mobile.png) | [desktop](../../docs/screenshots/auth-core/not-me-dark-desktop.png) · [mobile](../../docs/screenshots/auth-core/not-me-dark-mobile.png) |
| A used or expired link | [desktop](../../docs/screenshots/auth-core/invalid-link-light-desktop.png) · [mobile](../../docs/screenshots/auth-core/invalid-link-light-mobile.png) | [desktop](../../docs/screenshots/auth-core/invalid-link-dark-desktop.png) · [mobile](../../docs/screenshots/auth-core/invalid-link-dark-mobile.png) |

Regenerate with `node samples/auth-core/screenshots.mjs` against the demo (see the script's header).

## Install

By hand, in the app root, after [dev-mailbox](../dev-mailbox/):

1. `cargo add hmac` (never with a version; read the resolved one in `Cargo.lock`).
2. Copy `files/` into the app, keeping the paths. Replace `offrails_host` with your crate name in the test file.
3. Wiring, each at its `samples: <anchor>` comment (the exact text is in `sample.toml`):
   - `src/kernel/mod.rs`: `auth_token`, `background`, `return_to`, `session`, `user_agent` modules.
   - `src/models/mod.rs`: `auth_event`, `auth_throttle`, `session`, `user`, `user_token`, and `pub type User = user::Model;`.
   - `src/controllers/mod.rs`: `account`, `auth`, `security`, `sessions`.
   - `src/views/mod.rs`: `pub mod auth;` and three `Ctx` fields, `current_user`, `dev_mailbox` and `auth_js` (the digested URL of `static/auth.js`); `src/controllers/application.rs` fills them in `Ctx`'s extractor.
   - `templates/layouts/application.html`: `<script src="{{ ctx.auth_js }}" defer></script>` at `samples: app head`, so every app page loads it. Never in a page's `{% block head %}`: links and forms are boosted, and a boosted click runs nothing in the next page's `<head>` (the Show button, Caps Lock hint and code auto-submit would be dead until a reload).
   - `src/mailers/mod.rs`: `pub mod auth_mailer;`; `src/mailers/previews.rs`: two previews.
   - `src/state.rs`: a `background: Background` field, built with `Background::default()`.
   - `src/config.rs`: an `auth: AuthConfig` field (`APP_SECRET_KEY`) with its section, parse, test and `Debug` lines.
   - `src/routes.rs`: the routes below, their `paths::` helpers and `ROUTES` rows.
   - `templates/shared/_nav.html`: `{% include "shared/_auth_nav.html" %}`.
   - `src/tasks/mod.rs`: the `auth_cleanup` and `auth_demo` tasks.
   - `migration/src/lib.rs`: the four migrations.
   - `.env.example`, `compose.yaml`: `APP_SECRET_KEY`.
   - `src/kernel/redact.rs`: the two token routes (`/account/email/confirm/{token}`, `/security/not-me/{token}`) in `TOKEN_ROUTES`, so logs show `[redacted]` for them.
   - `tests/app/main.rs`: `mod auth_core_test;`; `tests/app/factories.rs`: `user()`; `tests/app/support.rs`: `settle`, `sign_in_as`, `emails_to`, `link_in`.
4. `cargo fmt --all && cargo run -- db migrate && cargo run -q -- routes`, then the Definition of Done (`.offrails/workflow.md`).

`tools/install.sh auth-core <app>` does steps 1 to 3 mechanically.

## After installing

- Install at least one sign-in method: [auth-password](../auth-password/), [auth-magic-link](../auth-magic-link/), or both.
- Set `APP_SECRET_KEY` (at least 32 characters: `openssl rand -base64 48`) in the deploy configuration. Production refuses to boot without it.
- Schedule `app task auth_cleanup` daily (cron, or the jobs sample's scheduler).
- Set `ACCOUNTS:` in `AGENTS.md` (`signup`, `magic-link`, …) and log the install in `docs/decisions.md`.
- Point `paths::after_sign_in()` (`// APP:` in `src/routes.rs`) at your app's home for signed-in people.
- Protect your own handlers: `RequireUser(user)` on every private page, `RequireRecentAuth` on anything that changes credentials or money, and an owner-scoped query (`find_for(db, &user, id)`) for every record.

## Adapting it

- **Longer or shorter sessions**: `IDLE_DAYS` and `ABSOLUTE_DAYS` in `src/kernel/session.rs` (`// APP:`). Keep idle ≤ absolute.
- **A stricter or looser "sudo mode"**: `REAUTH_WINDOW` in `src/controllers/auth.rs` (`// APP:`).
- **Your own sensitive action** (deleting the account, changing payout details): take `RequireRecentAuth` instead of `RequireUser` in its handler, record an `auth_event` (add a `Kind`, `// APP:`), and `auth::notify` the owner with a new `Notice` variant in `src/mailers/auth_mailer.rs`.
- **Rename `User` to `Member`**: rename `src/models/user.rs` and its table in a new migration; the alias `pub type User = user::Model;` in `models/mod.rs` keeps call sites short while you do it.
- **Add OAuth later** ("Sign in with …"): a new controller that verifies the provider's response, then calls `auth::sign_in(state, headers, meta, user, "oauth")`. That one call gives it rotation, return-to, the audit trail, new-device emails and two-factor (with auth-totp). Store the provider identity in its own table, like `password_credentials`.
- **Throttle wording and pace**: `FREE_FAILURES`, `FIRST_PAUSE_SECS`, `MAX_PAUSE_SECS` in `src/models/auth_throttle.rs` (`// APP:`).
- **Security email wording**: `notice()` in `src/mailers/auth_mailer.rs` (`// APP:`).

## Do not change

- `static/auth.js` loads from the app layout on every page, once, and only listens on `document`, so it works after boosted links and htmx swaps (`auth_js_loads_from_the_layout_on_every_page`, `app_pages_load_no_scripts_or_stylesheets_in_their_head_block`, and in a browser `tools/e2e.sh`).
- Tokens are stored as digests only and compared by digest or in constant time (`only_token_digests_are_stored`, `constant_time_eq_compares_content`).
- Every sign-in goes through `auth::sign_in` → `finish_sign_in`, which rotates the session (`sign_in_rotates_the_session_and_kills_the_old_token`, `a_session_planted_before_sign_in_is_worthless_after_it`).
- Production reads only `__Host-` cookies (`production_ignores_a_planted_unprefixed_session_cookie`, `production_auth_cookies_are_host_prefixed_and_secure`).
- `return_to::is_safe` (`return_to_rejects_open_redirects`).
- Scoped revocation (`revoking_another_users_session_is_404`).
- Sensitive routes take `RequireRecentAuth` (`sensitive_actions_require_recent_auth`).
- Throttling counts before checking and never extends a pause (`parallel_guesses_are_counted_up_front`, `refused_attempts_do_not_extend_the_pause`).
- Token pages are `no-store` + `no-referrer` and never echo the token into canonical URLs (`token_pages_are_not_cached_and_send_no_referrer`).
- Tokens are minted inside the background task that sends the email, and no secret is logged (`session_tokens_never_reach_the_logs`).

## How it works

```
any sign-in method ──► auth::sign_in ──[samples: before sign in]──► finish_sign_in
                                    (auth-totp may answer "/two-factor")   │
                                                                           ├ delete this browser's old session (rotation)
                                                                           ├ insert the new session (digest only)
                                                                           ├ audit "signed_in"; new device? → email with "this wasn't me"
                                                                           └ cookies + redirect to return-to (local paths only)
```

Sign-in methods plug in through anchors in this sample's own files: `{# samples: sign in methods #}` / `sign in alternatives` in `templates/sessions/new.html`, `confirm second factor` / `confirm methods` / `confirm alternatives` in `templates/account/confirm.html`, `account fields` / `account load` / `account init` and `account sections` for the account page, `before sign in`, `step-up second factor`, `revoke everything`, `revoke other access` and `claim unverified account` (delete the method's credential when the address's owner claims an unverified account: pre-hijacking) in `controllers/auth.rs`, and `auth events` in `models/auth_event.rs` (`record`: every audit event, where dev-dashboard's optional wire feeds its brute-force and credential-stuffing detectors). That is how the samples combine without editing each other.

Emails go out through `state.background` (no job queue needed): the request returns immediately, the task mints its token and sends. `app.settle()` waits for them in tests.

| Route | Handler | Access |
|-------|---------|--------|
| `GET /session/new` | the sign-in page (one form per installed method) | public |
| `POST /session/delete` | sign out (deletes the session row) | user |
| `GET /account` | account security page | user |
| `GET /account/confirm` | "Confirm it's you" | user |
| `POST /account/sessions/{id}/delete` | sign out one device | user |
| `POST /account/sessions/delete-others` | sign out everywhere else | user |
| `GET /account/email/edit`, `POST /account/email` | change email (re-auth) | user |
| `GET`/`POST /account/email/confirm/{token}` | confirm the new address | public (token) |
| `GET`/`POST /security/not-me/{token}` | "This wasn't me" | public (token) |

## Files

| File | What |
|------|------|
| `src/kernel/auth_token.rs` | random tokens, SHA-256 and keyed digests, constant-time compare, numeric codes |
| `src/kernel/session.rs` | auth cookie names and flags (`__Host-` in production), lifetimes |
| `src/kernel/return_to.rs` | the return-to cookie, local paths only |
| `src/kernel/background.rs` | fire-and-forget email tasks, awaitable in tests |
| `src/kernel/user_agent.rs` | "Firefox on macOS" labels |
| `src/models/user.rs` | accounts, email normalization and validation |
| `src/models/session.rs` | sessions: start, find, touch, rotate, revoke |
| `src/models/user_token.rs` | single-use emailed tokens (verification, reset, email change, not-me) |
| `src/models/auth_event.rs` | the audit trail |
| `src/models/auth_throttle.rs` | progressive delays |
| `src/controllers/auth.rs` | extractors, `sign_in`, `grant_step_up`, `revoke_everything`, `notify` |
| `src/controllers/sessions.rs` | sign-in page, sign out |
| `src/controllers/account.rs` | account page, sessions, re-auth page, email change |
| `src/controllers/security.rs` | "This wasn't me" |
| `src/views/auth.rs` | page structs, `time_ago` |
| `src/mailers/auth_mailer.rs` | one template pair for every auth email; security notices |
| `src/tasks/auth_cleanup.rs`, `src/tasks/auth_demo.rs` | cleanup; development demo accounts |
| `templates/…` | sign-in, account, confirm, email change, not-me, check-email, invalid-link, nav, dev hint, mail |
| `static/auth.js` | show/hide password, Caps Lock hint, one-time-code auto-submit, copy, print: delegated `document` listeners, loaded once by the layout |
| `migration/src/m20261016_0010…_0013…` | users, sessions, user_tokens, auth_events + auth_throttles |
| `tests/app/auth_core_test.rs` | the tests named in CHECKS.md, plus `LogCapture` |
| `screenshots.mjs` (not installed) | the screenshots for all four auth samples |
