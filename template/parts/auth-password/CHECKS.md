# auth-password: checks

What "working and safe" means for email + password accounts, and the test that attacks each control. Written for an adversarial reviewer. The shared layer (sessions, cookies, CSRF, re-auth, "this wasn't me") is in [auth-core's CHECKS.md](../auth-core/CHECKS.md). `tools/security-check.sh` fails when a test named here doesn't exist; `tools/ci.sh auth-password` runs them on SQLite and Postgres.

## Threat model (STRIDE)

| STRIDE | Threat | Control | Test |
|--------|--------|---------|------|
| Spoofing | Online password guessing against one account | 5 free failures, then pauses that double up to 15 minutes, keyed by an HMAC of the address, counted before the check; refused attempts don't count; the owner is emailed | `wrong_passwords_are_throttled_progressively_even_for_unknown_emails`, `the_owner_is_emailed_when_sign_in_is_paused`, `parallel_guesses_are_counted_up_front` |
| Spoofing | Credential stuffing across many accounts from one network | 10 attempts per 3 minutes per network, checked before any argon2 work | `sign_in_is_rate_limited_per_network` |
| Spoofing | Weak or breached passwords | 12+ characters, a bundled common-password list, leetspeak, sequences, repeats, the email's name, the site's name | `weak_passwords_are_refused_at_signup_with_a_reason`, `weak_passwords_are_refused_with_a_reason`, `good_passwords_pass_and_the_meter_ranks_them` |
| Spoofing | Offline cracking of a leaked table | argon2id, 19 MiB / 2 / 1, random salt per hash; old parameters upgraded on sign-in | `hashes_use_argon2id_with_owasp_parameters`, `old_parameters_verify_and_ask_for_a_rehash`, `sign_in_rehashes_passwords_made_with_old_parameters` |
| Spoofing | Account pre-hijacking: an attacker signs up with the victim's address and a password they know | No session before verification; the latest sign-up for an unverified address wins; a link opened outside the sign-up browser needs the sign-up password | `the_latest_signup_wins_for_an_unverified_address`, `verification_in_another_browser_needs_the_password`, `unverified_accounts_cant_sign_in_and_get_a_fresh_link` |
| Spoofing | Pre-hijacking across methods: the attacker's sign-up stays unverified until the owner signs in **by magic link or code** (auth-magic-link), which verifies the account with the attacker's password still on it | `auth::claim_unverified_account` (wired here): proving the address deletes the unverified account's password, sessions and tokens in the same transaction; a sign-up locks the unverified row first, so it can't race the claim | `a_password_set_before_verification_dies_when_the_owner_signs_in_by_link`, `a_password_set_before_verification_dies_when_the_owner_signs_in_by_code`, `a_verified_accounts_password_survives_a_magic_link_sign_in` |
| Spoofing | A sign-up overwrites an existing account's password | Only unverified accounts take a new password | `a_signup_never_overwrites_a_verified_accounts_password`, `signup_with_an_existing_email_looks_identical` |
| Tampering | A reset or verification link is reused or raced | Consuming deletes the row in the transaction that applies it (the delete is the claim) | `a_reset_link_works_once`, `signup_verifies_the_address_before_signing_in` |
| Tampering | An old reset link is used after the password changed | Any password change or reset revokes every reset token; links last 30 minutes | `reset_links_expire_and_die_when_the_password_changes` |
| Repudiation | Failed and successful sign-ins, resets and changes can't be traced | `auth_events` rows for each, with IP and device | `the_owner_is_emailed_when_sign_in_is_paused`, `a_reset_ends_every_other_session` |
| Information disclosure | User enumeration through sign-in (message, status or timing) | Identical page and status; exactly one argon2 verify (counted: `verifies_so_far`) against a dummy hash and the same queries for unknown addresses | `unknown_and_known_emails_take_the_same_path` |
| Information disclosure | Enumeration through sign-up or reset | Identical status, location, body and cookies; the difference is only in the email the owner receives | `signup_with_an_existing_email_looks_identical`, `reset_requests_look_identical_for_unknown_emails` |
| Information disclosure | Enumeration through the throttle | Unknown addresses are throttled exactly like known ones | `wrong_passwords_are_throttled_progressively_even_for_unknown_emails` |
| Information disclosure | Passwords or tokens in logs or URLs | No `Debug` on params with passwords (a redacting one on `SignInParams`); the meter is a POST; a test captures all logs | `passwords_and_reset_tokens_never_reach_the_logs`, `sign_in_params_debug_hides_the_password`, `the_strength_meter_works_by_post_only` |
| Information disclosure | Reset token leaks through caches or `Referer` | Token pages are `no-store` + `no-referrer` and don't echo the token into canonical URLs | `reset_pages_are_not_cached_and_send_no_referrer` |
| Information disclosure | The password is echoed back into the form after an error | Never rendered into `value=` | `weak_passwords_are_refused_at_signup_with_a_reason` |
| Denial of service | Locking a victim out by failing on purpose | Pauses are short and capped (15 minutes), and a password reset unlocks immediately | `a_reset_unlocks_sign_in` |
| Denial of service | Email flooding through sign-up or reset | 3 per address per hour, 10 per network per hour (sign-up) / per 3 minutes (reset) | `signup_is_rate_limited_per_address`, `reset_requests_look_identical_for_unknown_emails` |
| Elevation of privilege | A stolen session changes the password | Current password **and** a recent re-auth; re-auth with the password is throttled | `changing_the_password_needs_the_current_one`, `password_reauth_confirms_and_returns`, `password_reauth_is_throttled` |
| Elevation of privilege | An attacker stays signed in after the owner changes or resets the password | Every other session ends (and trusted devices with auth-totp); this device gets a new token; the owner is emailed | `changing_the_password_signs_out_other_devices_and_keeps_this_one`, `a_reset_ends_every_other_session` |
| Elevation of privilege | An email change the attacker started (its link on the way to the attacker's address) moves the account after the owner changed or reset the password | Change and reset revoke pending email-change tokens (`auth::revoke_other_access`, `auth::revoke_everything`) | `a_password_change_cancels_a_pending_email_change`, `a_password_reset_cancels_a_pending_email_change` |
| Elevation of privilege | A password alone bypasses two-factor | Sign-in ends in `auth::sign_in`, where auth-totp intercepts; password re-auth is refused when two-factor is on | `a_password_alone_doesnt_sign_in_when_two_factor_is_on`, `two_factor_users_must_step_up_with_a_code` |

## ASVS-style checklist (OWASP ASVS 4.0, V2 Authentication)

| ASVS | Requirement | Status | Test |
|------|-------------|--------|------|
| V2.1.1 | At least 12 characters | Yes | `weak_passwords_are_refused_with_a_reason` |
| V2.1.2 | At least 64 characters allowed | 72 (the framework's bound) | `good_passwords_pass_and_the_meter_ranks_them` |
| V2.1.4 | Any printable Unicode, spaces | Yes | `good_passwords_pass_and_the_meter_ranks_them` |
| V2.1.5 | Users can change their password | Yes | `changing_the_password_signs_out_other_devices_and_keeps_this_one` |
| V2.1.6 | Change requires the current password | Yes | `changing_the_password_needs_the_current_one` |
| V2.1.7 | Checked against breached/common passwords | Bundled list, offline | `weak_passwords_are_refused_with_a_reason` |
| V2.1.8 | Strength meter | Server-rendered + htmx | `the_strength_meter_works_by_post_only` |
| V2.1.9 | No composition rules | Yes | `good_passwords_pass_and_the_meter_ranks_them` |
| V2.1.11 | Paste and password managers allowed | `autocomplete` hints, no paste blocking | `sign_in_form_fields_carry_autocomplete_hints` |
| V2.1.12 | Show/hide the password | Yes | `sign_in_form_fields_carry_autocomplete_hints` |
| V2.2.1 | Anti-automation | Per-account + per-network | `wrong_passwords_are_throttled_progressively_even_for_unknown_emails`, `sign_in_is_rate_limited_per_network` |
| V2.2.3 | Notify after credential changes | Yes | `a_reset_ends_every_other_session` |
| V2.4.1 | Approved slow hash (argon2id) | Yes | `hashes_use_argon2id_with_owasp_parameters` |
| V2.4.2 | Salt ≥ 32 bits, unique | 128-bit random salt (argon2 crate) | `hashes_use_argon2id_with_owasp_parameters` |
| V2.5.2 | No password hints or knowledge questions | Yes | (by construction) `weak_passwords_are_refused_with_a_reason` |
| V2.5.4 | No shared or default accounts | The demo account exists only in development | `the_demo_task_refuses_outside_development` |
| V2.5.6 | Reset uses a secure, time-limited, single-use token | 256-bit, hashed, 30 minutes, single use | `a_reset_link_works_once`, `reset_links_expire_and_die_when_the_password_changes` |
| V2.5.7 | Reset doesn't bypass 2FA | Reset ends in `auth::sign_in` (the code step applies) | `a_password_alone_doesnt_sign_in_when_two_factor_is_on` |
| V3.2.1 | New session after authentication | Yes | `sign_in_rotates_the_session_and_kills_the_old_token` |

## Invariants

| Invariant | Test |
|-----------|------|
| Sign-in honors return-to (local paths only) | `password_sign_in_starts_a_session_and_honors_return_to` |
| A passwordless (magic-link) account can add a password | `a_passwordless_account_can_set_a_password` |
| A first sign-in sends no "new device" email; a new browser does | `a_new_device_gets_a_sign_in_notice_and_a_known_one_doesnt` |
| Sign-up, reset and change-password pages follow the theme switch | `password_pages_follow_the_theme_switch` |

## Known limits (accepted, documented)

- **The breach check is a small bundled list**, not a full breach corpus: no network call, no third party. README "Adapting it" shows where a k-anonymity check plugs in.
- **Verification links opened in the sign-up browser verify with one click.** A victim who clicks an unsolicited verification link *in the same browser the attacker used* would have to be on the attacker's machine; elsewhere the password is required.
- **Sign-up timing.** A new address costs a few more database writes than an existing one (about a millisecond, next to the ~50 ms argon2 hash both paths compute). Sign-in and reset requests do identical work on both paths.
- **Pause notices** go out once per pause episode (the attempt that starts it), not per refused attempt.

## Commands

```sh
TEST_DATABASE_URL=postgres://… tools/ci.sh auth-password
tools/security-check.sh
```
