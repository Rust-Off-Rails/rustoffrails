# mailer: checks

What "working and safe" means for the mailer, and the test that proves each item. `tools/security-check.sh` fails when a test named here doesn't exist; `tools/ci.sh mailer` runs them all on SQLite and Postgres.

## Threat model

The mailer has no routes of its own. Its attack surface is what it puts into a message and where messages go.

| STRIDE | Threat | Control | Test |
|--------|--------|---------|------|
| Spoofing | Input changes who a message appears to be from, or adds recipients (`Bcc:`) through a line break in a header value | `mailer::validate` refuses control characters in the subject, every address, attachment names and types, and more than one address per field; every adapter calls it before sending or storing | `header_injection_is_refused_before_anything_is_sent`, `line_breaks_in_free_text_headers_are_refused`, `several_addresses_in_one_field_are_refused`, `the_memory_and_log_mailers_refuse_what_smtp_would_refuse` |
| Tampering | A non-ASCII subject is written raw and mangled or reinterpreted by a server | lettre encodes it (RFC 2047) | `a_unicode_subject_is_encoded_not_raw` |
| Repudiation | n/a: the mailer records nothing about who triggered a send; the app's own audit trail does | — | — |
| Information disclosure | `SMTP_URL` credentials leak through `/dev/introspect.json`, `/dev/system` or logs | `MailConfig`'s hand-written `Debug` redacts it; `/dev/system` shows "configured" or "not set" | `the_smtp_url_never_shows_in_config_debug` |
| Information disclosure | Bcc recipients are visible to the other recipients | Bcc goes to the envelope only, never into the message headers | `local_delivery_stores_the_message_that_smtp_would_send` |
| Information disclosure | Development mail reaches real people (a copied production `SMTP_URL` in `.env`) | Development always delivers to the dev mailbox unless `MAILER=smtp` says otherwise | `development_always_uses_the_dev_mailbox_even_with_smtp_url_set`, `selection_follows_the_environment_and_mailer` |
| Information disclosure | Production mail lands in the `mailbox_messages` table | Production selects SMTP or Log, never Local, unless `MAILER=local` | `selection_follows_the_environment_and_mailer` |
| Denial of service | A bad `MAIL_FROM` or `SMTP_URL` fails at the first send, at night | Both are parsed when the app boots, which fails with a message | `an_invalid_mail_from_fails_at_boot`, `selection_follows_the_environment_and_mailer` |
| Elevation of privilege | A sign-in link in an email points at another server (a stale `APP_URL` in `.env`), so the token goes there | Development links follow the address actually served; production links come from `APP_URL` (required) | `development_links_follow_the_port_being_served`, `production_links_come_from_app_url`, `a_stale_local_app_url_follows_the_port_being_served` |

## Invariants

| Invariant | Test |
|-----------|------|
| Every email has an HTML and a text part, with absolute links | `the_test_email_has_both_parts_and_absolute_links` |
| What `Mailer::Local` stores is byte for byte what SMTP would send | `local_delivery_stores_the_message_that_smtp_would_send` |
| Tests capture mail in memory (`app.sent_emails()`); nothing touches a network | `the_test_environment_captures_mail_in_memory` |
| Every preview renders, and previews are never sent or stored | `every_preview_renders_and_is_never_sent_or_stored` |
| `app task send_test_email <address>` goes through the selected mailer | `the_send_test_email_task_delivers_through_the_selected_mailer` |
| Mailbox search treats `%` and `_` literally and is case-insensitive on both databases | `mailbox_search_trim_and_latest`, `like_wildcards_are_literal` |
| Links and one-time codes are extracted from the decoded text, never the quoted-printable `.eml` | `links_come_from_text_then_html_hrefs_without_trailing_punctuation`, `codes_are_found_where_a_line_talks_about_a_code`, `years_prices_and_words_are_not_codes`, `dates_and_years_are_not_codes_even_next_to_the_word_code` |
| A code needs context: a code word a few words away, the line above, or a 6-digit line of its own; a year or a date is never one (a post titled "… code in 2026" once showed "CODE 2026") | `dates_and_years_are_not_codes_even_next_to_the_word_code`, `dates_and_years_in_a_message_are_not_shown_as_codes` |
| The migration rolls back and forward | `the_mailbox_migration_rolls_back_and_forward` |

## Commands

```sh
TEST_DATABASE_URL=postgres://… tools/ci.sh mailer   # install into host/, then the Definition of Done on both databases
tools/security-check.sh                             # every test named here exists
```
