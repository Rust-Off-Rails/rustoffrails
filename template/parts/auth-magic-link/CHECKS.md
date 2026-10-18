# auth-magic-link: checks

What "working and safe" means for passwordless sign-in, and the test that attacks each control. Written for an adversarial reviewer. The shared layer is in [auth-core's CHECKS.md](../auth-core/CHECKS.md). `tools/security-check.sh` fails when a test named here doesn't exist; `tools/ci.sh auth-magic-link` runs them on SQLite and Postgres.

## Threat model (STRIDE)

| STRIDE | Threat | Control | Test |
|--------|--------|---------|------|
| Spoofing | Login CSRF: an attacker sends the victim a link to the attacker's own account, so the victim works inside it | A link works only in the browser that asked for it (default); otherwise a 403 page that sends them to the code | `a_link_opened_in_another_browser_is_refused_by_default` |
| Spoofing | Brute-forcing the 6-digit code | The code works only in the asking browser; 5 wrong codes burn the link (counted before comparing, atomically); wrong codes also count **per address across every link, browser and network** in `auth_throttles` (5 free, then pauses doubling from 30 s to 15 min; sign-in and re-auth codes share it), so new links buy no new guesses; 30 guesses per network per 3 minutes; 10 links per address per 3 minutes and 20 per day | `a_code_allows_five_wrong_guesses_then_burns_the_link`, `code_guesses_are_throttled_per_address_across_links`, `the_code_signs_in_only_in_the_browser_that_asked`, `code_guesses_are_capped_per_network`, `requests_are_rate_limited_per_network_and_per_address_across_networks`, `link_requests_are_capped_per_address_per_day` |
| Spoofing | Account pre-hijacking: an attacker signed the address up with a password (auth-password) and waits for the owner to sign in by link, which would verify the account with the attacker's password on it | Using a link or code calls `auth::claim_unverified_account`: an unverified account loses its password, sessions, tokens and two-factor before it is verified | `a_password_set_before_verification_dies_when_the_owner_signs_in_by_link`, `a_password_set_before_verification_dies_when_the_owner_signs_in_by_code` |
| Spoofing | A leaked database is used to sign in | Token stored as SHA-256, code as HMAC under `APP_SECRET_KEY` | `only_token_and_code_digests_are_stored` |
| Spoofing | A re-auth code (sent to Ada) confirms someone else, or signs anyone in | Re-auth codes are a separate purpose, bound to the browser and to the signed-in person's address; the sign-in code form ignores them | `a_reauth_code_only_confirms_its_own_account`, `a_reauth_code_cant_sign_anyone_in` |
| Tampering | A link is used twice (replay, or two clicks racing) | The DELETE inside the session transaction is the claim | `a_link_works_once_even_when_used_twice_at_the_same_time`, `a_new_link_cancels_the_previous_one` |
| Tampering | Mail scanners or link previews use up the link | GET renders a confirm button; only POST consumes | `opening_a_link_twice_then_posting_still_signs_in` |
| Tampering | An old link works for too long | 15 minutes; a new request cancels earlier links | `links_expire_after_fifteen_minutes`, `a_new_link_cancels_the_previous_one` |
| Repudiation | First sign-ins and failed codes leave no trace | `signed_up`, `signed_in` and `sign_in_failed` events | `a_link_signs_in_and_creates_the_account_on_first_use` |
| Information disclosure | Enumeration: does this address have an account? | Every valid address gets the same redirect, body and cookies, and the request does the same database work; under `Signup::Open` both get an email (first use creates the account), under `Signup::Invite` the account lookup runs in the background send, after the response | `known_and_unknown_addresses_get_identical_responses` |
| Spoofing | Self-registration in an invite-only app (`Signup::Invite`) | An unknown address gets no email and no link row (no code to guess); a link that reaches `finish` for an address without an account (minted before the switch) is refused, never an account | `invite_only_magic_links_go_only_to_existing_accounts`, `invite_only_magic_links_never_create_accounts` |
| Information disclosure | Tokens or codes in logs | Minted in the background task, never in log fields; a test captures every log line | `link_tokens_and_codes_never_reach_the_logs` |
| Information disclosure | The token leaks through caches or `Referer` | Confirm and wrong-browser pages are `no-store` + `no-referrer` | `opening_a_link_twice_then_posting_still_signs_in` |
| Information disclosure | A development link points at another port (a stale `APP_URL`), sending the token to whatever listens there | Links are built from `config.app_url`, which follows the served address in development | `links_follow_the_app_url_being_served_not_a_stale_port`, `the_email_has_an_absolute_link_and_a_code` |
| Denial of service | Email flooding of one address | 10 requests per address per 3 minutes and 20 per day, across networks | `requests_are_rate_limited_per_network_and_per_address_across_networks`, `link_requests_are_capped_per_address_per_day` |
| Elevation of privilege | A link alone bypasses two-factor | Using a link ends in `auth::sign_in`, where auth-totp intercepts; emailed re-auth codes are refused when two-factor is on | `a_password_alone_doesnt_sign_in_when_two_factor_is_on`, `two_factor_users_must_step_up_with_a_code` |
| Elevation of privilege | A session that is merely old changes the email | Re-auth by emailed code, rotating the session | `emailed_code_reauth_confirms_identity`, `step_up_rotates_the_session_token` |

## ASVS-style checklist (OWASP ASVS 4.0, V2.7 out-of-band verifiers)

| ASVS | Requirement | Status | Test |
|------|-------------|--------|------|
| V2.7.2 | Out-of-band tokens expire (≤ 10 minutes recommended; 15 here, the framework default) | 15 minutes | `links_expire_after_fifteen_minutes` |
| V2.7.3 | Single use | Yes | `a_link_works_once_even_when_used_twice_at_the_same_time` |
| V2.7.4 | Bound to the original request | Browser binding | `a_link_opened_in_another_browser_is_refused_by_default`, `the_code_signs_in_only_in_the_browser_that_asked` |
| V2.7.6 | ≥ 20 bits of entropy for codes, rate limited | 6 digits (~20 bits), 5 tries per link | `a_code_allows_five_wrong_guesses_then_burns_the_link` |
| V2.8.4 | One-time values used once | Yes | `a_link_works_once_even_when_used_twice_at_the_same_time` |
| V2.2.1 | Anti-automation | Per network and per address | `requests_are_rate_limited_per_network_and_per_address_across_networks` |
| V3.2.1 | New session on authentication | Yes | `a_link_signs_in_and_creates_the_account_on_first_use` |

## Invariants

| Invariant | Test |
|-----------|------|
| First use creates exactly one verified account | `a_link_signs_in_and_creates_the_account_on_first_use` |
| Invalid addresses are a 422 on the field (a format error, not enumeration) | `invalid_addresses_are_a_422_with_the_error_on_the_field` |
| Sign-in by link honors return-to | `sign_in_by_link_honors_return_to` |
| The sent page has the code form, and the dev mailbox hint in development only | `the_sent_page_shows_the_code_form_and_the_dev_mailbox_hint_in_development` |
| Browser binding can be lifted by configuration only | `any_browser_can_be_allowed_by_config` |
| The sign-in and "check your mailbox" pages follow the theme switch | `magic_link_pages_follow_the_theme_switch` |

## Known limits (accepted, documented)

- **Whoever reads the mailbox can sign in.** That's the design of email sign-in; two-factor (auth-totp) removes it.
- **The emailed link's URL appears in request lines** at debug level in development (see auth-core's CHECKS.md). The link is single-use, POST-consumed and short-lived.
- **The code is 6 digits.** Wrong codes count per address across links: 5 free, then one guess per pause (30 s, doubling to 15 minutes), so after the first few minutes an attacker gets about 100 guesses a day against one address: an expected hit takes on the order of 10^4 days. The daily cap of 20 links per address bounds the mail.
- **Someone can pause code entry for your address.** Wrong codes for an address pause code entry for everyone using that address (that is what stops the guessing). The emailed link itself is not throttled, so the owner signs in by clicking it; the pause lasts at most 15 minutes after the last wrong code.
- **The daily cap is per process** (`RateLimiter`, in memory); the wrong-code throttle is in the database and holds across processes and restarts.

## Commands

```sh
TEST_DATABASE_URL=postgres://… tools/ci.sh auth-magic-link
tools/security-check.sh
```
