# auth-totp

Two-factor authentication with an authenticator app (TOTP, RFC 6238: HMAC-SHA-1, 30-second steps, 6 digits, one step of drift either way), on top of any first factor: [auth-password](../auth-password/), [auth-magic-link](../auth-magic-link/), or both. Built on [auth-core](../auth-core/).

- **Enrolment with a QR code generated on the server** (SVG, as a `data:` image; the secret never goes to a QR service) plus the setup key in groups of four with a copy button. **Confirm before enable**: two-factor turns on only after a code from the app checks out, so a bad scan can't lock anyone out. Reloading the page keeps the same secret.
- **A pending sign-in step** between the first factor and the code: no session exists until the code is right. The code field is `autocomplete="one-time-code"`, numeric, accepts pasted "123 456", and **submits itself at six digits** (works without JavaScript too).
- **Replay protection**: the last accepted time step is stored and claimed with a conditional UPDATE, so the same code can't be used twice, even by two racing requests.
- **10 recovery codes** (`XXXXX-XXXXX`, unambiguous letters), stored as keyed digests, single use, shown once with **download, print and copy**, a **"used X of 10"** counter on the account page, regenerable.
- **"Don't ask again on this device for 30 days"**: a random cookie whose digest is stored, listed under **Remembered devices** on the account page, revocable one by one, and forgotten on password change or reset, two-factor off, and "this wasn't me".
- **Step-up**: with two-factor on, re-authentication ("Confirm it's you" before changing the email or password, turning two-factor off, new recovery codes) takes an authenticator or recovery code; a password or an emailed code is refused.
- **Limits**: 5 wrong codes per pending sign-in (then start over), progressive delays per account across sign-ins, and a per-network cap.
- **Secrets encrypted at rest** with AES-256-GCM under `APP_TOTP_ENCRYPTION_KEY`, a random nonce per value and the owner's id as associated data (a row copied onto another account doesn't decrypt); `APP_TOTP_ENCRYPTION_KEY_PREVIOUS` supports key rotation.
- Emails on two-factor on/off, new recovery codes and recovery code use, each with "this wasn't me".
- Tested against the **RFC 6238 Appendix B vectors** (SHA-1, SHA-256, SHA-512).

New in the samples (the originals had no second factor), built to the same standard. Compared with the usual add-ons (devise-two-factor, rotp-based Rails code, `nimble_totp` in Phoenix): replay protection, encrypted secrets, confirm-before-enable, trusted devices and step-up are all built in and tested rather than left to the app.

Guides: [`.offrails/guides/auth.md`](https://rustoffrails.org/docs/guides/auth), [`.offrails/guides/security.md`](https://rustoffrails.org/docs/guides/security). Depends on: [auth-core](../auth-core/) and at least one sign-in method.

## Screens

<a href="../../docs/screenshots/auth-totp/enrol-light-desktop.png"><picture><source media="(prefers-color-scheme: dark)" srcset="../../docs/screenshots/auth-totp/enrol-dark-desktop.png"><img src="../../docs/screenshots/auth-totp/enrol-light-desktop.png" alt="Set up two-factor: QR code and setup key (a fake demo key)" width="860"></picture></a>

<table>
<tr><td valign="top"><a href="../../docs/screenshots/auth-totp/recovery-codes-light-desktop.png"><picture><source media="(prefers-color-scheme: dark)" srcset="../../docs/screenshots/auth-totp/recovery-codes-dark-desktop.png"><img src="../../docs/screenshots/auth-totp/recovery-codes-light-desktop.png" alt="Ten recovery codes with download, print and copy" width="420"></picture></a><br><sub>Recovery codes</sub></td><td valign="top"><a href="../../docs/screenshots/auth-totp/challenge-light-desktop.png"><picture><source media="(prefers-color-scheme: dark)" srcset="../../docs/screenshots/auth-totp/challenge-dark-desktop.png"><img src="../../docs/screenshots/auth-totp/challenge-light-desktop.png" alt="The authenticator code step at sign-in" width="420"></picture></a><br><sub>The code step</sub></td></tr>
</table>

<sub>Light or dark follows your GitHub theme; click an image for full size.</sub>

The QR code above holds the fixed, obviously fake demo key `AAAA AAAA …` that `app task auth_demo` seeds in development; real enrolments always get a random secret.

| Screen | Light | Dark |
|--------|-------|------|
| Set up (QR + key) | [desktop](../../docs/screenshots/auth-totp/enrol-light-desktop.png) · [mobile](../../docs/screenshots/auth-totp/enrol-light-mobile.png) | [desktop](../../docs/screenshots/auth-totp/enrol-dark-desktop.png) · [mobile](../../docs/screenshots/auth-totp/enrol-dark-mobile.png) |
| Recovery codes | [desktop](../../docs/screenshots/auth-totp/recovery-codes-light-desktop.png) · [mobile](../../docs/screenshots/auth-totp/recovery-codes-light-mobile.png) | [desktop](../../docs/screenshots/auth-totp/recovery-codes-dark-desktop.png) · [mobile](../../docs/screenshots/auth-totp/recovery-codes-dark-mobile.png) |
| The code step | [desktop](../../docs/screenshots/auth-totp/challenge-light-desktop.png) · [mobile](../../docs/screenshots/auth-totp/challenge-light-mobile.png) | [desktop](../../docs/screenshots/auth-totp/challenge-dark-desktop.png) · [mobile](../../docs/screenshots/auth-totp/challenge-dark-mobile.png) |
| Lost phone (recovery code) | [desktop](../../docs/screenshots/auth-totp/challenge-recovery-light-desktop.png) · [mobile](../../docs/screenshots/auth-totp/challenge-recovery-light-mobile.png) | [desktop](../../docs/screenshots/auth-totp/challenge-recovery-dark-desktop.png) · [mobile](../../docs/screenshots/auth-totp/challenge-recovery-dark-mobile.png) |
| Account page, two-factor on | [desktop](../../docs/screenshots/auth-totp/account-light-desktop.png) · [mobile](../../docs/screenshots/auth-totp/account-light-mobile.png) | [desktop](../../docs/screenshots/auth-totp/account-dark-desktop.png) · [mobile](../../docs/screenshots/auth-totp/account-dark-mobile.png) |
| Confirm with a code | [desktop](../../docs/screenshots/auth-totp/confirm-with-code-light-desktop.png) · [mobile](../../docs/screenshots/auth-totp/confirm-with-code-light-mobile.png) | [desktop](../../docs/screenshots/auth-totp/confirm-with-code-dark-desktop.png) · [mobile](../../docs/screenshots/auth-totp/confirm-with-code-dark-mobile.png) |

## Install

By hand, in the app root, after [auth-core](../auth-core/) and a sign-in method:

1. `cargo add totp-rs --features otpauth`, `cargo add qrcode --no-default-features --features svg`, `cargo add aes-gcm` (never with versions; this sample was verified with totp-rs 6, qrcode 0.14, aes-gcm 0.11).
2. Copy `files/` into the app, keeping the paths. Replace `offrails_host` with your crate name in the test file.
3. Wiring, each at its `samples: <anchor>` comment (exact text in `sample.toml`):
   - `src/kernel/mod.rs`: `secret_box`; `src/models/mod.rs`: `pending_login`, `recovery_code`, `totp_credential`, `trusted_device`; `src/controllers/mod.rs` and `src/views/mod.rs`: `two_factor`.
   - `src/config.rs`: a `totp: TotpConfig` field (`APP_TOTP_ENCRYPTION_KEY`, `APP_TOTP_ENCRYPTION_KEY_PREVIOUS`) with its section, parse, test and `Debug` lines.
   - `src/routes.rs`: the routes below, their `paths::` helpers and `ROUTES` rows.
   - auth-core's `src/controllers/auth.rs`: the `intercept` call at `before sign in`, the check at `step-up second factor`, the trusted-device/pending-sign-in cleanup at `revoke everything` and `revoke other access`, and the removal of everything two-factor at `claim unverified account`.
   - auth-core's account page (`account fields`, `account load`, `account init`, `account sections`) and "Confirm it's you" (`confirm second factor`); `src/tasks/auth_cleanup.rs` and `src/tasks/auth_demo.rs`.
   - `migration/src/lib.rs`: the two-factor migration; `.env.example`, `compose.yaml`: the key.
   - `tests/app/main.rs`: `mod auth_totp_test;`.
4. `cargo fmt --all && cargo run -- db migrate && cargo run -q -- routes`, then the Definition of Done.

`tools/install.sh auth-totp <app>` does it all (install a sign-in method too).

## After installing

- Set `APP_TOTP_ENCRYPTION_KEY` (`openssl rand -base64 32`) in the deploy configuration; production refuses to boot without it. **Losing it locks out every two-factor account** (they'd need recovery codes): back it up like a database password.
- Log the install in `docs/decisions.md`; mention two-factor in `product/` so the human knows it exists.
- Try it in development: `cargo run -- task auth_demo`, sign in as `demo-2fa@example.com`, and type a code for the fake key `AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA` (any authenticator app, or `oathtool --totp -b AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA`).

## How the samples combine

| You install | The code step comes after |
|-------------|---------------------------|
| auth-password + auth-totp | the password, a password reset, and email verification |
| auth-magic-link + auth-totp | the emailed link or code |
| all three | any of them |

Every first factor ends in auth-core's `auth::sign_in`; this sample's `intercept` runs there (the `before sign in` anchor), so a new sign-in method (OAuth, passkeys) gets the code step for free.

## Adapting it

- **Require two-factor for some people** (admins): in your admin extractor, refuse until `totp_credential::enabled_for(db, user.id)` is true, and send them to `paths::new_account_two_factor()`.
- **Trusted-device lifetime**: `TTL` in `src/models/trusted_device.rs` (`// APP:`). To disable the feature, remove the checkbox from `templates/two_factor/challenge.html` (`trust_device` is then never sent).
- **More or fewer tries**: `MAX_ATTEMPTS` and `TTL` in `src/models/pending_login.rs` (`// APP:`), auth-core's throttle (`// APP:`).
- **Recovery code count**: `COUNT` in `src/models/recovery_code.rs` (`// APP:`).
- **The name in authenticator apps**: the issuer in `totp_credential::totp` (`// APP:`).
- **Rotate the encryption key**: move the current key to `APP_TOTP_ENCRYPTION_KEY_PREVIOUS`, set a new `APP_TOTP_ENCRYPTION_KEY`, deploy; secrets still decrypt, and `totp_credential::reseal` moves one to the new key (run it over all rows in a task), then drop the previous key.

## Do not change

- RFC 6238 parameters (SHA-1, 30 s, 6 digits, ±1 step): apps disagree on anything else (`rfc6238_test_vectors`, `codes_match_within_one_step_and_never_an_old_step`).
- The step claim (`a_totp_code_works_only_once`, `two_racing_requests_with_one_code_sign_in_once`).
- No session before the code (`a_password_alone_doesnt_sign_in_when_two_factor_is_on`).
- Confirm before enable (`enrolment_needs_a_valid_code_before_two_factor_turns_on`).
- Step-up needs a code when two-factor is on (`two_factor_users_must_step_up_with_a_code`, `turning_two_factor_off_needs_a_fresh_code`).
- The owner id as associated data in `secret_box` (`totp_secrets_are_encrypted_at_rest_and_bound_to_their_owner`).
- Trusted devices are scoped to their account and forgotten on credential changes (`a_trusted_device_cookie_only_works_for_its_own_account`, `credential_changes_and_not_me_forget_trusted_devices`).

## How it works

| Route | Handler | Access |
|-------|---------|--------|
| `GET /two-factor`, `POST /two-factor` | the code step | public (pending sign-in cookie) |
| `POST /two-factor/recovery` | a recovery code instead | public (pending sign-in cookie) |
| `GET /account/two-factor/new` | QR code and setup key | user, recent re-auth |
| `POST /account/two-factor` | confirm and turn on; shows the recovery codes once | user, recent re-auth |
| `POST /account/two-factor/recovery-codes` | a new set | user, recent re-auth |
| `POST /account/two-factor/delete` | turn off | user, recent re-auth |
| `POST /account/trusted-devices/{id}/delete` | forget a device | user |
| `POST /account/confirm/two-factor` | re-auth with a code | user |

Tables: `totp_credentials` (user id, the encrypted secret, `enabled_at`, `last_used_step`), `recovery_codes` (keyed digests, `used_at`), `trusted_devices` and `pending_logins` (token digests, expiry; pending sign-ins count attempts).

## Files

| File | What |
|------|------|
| `src/kernel/secret_box.rs` | AES-256-GCM seal/open with key rotation |
| `src/models/totp_credential.rs` | RFC 6238 via totp-rs, matching with replay protection, enrolment, the RFC vectors |
| `src/models/recovery_code.rs` | generate, redeem (atomic), count |
| `src/models/trusted_device.rs` | trust, check (scoped), list, revoke |
| `src/models/pending_login.rs` | the half-signed-in step |
| `src/controllers/two_factor.rs` | `intercept`, the code step, enrolment, recovery codes, off, devices, re-auth |
| `src/views/two_factor.rs` | page structs, the account card, the QR data URI |
| `templates/two_factor/`, `templates/account/_two_factor_section.html` | the screens |
| `migration/src/m20261016_004000_create_two_factor.rs` | the four tables |
| `tests/app/auth_totp_test.rs` | the tests named in CHECKS.md |
