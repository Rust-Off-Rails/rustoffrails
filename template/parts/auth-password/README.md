# auth-password

Email + password accounts, done the way current guidance says (NIST SP 800-63B, the OWASP cheat sheets): sign-up with email verification, sign-in, password reset, change password, and the protections around them. It is the `ACCOUNTS: signup` variant of the framework's auth guide, built on [auth-core](../auth-core/) (sessions, account page, re-auth, security emails, audit trail).

- **argon2id with OWASP's parameters** (19 MiB, 2 iterations, 1 lane), run on the blocking pool; hashes made with older parameters are **upgraded on the next sign-in**.
- **Sign-up that never reveals existing accounts**: every submit says "check your email"; a new address gets a verification link, an existing one gets "you already have an account". Nobody is signed in before the address is verified.
- **Pre-hijacking defense**: the latest sign-up wins for an unverified address, and a verification link opened in another browser asks for the sign-up password, so a victim clicking an attacker's link can't confirm the attacker's account. With auth-magic-link too, the owner signing in by link deletes the unverified account's password (`auth::claim_unverified_account`, wired by this sample).
- **Sign-in with no enumeration**: one argon2 verify on every path (a dummy hash for unknown addresses), the same queries, the same 422 page.
- **Progressive delays per account** (5 free failures, then 30 s, 1 m, 2 m … up to 15 m), identical for unknown addresses, plus a per-network limit; refused attempts never extend a pause; the owner is emailed when sign-in is paused, and a **password reset unlocks** at once.
- **Reset by single-use, hashed, 30-minute links**, minted inside the background task that sends them; a reset or change of password kills every older link and ends every other session.
- **Change password** needs the current password *and* a recent re-auth; this device stays signed in with a fresh token, every other device is signed out, and the owner is emailed.
- **Passwords people can live with**: 12 to 72 characters, spaces and paste welcome, no composition rules; a **bundled common-password check** (offline, ~400 entries plus leetspeak, sequences, repeats, the email's name and the site's name).
- **A strength meter that works without JavaScript** (rendered by the server after a submit) and live while typing with htmx (a POST, never a URL); **show/hide password**, a **Caps Lock hint**, `autocomplete` hints for password managers (`username`, `current-password`, `new-password`).

Extracted from Rust off Rails apps and improved: the originals had no password variant in production; this one is built to the same standard as their magic links, and goes past Rails 8's generator (no verification, no throttle per account, no strength or breach check), `phx.gen.auth` (no progressive delays, no strength meter, no pre-hijacking defense) and Devise's defaults (lockable/confirmable off by default, enumeration on by default).

Guides: [`.offrails/guides/auth.md`](https://rustoffrails.org/docs/auth), [`.offrails/guides/security.md`](https://rustoffrails.org/docs/security). Depends on: [auth-core](../auth-core/).

## Screenshots

![Sign-up with the strength meter, light, desktop](../../docs/screenshots/auth-password/sign-up-strength-light-desktop.png)

| Screen | Light | Dark |
|--------|-------|------|
| Sign up | [desktop](../../docs/screenshots/auth-password/sign-up-light-desktop.png) · [mobile](../../docs/screenshots/auth-password/sign-up-light-mobile.png) | [desktop](../../docs/screenshots/auth-password/sign-up-dark-desktop.png) · [mobile](../../docs/screenshots/auth-password/sign-up-dark-mobile.png) |
| Strength meter | [desktop](../../docs/screenshots/auth-password/sign-up-strength-light-desktop.png) · [mobile](../../docs/screenshots/auth-password/sign-up-strength-light-mobile.png) | [desktop](../../docs/screenshots/auth-password/sign-up-strength-dark-desktop.png) · [mobile](../../docs/screenshots/auth-password/sign-up-strength-dark-mobile.png) |
| A refused password | [desktop](../../docs/screenshots/auth-password/sign-up-errors-light-desktop.png) · [mobile](../../docs/screenshots/auth-password/sign-up-errors-light-mobile.png) | [desktop](../../docs/screenshots/auth-password/sign-up-errors-dark-desktop.png) · [mobile](../../docs/screenshots/auth-password/sign-up-errors-dark-mobile.png) |
| Check your email | [desktop](../../docs/screenshots/auth-password/check-email-light-desktop.png) · [mobile](../../docs/screenshots/auth-password/check-email-light-mobile.png) | [desktop](../../docs/screenshots/auth-password/check-email-dark-desktop.png) · [mobile](../../docs/screenshots/auth-password/check-email-dark-mobile.png) |
| Verify (another browser) | [desktop](../../docs/screenshots/auth-password/verify-other-browser-light-desktop.png) · [mobile](../../docs/screenshots/auth-password/verify-other-browser-light-mobile.png) | [desktop](../../docs/screenshots/auth-password/verify-other-browser-dark-desktop.png) · [mobile](../../docs/screenshots/auth-password/verify-other-browser-dark-mobile.png) |
| Sign in | [desktop](../../docs/screenshots/auth-password/sign-in-light-desktop.png) · [mobile](../../docs/screenshots/auth-password/sign-in-light-mobile.png) | [desktop](../../docs/screenshots/auth-password/sign-in-dark-desktop.png) · [mobile](../../docs/screenshots/auth-password/sign-in-dark-mobile.png) |
| Forgot password | [desktop](../../docs/screenshots/auth-password/forgot-password-light-desktop.png) · [mobile](../../docs/screenshots/auth-password/forgot-password-light-mobile.png) | [desktop](../../docs/screenshots/auth-password/forgot-password-dark-desktop.png) · [mobile](../../docs/screenshots/auth-password/forgot-password-dark-mobile.png) |
| Choose a new password | [desktop](../../docs/screenshots/auth-password/reset-light-desktop.png) · [mobile](../../docs/screenshots/auth-password/reset-light-mobile.png) | [desktop](../../docs/screenshots/auth-password/reset-dark-desktop.png) · [mobile](../../docs/screenshots/auth-password/reset-dark-mobile.png) |
| Change password | [desktop](../../docs/screenshots/auth-password/change-password-light-desktop.png) · [mobile](../../docs/screenshots/auth-password/change-password-light-mobile.png) | [desktop](../../docs/screenshots/auth-password/change-password-dark-desktop.png) · [mobile](../../docs/screenshots/auth-password/change-password-dark-mobile.png) |

## Install

Through the samples MCP server: `get_sample auth-password`. By hand, in the app root (install [auth-core](../auth-core/) first, which brings dev-mailbox and mailer):

1. `cargo add argon2` (never with a version; argon2 0.6 hashes with `hash_password(pw)`, no salt argument).
2. Copy `files/` into the app, keeping the paths. Replace `offrails_host` with your crate name in the test file.
3. Wiring, each at its `samples: <anchor>` comment (exact text in `sample.toml`):
   - `src/models/mod.rs`: `password_credential`, `password_policy`.
   - `src/controllers/mod.rs`: `account_passwords`, `password_sessions`, `passwords`, `registrations`; `src/views/mod.rs`: `passwords`.
   - `src/routes.rs`: the routes below, their `paths::` helpers and `ROUTES` rows.
   - auth-core's files: the password form in `templates/sessions/new.html` (`sign in methods`) and the sign-up link (`sign in links`); "Sign up" in `templates/shared/_auth_nav.html`; the password card on the account page (`account fields` in `src/views/auth.rs`, `account load`/`account init` in `src/controllers/account.rs`, `account sections` in `templates/account/show.html`); the password form on "Confirm it's you" (`confirm fields`, `confirm load`, `confirm init`, `confirm methods`); the demo password in `src/tasks/auth_demo.rs`; the password's removal at `claim unverified account` in `src/controllers/auth.rs`.
   - `migration/src/lib.rs`: the `password_credentials` migration.
   - `src/kernel/redact.rs`: `/email-verification/{token}`, `/passwords/{token}/edit` and `/passwords/{token}` in `TOKEN_ROUTES`, so logs show `[redacted]` for them.
   - `tests/app/main.rs`: `mod auth_password_test;`; `tests/app/factories.rs`: `PASSWORD` and `user_with_password()`.
4. `cargo fmt --all && cargo run -- db migrate && cargo run -q -- routes`, then the Definition of Done.

`tools/install.sh auth-password <app>` does it all, auth-core included.

## After installing

- `ACCOUNTS: signup` in `AGENTS.md`; log the install in `docs/decisions.md`.
- Production needs a real mail provider (`SMTP_URL`): verification and resets are emails.
- Try it: `cargo run -- task auth_demo` prints `demo@example.com` / `not-a-real-password-42` (development only).

## How the samples combine

| You install | You get |
|-------------|---------|
| auth-password | email + password, with everything above |
| auth-password + [auth-totp](../auth-totp/) | the code step after the password (and after a reset); re-auth then needs a code; a password change also forgets remembered devices |
| auth-password + [auth-magic-link](../auth-magic-link/) | the sign-in page shows the password form, then "or email me a sign-in link"; magic-link accounts can **add** a password from the account page ("Set a password"); either proof works for re-auth |
| all three | both first factors, then the code step |

Each sample wires into auth-core's anchors, so install order doesn't matter (`tools/install.sh` puts the password form first on the sign-in page either way).

## Adapting it

- **Invite-only accounts** (`ACCOUNTS: invite`): remove the `/registration*` routes and the "Sign up"/"Create an account" links (the `auth nav links` and `sign in links` wiring); create users from an admin screen or a task with `user::find_or_create` + `password_credential::set`, and send them a reset link (`passwords::send_reset`) to choose a password.
- **Stronger hashing**: raise `M_COST_KIB` / `T_COST` in `src/models/password_credential.rs` (`// APP:`); existing hashes upgrade on sign-in. Keep a sign-in under ~250 ms on your servers.
- **A bigger breach list**: append lines to `src/models/common_passwords.txt` (`// APP:`), or check a k-anonymity breach API in `password_policy::problem` (it's network and a third party: decide it explicitly and log it in `docs/decisions.md`).
- **Length rules**: `MIN_CHARS` / `MAX_CHARS` in `password_policy.rs` (`// APP:`). Never lower the minimum below 12.
- **Throttle pace**: auth-core's `auth_throttle.rs` (`// APP:`); per-network limits in `password_sessions.rs` and `registrations.rs` (`// APP:`).
- **Require two-factor for admins**: install auth-totp, then in `auth::sign_in`'s caller (or in your `RequireAdmin`), refuse admin pages until `totp_credential::enabled_for` is true and send them to `/account/two-factor/new`.

## Do not change

- One argon2 verify on every sign-in path, unknown accounts included (`unknown_and_known_emails_take_the_same_path`).
- Sign-up and reset responses identical for known and unknown addresses (`signup_with_an_existing_email_looks_identical`, `reset_requests_look_identical_for_unknown_emails`).
- Verification outside the sign-up browser asks for the password; the latest sign-up wins (`verification_in_another_browser_needs_the_password`, `the_latest_signup_wins_for_an_unverified_address`).
- Reset links: single use, 30 minutes, dead after any password change; a reset ends every session (`a_reset_link_works_once`, `reset_links_expire_and_die_when_the_password_changes`, `a_reset_ends_every_other_session`).
- Change password needs the current one and re-auth (`changing_the_password_needs_the_current_one`, `password_reauth_confirms_and_returns`).
- `SignInParams`'s hand-written `Debug` and no `Debug` on the other params (`sign_in_params_debug_hides_the_password`, `passwords_and_reset_tokens_never_reach_the_logs`).
- The strength meter is a POST (`the_strength_meter_works_by_post_only`).

## How it works

| Route | Handler | Access |
|-------|---------|--------|
| `POST /session/password` | sign in | public |
| `GET /registration/new`, `POST /registration`, `GET /registration/sent` | sign up, "check your email" | public |
| `GET`/`POST /email-verification/{token}` | verify (one click in the sign-up browser, else the password) | public (token) |
| `GET /passwords/new`, `POST /passwords`, `GET /passwords/sent` | forgot password | public |
| `GET /passwords/{token}/edit`, `POST /passwords/{token}` | choose a new password | public (token) |
| `POST /password-strength` | the meter fragment (htmx) | public |
| `GET /account/password/edit`, `POST /account/password` | change or set a password | user, recent re-auth |
| `POST /account/confirm/password` | re-auth with the password | user |

`password_credentials` is its own table (`user_id`, the PHC string, `changed_at`), so `users` stays the same whichever methods are installed. Sign-in, verification and reset all end in auth-core's `auth::sign_in`, which is where two-factor (if installed) takes over.

## Files

| File | What |
|------|------|
| `src/models/password_credential.rs` | argon2id hash, verify (with the dummy), rehash, set |
| `src/models/password_policy.rs`, `src/models/common_passwords.txt` | length, common passwords, patterns; the strength estimate |
| `src/controllers/password_sessions.rs` | sign in; re-auth with the password |
| `src/controllers/registrations.rs` | sign up; email verification |
| `src/controllers/passwords.rs` | forgot/reset; the strength meter |
| `src/controllers/account_passwords.rs` | change or set a password |
| `src/views/passwords.rs` | page structs |
| `templates/password_sessions/`, `registrations/`, `passwords/`, `account_passwords/`, `account/_password_section.html` | the screens and partials |
| `migration/src/m20261016_002000_create_password_credentials.rs` | the table |
| `tests/app/auth_password_test.rs` | the tests named in CHECKS.md |
