# auth-magic-link

Passwordless sign-in (Phoenix 1.8's default, the framework's `ACCOUNTS: magic-link` variant): enter your email, then **click the link or type the 6-digit code** from the same email. The first use creates the account, or, invite-only (`ACCOUNTS: magic-link invite`), links go only to accounts that exist. Built on [auth-core](../auth-core/) (sessions, account page, re-auth, security emails, audit trail).

- **A link and a code in one email.** Phones often open mail links in another browser (the mail app's); the code lets you finish where you started.
- **Single-use, hashed, 15-minute** links and codes, minted inside the background task that sends them. Only the SHA-256 of the token and an HMAC of the code (keyed by `APP_SECRET_KEY`) are stored.
- **Race-safe**: using a link deletes its row in the same transaction that starts the session; of two simultaneous clicks exactly one signs in.
- **Scanner-safe**: opening the link shows "Continue as ada@example.com" with a button; only the POST signs in, so mail scanners and link previews can't burn it.
- **Bound to the asking browser, by default**: the link and the code only work in the browser that asked for them. That stops **login CSRF** (an attacker mailing you a link to *their* account) and forwarded links. The code covers the other-device case; `APP_MAGIC_LINK_ANY_BROWSER=true` lifts the binding if you must.
- **5 wrong codes burn the link**, and wrong codes count **per address across every link** in `auth_throttles` (growing pauses), plus a per-network cap on guesses; requests are limited per network **and per address across networks** (10 per 3 minutes each, 20 per address per day).
- **Pre-hijacking safe with passwords**: signing in by link or code to an account someone registered but never verified (auth-password) drops that account's password, sessions and tokens first (`auth::claim_unverified_account`).
- **No enumeration**: every valid address gets the same redirect and the request does the same work; under open sign-up both get an email (the first use creates the account).
- **Open or invite-only** from auth-core's one switch (`Signup`, `APP_AUTH_SIGNUP`): invite-only, an unknown address gets no email and no link (decided in the background send, so it can't be timed), a link never creates an account, and `app task auth_invite <email>` lets people in.
- **Re-auth by emailed code** for sensitive actions ("Email me a code" on "Confirm it's you"); refused when two-factor is on.
- **The dev mailbox hint** on "Check your email" in development, and links built from the address actually served, so a development link never points at a stale port.

Extracted from Rust off Rails apps and improved: the originals had single-use hashed links, the confirm page and the dev hint; this adds the code, browser binding (login-CSRF defense), attempt limits, a per-address limit across networks, re-auth by code, security emails and a stale-port regression test.

Guides: [`.offrails/guides/auth.md`](https://rustoffrails.org/docs/guides/auth#magic-link-variants-magic-link-magic-link-invite), [`.offrails/guides/security.md`](https://rustoffrails.org/docs/guides/security). Depends on: [auth-core](../auth-core/).

## Screens

<a href="../../docs/screenshots/auth-magic-link/sent-light-desktop.png"><picture><source media="(prefers-color-scheme: dark)" srcset="../../docs/screenshots/auth-magic-link/sent-dark-desktop.png"><img src="../../docs/screenshots/auth-magic-link/sent-light-desktop.png" alt="Check your email, with the code form" width="860"></picture></a>

<table>
<tr><td valign="top"><a href="../../docs/screenshots/auth-magic-link/email-light-desktop.png"><picture><source media="(prefers-color-scheme: dark)" srcset="../../docs/screenshots/auth-magic-link/email-dark-desktop.png"><img src="../../docs/screenshots/auth-magic-link/email-light-desktop.png" alt="The sign-in email with link and code, in the dev mailbox" width="420"></picture></a><br><sub>The email</sub></td><td valign="top"><a href="../../docs/screenshots/auth-magic-link/confirm-light-desktop.png"><picture><source media="(prefers-color-scheme: dark)" srcset="../../docs/screenshots/auth-magic-link/confirm-dark-desktop.png"><img src="../../docs/screenshots/auth-magic-link/confirm-light-desktop.png" alt="Continue as, the scanner-safe confirmation" width="420"></picture></a><br><sub>Confirm the link (scanners can't burn it)</sub></td></tr>
</table>

<sub>Light or dark follows your GitHub theme; click an image for full size.</sub>

| Screen | Light | Dark |
|--------|-------|------|
| Sign in | [desktop](../../docs/screenshots/auth-magic-link/sign-in-light-desktop.png) · [mobile](../../docs/screenshots/auth-magic-link/sign-in-light-mobile.png) | [desktop](../../docs/screenshots/auth-magic-link/sign-in-dark-desktop.png) · [mobile](../../docs/screenshots/auth-magic-link/sign-in-dark-mobile.png) |
| Check your email (code) | [desktop](../../docs/screenshots/auth-magic-link/sent-light-desktop.png) · [mobile](../../docs/screenshots/auth-magic-link/sent-light-mobile.png) | [desktop](../../docs/screenshots/auth-magic-link/sent-dark-desktop.png) · [mobile](../../docs/screenshots/auth-magic-link/sent-dark-mobile.png) |
| The email (dev mailbox) | [desktop](../../docs/screenshots/auth-magic-link/email-light-desktop.png) · [mobile](../../docs/screenshots/auth-magic-link/email-light-mobile.png) | [desktop](../../docs/screenshots/auth-magic-link/email-dark-desktop.png) · [mobile](../../docs/screenshots/auth-magic-link/email-dark-mobile.png) |
| Confirm the link | [desktop](../../docs/screenshots/auth-magic-link/confirm-light-desktop.png) · [mobile](../../docs/screenshots/auth-magic-link/confirm-light-mobile.png) | [desktop](../../docs/screenshots/auth-magic-link/confirm-dark-desktop.png) · [mobile](../../docs/screenshots/auth-magic-link/confirm-dark-mobile.png) |
| Opened in another browser | [desktop](../../docs/screenshots/auth-magic-link/other-browser-light-desktop.png) · [mobile](../../docs/screenshots/auth-magic-link/other-browser-light-mobile.png) | [desktop](../../docs/screenshots/auth-magic-link/other-browser-dark-desktop.png) · [mobile](../../docs/screenshots/auth-magic-link/other-browser-dark-mobile.png) |

## Install

By hand, in the app root (install [auth-core](../auth-core/) first):

1. No crates beyond auth-core's.
2. Copy `files/` into the app, keeping the paths. Replace `offrails_host` with your crate name in the test file.
3. Wiring, each at its `samples: <anchor>` comment (exact text in `sample.toml`):
   - `src/models/mod.rs`: `login_link`; `src/controllers/mod.rs`: `login_links`; `src/views/mod.rs`: `login_links`.
   - `src/config.rs`: a `magic_link: MagicLinkConfig` field (`APP_MAGIC_LINK_ANY_BROWSER`) with its section, parse, test and `Debug` lines.
   - `src/routes.rs`: the routes below, their `paths::` helpers and `ROUTES` rows.
   - auth-core's files: the email form in `templates/sessions/new.html` (`sign in alternatives`); "Email me a code" in `templates/account/confirm.html` (`confirm alternatives`); expired links in `src/tasks/auth_cleanup.rs`.
   - `migration/src/lib.rs`: the `login_links` migration; `.env.example`: `APP_MAGIC_LINK_ANY_BROWSER`.
   - `src/kernel/redact.rs`: `/login_links/{token}` in `TOKEN_ROUTES`, so logs show `[redacted]` for it.
   - `tests/app/main.rs`: `mod auth_magic_link_test;`.
   - With auth-password installed: `mod auth_magic_link_password_test;` in `tests/app/main.rs` (the cross-method pre-hijacking tests; the file is copied either way and compiled only then).
4. `cargo fmt --all && cargo run -- db migrate && cargo run -q -- routes`, then the Definition of Done.

`tools/install.sh auth-magic-link <app>` does it all, auth-core included.

## After installing

- `ACCOUNTS: magic-link` in `AGENTS.md` (`password+magic-link`, `magic-link invite`, …: auth-core's README, "After installing"); log the install in `docs/decisions.md`.
- Production needs a real mail provider (`SMTP_URL`) before launch: this is the only way in.
- Try it in development: enter any `@example.com` address, then open `/dev/mailbox` (the page links to it).

## How the samples combine

| You install | You get |
|-------------|---------|
| auth-magic-link | link-or-code sign-in, sign-up on first use |
| auth-magic-link + [auth-totp](../auth-totp/) | the code step after the link or code; re-auth then needs an authenticator code (emailed codes are refused) |
| auth-magic-link + [auth-password](../auth-password/) | the sign-in page shows the password form, then "or email me a sign-in link"; magic-link accounts can add a password from the account page |
| all three | both first factors, then the code step |

## Adapting it

- **Invite-only** (`ACCOUNTS: magic-link invite`): set `DEFAULT_SIGNUP = Signup::Invite` in `src/config.rs` (`// APP:`, auth-core). Nothing else changes in code or tests (the tests that rely on a first use creating the account ask for `Signup::Open`). Invite people with `app task auth_invite <email>`; the invitation's button opens the sign-in page.
- **Shorter links or fewer guesses**: `TTL` and `MAX_CODE_ATTEMPTS` in `src/models/login_link.rs` (`// APP:`).
- **Links that work in any browser** (an app whose users mostly open mail on another device and can't type codes): `APP_MAGIC_LINK_ANY_BROWSER=true`. This re-opens login CSRF; log the decision.
- **Request limits**: `LIMIT` and `WINDOW` in `src/controllers/login_links.rs` (`// APP:`).
- **Email wording**: `send_link` in `src/controllers/login_links.rs` (`// APP:`), rendered through auth-core's one auth mail template.

## Do not change

- GET never signs in (`opening_a_link_twice_then_posting_still_signs_in`).
- The claim is the DELETE inside the session transaction (`a_link_works_once_even_when_used_twice_at_the_same_time`).
- Browser binding on by default; codes only in the asking browser (`a_link_opened_in_another_browser_is_refused_by_default`, `the_code_signs_in_only_in_the_browser_that_asked`).
- The attempt is counted before the code is compared (`a_code_allows_five_wrong_guesses_then_burns_the_link`), and counted against the address too (`code_guesses_are_throttled_per_address_across_links`).
- `finish` claims an unverified account with `auth::claim_unverified_account`, never a bare `user::mark_verified` (`a_password_set_before_verification_dies_when_the_owner_signs_in_by_link`).
- Identical responses for known and unknown addresses, in both sign-up modes (`known_and_unknown_addresses_get_identical_responses`); invite-only, the account check stays in the background send and `finish` never creates an account (`invite_only_magic_links_go_only_to_existing_accounts`, `invite_only_magic_links_never_create_accounts`).
- Tokens and codes are minted in the background task; nothing is logged (`link_tokens_and_codes_never_reach_the_logs`).
- Links come from `config.app_url` (`links_follow_the_app_url_being_served_not_a_stale_port`).

## How it works

| Route | Handler | Access |
|-------|---------|--------|
| `POST /login_links` | ask for a link + code (303 to `/login_links/sent`, same for every address; invite-only, sent only to accounts) | public |
| `GET /login_links/sent` | "Check your email", with the code form and the dev hint | public |
| `POST /login_links/code` | sign in with the code (asking browser only) | public |
| `GET /login_links/{token}` | "Continue as …" (never signs in) | public (token) |
| `POST /login_links/{token}` | use the link | public (token) |
| `POST /account/confirm/email` | email me a re-auth code | user |
| `POST /account/confirm/email/code` | re-auth with it | user |

`login_links` stores the email (not a user id, so the same flow signs up), the purpose (`sign_in` or `reauth`), the token digest, the code's HMAC, the asking browser's cookie digest, the attempt count and the expiry. Using a link or code ends in auth-core's `auth::sign_in`, so rotation, return-to, new-device emails and two-factor all apply.

## Files

| File | What |
|------|------|
| `src/models/login_link.rs` | mint, find, claim, `check_code` (counted, constant-time), browser binding |
| `src/controllers/login_links.rs` | request, sent page, code, confirm page, consume; re-auth by code |
| `src/views/login_links.rs` | the confirm and wrong-browser pages |
| `templates/login_links/` | sign-in form, confirm page, wrong-browser page, re-auth partial |
| `migration/src/m20261016_003000_create_login_links.rs` | the table |
| `tests/app/auth_magic_link_test.rs` | the tests named in CHECKS.md |
