# auth-core: checks

What "working and safe" means for the shared accounts layer, and the test that proves each item. Written for an adversarial reviewer: every control names the test that attacks it. `tools/security-check.sh` fails when a test named here doesn't exist; `tools/ci.sh auth-password` (which installs this sample) runs them on SQLite and Postgres.

## Threat model (STRIDE)

| STRIDE | Threat | Control | Test |
|--------|--------|---------|------|
| Spoofing | A stolen database dump is used to log in | Only SHA-256 digests of session and emailed tokens are stored; short codes use HMAC under `APP_SECRET_KEY` | `only_token_digests_are_stored`, `keyed_digests_depend_on_key_and_purpose` |
| Spoofing | Session fixation: an attacker plants a known session cookie before the victim signs in | Every sign-in deletes this browser's previous session and issues a new token | `sign_in_rotates_the_session_and_kills_the_old_token`, `a_session_planted_before_sign_in_is_worthless_after_it` |
| Spoofing | A sibling subdomain or an http response plants a `session_token` cookie | Production reads and writes only `__Host-` cookies (Secure, Path=/, no Domain) | `production_ignores_a_planted_unprefixed_session_cookie`, `production_auth_cookies_are_host_prefixed_and_secure` |
| Spoofing | A token is guessed | 256-bit tokens from the OS CSPRNG | `tokens_are_long_unique_and_url_safe` |
| Spoofing | A token comparison leaks its prefix through timing | Digest lookups; in-memory comparisons go through `constant_time_eq` | `constant_time_eq_compares_content` |
| Tampering | Cross-site requests change state (CSRF) | tower-http `CsrfLayer` (Fetch Metadata + Origin) on every POST; GETs never change state | `cross_site_posts_to_auth_routes_are_refused`, `cross_site_posts_to_every_unsafe_route_are_403` |
| Tampering | A crafted `return_to` cookie redirects to another site after sign-in | Only `/path` values, never `//`, `\`, control characters or absolute URLs | `return_to_rejects_open_redirects`, `only_local_paths_are_safe`, `a_tampered_cookie_is_ignored` |
| Tampering | A guessed id revokes someone else's session | Revocation is scoped by `user_id` in the query; another user's id is a 404 | `revoking_another_users_session_is_404` |
| Tampering | Two first sign-ins race and abort a Postgres transaction | `INSERT … ON CONFLICT DO NOTHING` then select | `find_or_create_twice_in_one_transaction_keeps_it_usable` |
| Repudiation | A takeover or a change can't be reconstructed | `auth_events`: sign-ins, failures, revocations, re-auth, email/password/2FA changes, "not me", with IP, user agent and device digest | `the_sessions_list_shows_devices_and_revokes_one_or_all_others`, `sign_out_kills_the_token_server_side`, `step_up_rotates_the_session_token`, `not_me_signs_out_everywhere` |
| Information disclosure | Tokens or secrets in logs | Tokens never go into log fields, job args or URLs we log at info; a test captures every log line at trace level | `session_tokens_never_reach_the_logs` |
| Information disclosure | Token URLs leak through caches, history and `Referer` | Token pages send `Cache-Control: no-store` and `Referrer-Policy: no-referrer`, and canonical/`og:url` never carry the token | `token_pages_are_not_cached_and_send_no_referrer` |
| Information disclosure | Private pages land in a shared or back/forward cache | Every response to a request with a session is `no-store`; the full header matrix | `signed_in_pages_are_no_store_and_carry_the_header_matrix` |
| Information disclosure | The email-change form reveals which addresses have accounts | A taken address gets the same response and no email | `an_email_change_to_a_taken_address_looks_the_same_and_sends_nothing` |
| Information disclosure | The dev mailbox link shows in production | Rendered only with `APP_ENV=development` and the local mailer | `auth_screens_link_to_the_dev_mailbox_in_development_only` |
| Denial of service | Unlimited password/code guessing | Progressive per-key delays, counted before the check under a row lock; refused attempts don't count | `parallel_guesses_are_counted_up_front`, `refused_attempts_do_not_extend_the_pause`, `throttle_delays_grow_then_cap`, `a_success_forgives_and_a_quiet_day_forgets` |
| Denial of service | An email flood through "change email" | 5 per account and 20 per network per hour | `changing_the_email_needs_the_new_address_to_confirm_and_warns_the_old_one` (limits in `account::update_email`) |
| Elevation of privilege | A stolen session (or an unlocked laptop) changes the email or password for good | `RequireRecentAuth`: a proof within 10 minutes; re-auth rotates the token | `sensitive_actions_require_recent_auth`, `step_up_rotates_the_session_token` |
| Elevation of privilege | An email change moves the account to an attacker's address | Re-auth, then a link to the new address; the old address is warned with a "this wasn't me" link; old reset links die | `changing_the_email_needs_the_new_address_to_confirm_and_warns_the_old_one`, `a_reset_link_sent_to_an_old_address_dies_with_the_change` |
| Elevation of privilege | An attacker keeps access after the owner notices | "This wasn't me" ends every session and trusted device and cancels a pending email change, single use, works signed out; password change and reset cancel pending email changes too | `not_me_signs_out_everywhere`, `expired_and_wrong_purpose_tokens_are_refused`, `credential_changes_and_not_me_forget_trusted_devices`, `a_password_change_cancels_a_pending_email_change`, `a_password_reset_cancels_a_pending_email_change` |
| Elevation of privilege | Account pre-hijacking: someone registers the address first (a password sign-up never verified) and waits for the owner to verify it by another method | `auth::claim_unverified_account`: whoever proves the address verifies the account and, if it was unverified, drops its sessions, emailed tokens and every credential a sample wires in (password, two-factor), in one transaction | `a_password_set_before_verification_dies_when_the_owner_signs_in_by_link`, `a_password_set_before_verification_dies_when_the_owner_signs_in_by_code`, `claiming_an_unverified_account_drops_its_two_factor` |
| Elevation of privilege | An idle or very old stolen cookie keeps working | 30-day idle and 90-day absolute expiry in the lookup query | `expired_sessions_log_nobody_in` |
| Elevation of privilege | A signed-out visitor reaches a private page | `RequireUser`/`CurrentSession` on every user route; the security suite requests every `Access::User` route anonymously | `the_account_page_needs_a_session_and_remembers_where_you_were_going`, `non_public_routes_never_answer_2xx_anonymously`, `htmx_requests_without_a_session_get_hx_redirect` |
| Elevation of privilege | Production boots with a guessable or missing secret key | `APP_SECRET_KEY` is required in production and must be ≥ 32 characters; tests get a random key | `the_secret_key_must_be_long_enough_and_set_in_production` |
| Elevation of privilege | The demo account (known password) exists in production | `auth_demo` refuses outside development | `the_demo_task_refuses_outside_development` |

## ASVS-style checklist (OWASP ASVS 4.0, the items this layer owns)

| ASVS | Requirement | Status | Test |
|------|-------------|--------|------|
| V3.1.1 | Session tokens never in URLs | Cookie only | `production_auth_cookies_are_host_prefixed_and_secure` |
| V3.2.1 | New session token on authentication | Yes | `sign_in_rotates_the_session_and_kills_the_old_token` |
| V3.2.2 | ≥ 64 bits of entropy | 256 bits | `tokens_are_long_unique_and_url_safe` |
| V3.2.3 | Tokens stored securely in the browser | `HttpOnly`, `Secure`, `SameSite=Lax`, `__Host-` | `production_auth_cookies_are_host_prefixed_and_secure`, `flags` |
| V3.3.1 | Logout invalidates the session server-side | Row deleted | `sign_out_kills_the_token_server_side` |
| V3.3.2 | Idle and absolute timeouts | 30 days idle, 90 days absolute | `expired_sessions_log_nobody_in` |
| V3.3.3 | Option to end all other sessions after a credential change | Automatic on password change/reset; button on the account page | `the_sessions_list_shows_devices_and_revokes_one_or_all_others`, `not_me_signs_out_everywhere` |
| V3.3.4 | Users can view and log out of active sessions | Account page | `the_sessions_list_shows_devices_and_revokes_one_or_all_others` |
| V3.4.1–3.4.3 | Cookie `Secure`, `HttpOnly`, `SameSite` | Yes | `flags`, `production_auth_cookies_are_host_prefixed_and_secure` |
| V3.4.4 | `__Host-` prefix | Yes (production) | `production_names_are_host_prefixed` |
| V3.7.1 | Re-authentication before sensitive changes | `RequireRecentAuth` | `sensitive_actions_require_recent_auth` |
| V2.5.7 / V2.2.3 | Notify the user of credential changes and new sign-ins | Security emails with "this wasn't me" | `security_notices_go_out_with_an_absolute_not_me_link`, `notices_carry_an_absolute_not_me_link_and_no_markup_injection` |
| V2.2.1 | Anti-automation | Progressive throttle + per-network limits | `parallel_guesses_are_counted_up_front` |
| V7.1.1 | No credentials or tokens in logs | Yes | `session_tokens_never_reach_the_logs` |
| V8.2.1 | Anti-caching headers on sensitive pages | `no-store` | `signed_in_pages_are_no_store_and_carry_the_header_matrix` |
| V4.2.2 | CSRF protection | `CsrfLayer` | `cross_site_posts_to_auth_routes_are_refused` |
| V5.1.5 | Redirects only to allowed destinations | Local paths | `return_to_rejects_open_redirects` |

## Invariants

| Invariant | Test |
|-----------|------|
| The account page lists every session, marks this one, and its query count doesn't grow with them | `the_sessions_list_shows_devices_and_revokes_one_or_all_others`, `the_account_page_query_count_is_stable` |
| Security emails have an HTML and a text part, absolute links, and escape what they show | `notices_carry_an_absolute_not_me_link_and_no_markup_injection` |
| Background emails finish before `settle()` returns | `idle_waits_for_spawned_work` |
| Emails are normalized (trimmed, lowercased) everywhere | `normalizes_and_validates` |
| Auth pages use the app layout and follow the kernel theme switch (`data-theme` from the cookie) | `auth_pages_follow_the_theme_switch` |
| `static/auth.js` loads once, from the app layout, on every page, and binds only on `document`: the Show button, Caps Lock hint, code auto-submit, copy and print work after a boosted link and after an htmx swap (a form re-rendered after a failed attempt); in a real browser, `tools/e2e.sh` arrives at each page through a boosted click | `auth_js_loads_from_the_layout_on_every_page`, `app_pages_load_no_scripts_or_stylesheets_in_their_head_block` |
| Every handler that deletes session rows calls `auth::sessions_ended` after its commit (realtime wires its socket revocation there), signing in over another session included | `every_session_ending_path_calls_sessions_ended`, `signing_in_over_a_session_closes_its_sockets` |

## Known limits (accepted, documented)

- **Request lines at debug level.** The host's `TraceLayer` logs request URIs at `debug`, which in development includes the emailed token of a link being opened. Production logs at `info` (no URIs). Every emailed token is single-use, short-lived and consumed by POST, so a logged URL is useless once used. Suggested host change: redact `{token}` path segments in the trace span.
- **Lookups by digest are not constant-time.** A database index lookup on a SHA-256 or HMAC digest can only leak information about the digest, which reveals nothing usable about the token.
- **Background email on shutdown.** An email queued in the last moment before SIGTERM can be lost; the person asks again. With the jobs sample, move the tasks into jobs whose args carry ids only.
- **Device labels come from the User-Agent** and are display-only; nothing security-relevant depends on them.
- **The per-network limits** (`RateLimiter`) are per process; the per-account throttle is in the database and holds across processes.

## Commands

```sh
TEST_DATABASE_URL=postgres://… tools/ci.sh auth-password    # installs auth-core too; Definition of Done on both databases
tools/security-check.sh                                    # every test named here exists
```
