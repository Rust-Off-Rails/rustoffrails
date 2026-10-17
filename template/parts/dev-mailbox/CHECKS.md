# dev-mailbox: checks

What "working and safe" means for the dev mailbox, and the test that proves each item. `tools/security-check.sh` fails when a test named here doesn't exist; `tools/ci.sh dev-mailbox` runs them all on SQLite and Postgres.

The mailbox renders mail that the app (and, through it, anyone who can type into a form) wrote. Treat every message as hostile HTML.

## Threat model

| STRIDE | Threat | Control | Test |
|--------|--------|---------|------|
| Spoofing | A cross-site page makes the browser delete or clear mail, or send test mail (CSRF) | Every POST goes through tower-http's `CsrfLayer` (Fetch Metadata, then `Origin`) | `cross_site_posts_to_every_unsafe_route_are_403`, `the_origin_header_decides_without_fetch_metadata` |
| Tampering | Script in an email runs in the mailbox page and changes it, or acts as the developer on the app | The HTML part is never inlined: it is served alone from `/{id}/html` into an `<iframe sandbox>` without `allow-scripts` or `allow-same-origin`, and that response carries its own `Content-Security-Policy: sandbox …; default-src 'none'` (no script source at all) and `frame-ancestors 'self'` | `the_html_body_is_served_alone_in_a_sandbox` |
| Tampering | Markup in a subject, address, text part, header or filename is rendered as HTML | Askama escapes every value; Raw, Text and Headers are escaped text | `markup_in_mail_is_escaped_on_every_mailbox_page` |
| Tampering | An HTML or SVG attachment, opened from the mailbox, runs on the app's origin | Attachments are always `Content-Disposition: attachment` with `Content-Security-Policy: sandbox`; thumbnails only for raster types, never SVG | `attachments_are_downloads_never_documents_on_this_origin` |
| Tampering | A filename breaks out of the `Content-Disposition` header | `safe_filename` keeps letters, digits, `.`, `-`, `_` and spaces | `filenames_are_safe_in_a_quoted_header`, `attachments_are_downloads_never_documents_on_this_origin` |
| Repudiation | n/a: development tool on the developer's machine; nothing to attribute | — | — |
| Information disclosure | The mailbox (sign-in links, codes, reset tokens) is reachable in production | The router is merged only when `APP_ENV=development`; every route is `Access::Dev`, and production and test answer 404 from the router fallback | `mailbox_routes_do_not_exist_in_production_or_test`, `dev_routes_are_404_outside_development` |
| Information disclosure | Production mail lands in the table anyway | Production never selects the local mailer | `mail_in_production_never_reaches_the_mailbox_table` |
| Information disclosure | Mail stays in a shared cache or comes back from the back/forward cache after it was deleted | Every mailbox response (pages, fragments, JSON, downloads, redirects, the event stream) is `Cache-Control: no-store`; `admin.js` reloads pages restored from the bfcache | `every_mailbox_response_is_no_store`, `the_inbox_is_live_over_server_sent_events` |
| Information disclosure | Opening a link from a mail leaks the mailbox URL as the Referer, or the opened page scripts the mailbox tab | Links carry `rel="noopener noreferrer"`; the HTML body is served with `Referrer-Policy: no-referrer` | `codes_and_links_are_extracted_with_copy_buttons`, `the_html_body_is_served_alone_in_a_sandbox` |
| Denial of service | The test button or a stale `MAILER=smtp` sends real mail from development | The test email is stored through `Mailer::local`, never over SMTP | `the_test_email_button_never_sends_over_smtp` |
| Elevation of privilege | n/a: no accounts; the mailbox exists only on a development machine, where the developer already owns everything | — | — |

## Invariants

| Invariant | Test |
|-----------|------|
| Every mailbox page links its own stylesheet and script, never `app.css` (the bug this sample fixes) | `every_mailbox_page_links_its_own_stylesheet_and_script` |
| Every class a mailbox template uses has a rule in `admin.css` or `mailbox.css` | `every_mailbox_class_has_a_rule` |
| Opening a message marks it read; the inbox opens the newest | `opening_a_message_marks_it_read_and_shows_its_parts` |
| A deleted message's URL redirects to the inbox with a flash, never a bare 404 | `delete_clear_and_gone_messages_redirect_with_a_flash` |
| The live inbox hears about new mail from any process, and a reconnect resumes from the last event id | `the_inbox_is_live_over_server_sent_events`, `a_reconnect_resumes_from_the_last_event_id`, `the_list_fragment_reports_the_counts_for_the_live_inbox` |
| The preview renders in the chosen scheme, whatever the developer's OS theme | `previews_render_in_the_chosen_scheme_whatever_the_os_says` |
| The mailbox follows the kernel header's theme switch (`data-theme`, `light-dark()` tokens); the email's Dark view stays the email's, whatever the tool's theme | `the_mailbox_follows_the_theme_switch_and_the_dark_preview_stays_the_emails`, `every_kernel_page_renders_data_theme` |
| The `.eml` download is the exact stored message | `the_eml_download_is_the_exact_message` |
| Previews render in the same viewer and are never stored | `previews_render_in_the_same_viewer_and_are_never_stored` |
| Agents get links and codes as JSON, filterable by recipient, subject and cursor | `the_json_endpoints_filter_and_carry_links_and_codes` |
| The checks flag relative URLs, missing text parts, images without alt, Gmail clipping, scripts | `checks_flag_what_breaks_in_real_inboxes` |
| The checks flag mail wider than 320 px (fixed widths no `max-width` reins in), and the seeded weekly digest's 496 px banner is flagged | `mail_that_overflows_a_320_px_screen_is_flagged`, `the_seeded_weekly_digest_overflows_at_320_px` |
| The base target goes inside `<head>`, never before the doctype (quirks mode) | `the_base_goes_inside_head_and_never_before_the_doctype` |

## In a browser (not part of the test suite)

- axe-core (WCAG 2.2 AA), light and dark (the OS scheme and the switch forcing each), on the inbox, a message in every tab and view, previews, search and the dev pages: 0 violations.
- Keyboard: every action is a link, button or form; `j`/`k`/`/`/`c`/`o`/`e`/`d`/`1`–`4`/`?` are shortcuts on top, never the only way.
- No console errors and no CSP violations on any mailbox page.

```sh
tools/demo.sh dev-mailbox
PATHS="/dev/mailbox /dev/mailbox/1 /dev/mailbox/1?tab=raw /dev/mailbox/previews" BASE=http://localhost:3900 \
  bash host/.offrails/tools/browser-smoke/run.sh a11y
```

## Commands

```sh
TEST_DATABASE_URL=postgres://… tools/ci.sh dev-mailbox   # with its dependency (mailer), on both databases
tools/security-check.sh
```
