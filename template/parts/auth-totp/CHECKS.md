# auth-totp: checks

What "working and safe" means for two-factor authentication, and the test that attacks each control. Written for an adversarial reviewer. The shared layer is in [auth-core's CHECKS.md](../auth-core/CHECKS.md). `tools/security-check.sh` fails when a test named here doesn't exist; `tools/ci.sh auth-totp` runs them on SQLite and Postgres (with `tools/ci.sh all`, together with both sign-in methods).

## Threat model (STRIDE)

| STRIDE | Threat | Control | Test |
|--------|--------|---------|------|
| Spoofing | A stolen password (or mailbox) signs in | The first factor only starts a pending sign-in; no session until a code is right | `a_password_alone_doesnt_sign_in_when_two_factor_is_on`, `users_without_two_factor_sign_in_directly` |
| Spoofing | A phished or shoulder-surfed code is replayed | Codes are accepted only for a time step after the last used one, claimed by a conditional UPDATE | `a_totp_code_works_only_once`, `two_racing_requests_with_one_code_sign_in_once`, `codes_match_within_one_step_and_never_an_old_step` |
| Spoofing | Brute-forcing the 6-digit code | 5 tries per pending sign-in (counted before checking), then the first factor again; progressive delays per account across sign-ins; a per-network cap | `a_pending_sign_in_allows_five_wrong_codes_then_starts_over`, `wrong_codes_are_throttled_per_account_across_sign_ins` |
| Spoofing | A recovery code is reused, or used twice at the same moment | Atomic `UPDATE … WHERE used_at IS NULL` | `a_recovery_code_works_only_once`, `a_recovery_code_used_twice_at_the_same_time_works_once` |
| Spoofing | An attacker set up two-factor on an account before its owner verified the address | Claiming an unverified account (auth-magic-link) removes its authenticator, recovery codes, trusted devices and pending sign-ins (wired at `claim unverified account`) | `claiming_an_unverified_account_drops_its_two_factor` |
| Spoofing | A trusted-device cookie from one account skips the code on another | Checked against the signing-in user's own devices | `a_trusted_device_cookie_only_works_for_its_own_account` |
| Spoofing | A stale pending sign-in is finished later | Pending sign-ins expire after 10 minutes | `a_pending_sign_in_expires` |
| Tampering | A mis-scanned secret locks the person out, or an attacker enables 2FA with their own secret | Confirm before enable (a valid code from the pending secret), and enrolment needs a recent re-auth | `enrolment_needs_a_valid_code_before_two_factor_turns_on`, `enrolment_needs_a_recent_reauth` |
| Tampering | A database writer copies a known secret onto another account | The owner id is AES-GCM associated data: a copied row doesn't decrypt | `totp_secrets_are_encrypted_at_rest_and_bound_to_their_owner`, `encrypted_secrets_round_trip_and_bind_to_their_owner` |
| Repudiation | 2FA changes and recovery-code use can't be traced | Events for enabled, disabled, failed, recovery code used, codes regenerated, device trusted; the owner is emailed | `turning_two_factor_off_needs_a_fresh_code`, `a_recovery_code_works_only_once`, `regenerating_recovery_codes_voids_the_old_ones` |
| Information disclosure | A database leak reveals TOTP secrets | AES-256-GCM under `APP_TOTP_ENCRYPTION_KEY` (not in the database), random nonce per value | `totp_secrets_are_encrypted_at_rest_and_bound_to_their_owner` |
| Information disclosure | A database leak reveals recovery codes | HMAC digests under `APP_SECRET_KEY` | `recovery_codes_are_stored_as_keyed_digests` |
| Information disclosure | The secret goes to a third party | The QR code is rendered on the server (SVG data URI) | `enrolment_needs_a_valid_code_before_two_factor_turns_on` |
| Information disclosure | Pages showing secrets or codes are cached | `no-store` on enrolment and recovery-code pages | `the_enrolment_page_is_not_cached`, `regenerating_recovery_codes_voids_the_old_ones` |
| Information disclosure | Secrets or codes in logs | Never logged; a test captures every log line during enrolment | `totp_secrets_and_codes_never_reach_the_logs` |
| Denial of service | Locking the owner out by failing codes | Per-account delays are short and capped; recovery codes always work through the same limits | `wrong_codes_are_throttled_per_account_across_sign_ins`, `a_recovery_code_works_only_once` |
| Elevation of privilege | A stolen session or a phished password turns 2FA off | Turning it off (and new codes, email/password changes) needs a recent re-auth, which with 2FA on needs a code; password and emailed-code re-auth are refused | `turning_two_factor_off_needs_a_fresh_code`, `two_factor_users_must_step_up_with_a_code` |
| Elevation of privilege | An attacker keeps a trusted device after the owner reacts | Trusted devices are forgotten on password change/reset, 2FA off and "this wasn't me"; listed and revocable; another user's device id is a 404 | `credential_changes_and_not_me_forget_trusted_devices`, `forgetting_another_users_trusted_device_is_404`, `a_trusted_device_skips_the_code_for_thirty_days` |
| Elevation of privilege | Other sessions survive enabling 2FA | Enabling rotates this session and ends every other one | `enrolment_needs_a_valid_code_before_two_factor_turns_on` |

## ASVS-style checklist (OWASP ASVS 4.0, V2.8 one-time verifiers)

| ASVS | Requirement | Status | Test |
|------|-------------|--------|------|
| V2.8.1 | Time-based OTPs have a defined lifetime | 30 s step, ±1 step | `codes_match_within_one_step_and_never_an_old_step` |
| V2.8.3 | Approved algorithms (HOTP/TOTP, RFC 6238) | Yes, verified against the RFC vectors | `rfc6238_test_vectors` |
| V2.8.4 | Each OTP used once | Step claim | `a_totp_code_works_only_once` |
| V2.8.5 | Reuse is logged and refused | Failure event + refused | `a_totp_code_works_only_once` |
| V2.8.7 | Secrets protected at rest | AES-256-GCM | `totp_secrets_are_encrypted_at_rest_and_bound_to_their_owner` |
| V2.5.6 / V2.5.7 | Recovery doesn't bypass the factor; codes single use | Recovery codes count as the second factor, single use | `a_recovery_code_works_only_once` |
| V2.2.1 | Anti-automation on the second factor | Per pending sign-in, per account, per network | `a_pending_sign_in_allows_five_wrong_codes_then_starts_over`, `wrong_codes_are_throttled_per_account_across_sign_ins` |
| V3.7.1 | Re-authentication before changing the factor | With a code | `turning_two_factor_off_needs_a_fresh_code` |
| V2.2.3 | Notify after changes | Emails with "this wasn't me" | `turning_two_factor_off_needs_a_fresh_code` |

## Invariants

| Invariant | Test |
|-----------|------|
| The manual key is base32 in groups of four | `the_manual_key_is_grouped_base32` |
| Recovery codes use unambiguous characters and normalize case, spaces and dashes | `codes_are_readable_and_normalize` |
| A recovery code also confirms identity (lost phone) | `a_recovery_code_also_confirms_identity` |
| The account page shows "Used X of 10" | `a_recovery_code_works_only_once` |
| The two-factor pages follow the theme switch | `two_factor_pages_follow_the_theme_switch` |

## Known limits (accepted, documented)

- **TOTP is phishable in real time** (a relay site can forward a fresh code within its 30-90 s window). Replay protection limits it to one use; passkeys (WebAuthn) are the next step for phishing resistance.
- **Losing `APP_TOTP_ENCRYPTION_KEY` disables every enrolled authenticator**; people then need recovery codes. Back the key up like a database credential.
- **Trusted devices skip only the code**, never the first factor.

## Commands

```sh
TEST_DATABASE_URL=postgres://… tools/ci.sh auth-totp
TEST_DATABASE_URL=postgres://… tools/ci.sh all       # with auth-password and auth-magic-link
tools/security-check.sh
```
