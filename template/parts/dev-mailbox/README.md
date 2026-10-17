# dev-mailbox

Every email your app sends in development, in a real inbox at **`/dev/mailbox`**: live as it arrives, rendered safely, with the sign-in link or one-time code one keystroke away. Rails has letter_opener, Phoenix has Swoosh's mailbox; this is the one that also tells you why your email will break in Gmail.

- **Live inbox** over Server-Sent Events: mail from a request, a job or a task appears within a second, with a count in the tab title. Pause and resume.
- **Message view**: the HTML part in a sandboxed frame (no script, opaque origin), the text part, the raw RFC 822 source, every header.
- **Desktop, 600 px, 320 px and Dark** previews. Dark applies the mail's own dark-mode CSS, or the full-color inversion many mail apps do.
- **Codes and links** pulled out of the message with big copy buttons; links sent while the app ran on another port get an "Open here".
- **Checks** before real inboxes see it: relative URLs, a missing text part, images without alt text, Gmail's 102 KB clipping, a layout wider than a 320 px phone (a fixed-width banner, table or block that no `max-width` reins in), `<script>`, a long subject, `List-Unsubscribe`.
- **Attachments** listed with size and type, raster images as thumbnails, always downloaded (never rendered on your origin).
- **Search**, **delete**, **clear**, **`.eml` download**, unread state.
- **Mailer previews**: every mailer rendered from sample data in the same viewer, never sent.
- **Keyboard**: `j`/`k` next and previous, `/` search, `c` copy the code, `o` open the first link, `e` download, `d` delete, `1`–`4` tabs, `?` the list.
- **For agents and tests**: `/dev/mailbox/latest.json?to=…` and `/dev/mailbox.json?to=&subject=&since=` with links and codes; `app mailbox tail|list|show|clear|seed` in a terminal.
- **It styles itself**: `static/kernel/mailbox.css` and `mailbox.js` ship with it and every mailbox page links them. Light and dark, following the kernel header's System / Light / Dark switch (tokens with `light-dark()`, [theming](../FORMAT.md#theming)), WCAG 2.2 AA (axe-clean in both), usable at 320 px. The Dark *preview* is about the email and works whatever the tool's theme.
- **Development only**: in production the routes don't exist (404, proven by a test).

Extracted from Rust off Rails apps and rebuilt. Fixed on the way: an app moved the mailbox styles into `app.css`, which kernel pages never load, so the mailbox rendered unstyled; `<base target="_blank">` could land before the doctype (quirks mode); the inbox polled every 2 s per tab. Added: the live stream, codes, checks, previews in four widths and themes, keyboard, JSON filters and the CLI.

Guide: [`.offrails/guides/mailers.md#the-dev-mailbox`](https://rustoffrails.org/docs/mailers) · [`.offrails/guides/dev-tools.md`](https://rustoffrails.org/docs/dev-tools). Depends on: [mailer](../mailer/).

## Screenshots

![The inbox with a one-time code, light, desktop](../../docs/screenshots/dev-mailbox/inbox-light-desktop.png)

| Screen | Light | Dark |
|--------|-------|------|
| Inbox, one-time code | [desktop](../../docs/screenshots/dev-mailbox/inbox-light-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/inbox-light-mobile.png) | [desktop](../../docs/screenshots/dev-mailbox/inbox-dark-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/inbox-dark-mobile.png) |
| Attachments and links | [desktop](../../docs/screenshots/dev-mailbox/attachments-light-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/attachments-light-mobile.png) | [desktop](../../docs/screenshots/dev-mailbox/attachments-dark-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/attachments-dark-mobile.png) |
| Checks | [desktop](../../docs/screenshots/dev-mailbox/checks-light-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/checks-light-mobile.png) | [desktop](../../docs/screenshots/dev-mailbox/checks-dark-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/checks-dark-mobile.png) |
| Dark preview | [desktop](../../docs/screenshots/dev-mailbox/dark-preview-light-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/dark-preview-light-mobile.png) | [desktop](../../docs/screenshots/dev-mailbox/dark-preview-dark-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/dark-preview-dark-mobile.png) |
| 320 px preview | [desktop](../../docs/screenshots/dev-mailbox/width-320-light-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/width-320-light-mobile.png) | [desktop](../../docs/screenshots/dev-mailbox/width-320-dark-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/width-320-dark-mobile.png) |
| Raw source | [desktop](../../docs/screenshots/dev-mailbox/raw-light-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/raw-light-mobile.png) | [desktop](../../docs/screenshots/dev-mailbox/raw-dark-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/raw-dark-mobile.png) |
| All headers | [desktop](../../docs/screenshots/dev-mailbox/headers-light-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/headers-light-mobile.png) | [desktop](../../docs/screenshots/dev-mailbox/headers-dark-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/headers-dark-mobile.png) |
| Search | [desktop](../../docs/screenshots/dev-mailbox/search-light-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/search-light-mobile.png) | [desktop](../../docs/screenshots/dev-mailbox/search-dark-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/search-dark-mobile.png) |
| Previews | [desktop](../../docs/screenshots/dev-mailbox/previews-light-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/previews-light-mobile.png) | [desktop](../../docs/screenshots/dev-mailbox/previews-dark-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/previews-dark-mobile.png) |
| Keyboard shortcuts | [desktop](../../docs/screenshots/dev-mailbox/shortcuts-light-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/shortcuts-light-mobile.png) | [desktop](../../docs/screenshots/dev-mailbox/shortcuts-dark-desktop.png) · [mobile](../../docs/screenshots/dev-mailbox/shortcuts-dark-mobile.png) |

## Install

Through the samples MCP server: `get_sample dev-mailbox`. Or by hand, in the app root, after the [mailer](../mailer/) sample:

1. No crates to add.
2. Copy `files/` into the app, keeping the paths. In the test file and the `main.rs` wiring, replace `offrails_host` with your crate name.
3. Wiring (each at its `samples: <anchor>` comment when the app has one; otherwise at the spot described):
   - `src/kernel/mod.rs`: `pub mod mailbox_cli;` and `pub mod mailbox_web;`.
   - `src/kernel/ui.rs`: a `Tab { key: "mailbox", label: "Mailbox", href: "/dev/mailbox", dev_only: true }` in the kernel header's `TABS`.
   - `src/routes.rs`: inside `if config.is_development() { … }`, `app = app.merge(crate::kernel::mailbox_web::router(crate::mailers::previews::PREVIEWS));`; a `paths::dev_mailbox()` helper; the sixteen `ROUTES` rows, all `Access::Dev` (they're in `sample.toml`).
   - `src/controllers/dev.rs`: the Mailbox and Mailer previews tiles, and the "Send a test email" button.
   - `src/kernel/introspect.rs`: `out["mailbox"]` with the counts.
   - `src/main.rs`: the `Mailbox { command }` subcommand and its dispatch arm.
   - `templates/layouts/application.html`: a footer link `<a href="{{ paths::dev_mailbox() }}" hx-boost="false" class="link">Mailbox</a>` next to Dev tools.
   - `tests/app/main.rs`: `mod dev_mailbox_test;`; `tests/app/security_test.rs`: `"src/kernel/mailbox_web.rs"` in the route sources; `tests/app/kernel_web_test.rs`: the mailbox paths in the no-store list; `tests/app/theme_switch_test.rs`: `"/dev/mailbox"` and `"/dev/mailbox/previews"` in `KERNEL_PAGES`.
4. `cargo fmt --all && cargo run -- routes` (rewrites `docs/routes.md`), then the Definition of Done.

`tools/install.sh dev-mailbox <app>` installs mailer first if needed, then does steps 1 to 3.

## After installing

- Every link from an app page into the mailbox carries `hx-boost="false"` (a kernel page has its own `<head>`; boosted, it renders unstyled). `kernel_web_test.rs::app_links_into_kernel_pages_are_not_boosted` checks it.
- Try it: `cargo run -- mailbox seed --app-url http://localhost:3000`, open `/dev/mailbox`, press `?`.
- Agents: after a flow that sends mail, `curl -s $URL/dev/mailbox/latest.json?to=ada@example.com | jq '.links[0], .codes[0]'`, or `cargo run -q -- mailbox tail` in a second terminal.

## Adapting it

- **More messages per page, or a different poll**: `LIST_LIMIT` and `POLL` in `kernel/mailbox_web.rs` (`// APP:`).
- **Detect your product's codes**: `WORDS` in `kernel/mailbox.rs` (from the mailer sample).
- **Your own checks**: add a rule to `checks()` in `kernel/mailbox_web.rs` (`add("warn", …)`), and a case to `checks_flag_what_breaks_in_real_inboxes`.
- **A mailbox on a staging server**: don't route this one there. Run staging with `MAILER=smtp` against a catcher (Mailpit), which is built for shared use; this mailbox assumes the only person who can reach it owns the machine.
- **Brand tones**: the `--mbx-*` tokens at the top of `static/kernel/mailbox.css` (`/* APP: */`), each `light-dark(<light>, <dark>)`; the shared ones (`--accent`, `--panel`…) are in `admin.css`. Keep AA contrast in both and never add a `prefers-color-scheme` block (it ignores the theme switch).
- **Assert on mail in end-to-end tests**: `GET /dev/mailbox.json?to=…&since=<id>` from the browser test, or `app.sent_emails()` in Rust tests.

## Do not change

- The iframe and the body response stay sandboxed without `allow-scripts` and `allow-same-origin` (`the_html_body_is_served_alone_in_a_sandbox`).
- The router is merged only in development (`mailbox_routes_do_not_exist_in_production_or_test`).
- Attachments are always downloads with a sandbox CSP; no SVG thumbnails (`attachments_are_downloads_never_documents_on_this_origin`).
- `no_store()` on the router (`every_mailbox_response_is_no_store`).
- Mailbox styles live in `static/kernel/mailbox.css`, linked by the page's `Chrome`, never in `assets/app.css` (`every_mailbox_page_links_its_own_stylesheet_and_script`).

## How it works

The [mailer](../mailer/) sample's `Mailer::Local` stores each message in `mailbox_messages`. This sample reads that table:

| Route | Does |
|-------|------|
| `GET /dev/mailbox` | the inbox, newest message open; `?q=` searches subject, address and text |
| `GET /dev/mailbox/{id}` | message `id` open (marks it read); `?tab=html\|text\|raw\|headers`, `?view=desktop\|600\|320\|dark` |
| `GET /dev/mailbox/{id}/html` | the HTML part alone, for the sandboxed iframe; `?scheme=light\|dark` |
| `GET /dev/mailbox/{id}/message.eml` | the exact message, as a download |
| `GET /dev/mailbox/{id}/attachments/{n}` | attachment `n`, as a download |
| `POST /dev/mailbox/{id}/delete`, `POST /dev/mailbox/clear` | housekeeping, with a flash |
| `POST /dev/mailbox/test` | a sample message, stored locally (never SMTP) |
| `GET /dev/mailbox/events?since=` | Server-Sent Events: `mailbox` with `{newest, total, unread}` whenever they change |
| `GET /dev/mailbox/list?q=&selected=` | the list fragment the live inbox swaps in (counts in `X-Mailbox-Total`/`-Unread`) |
| `GET /dev/mailbox/previews`, `/previews/{mailer}/{action}`, `…/html` | mailer previews in the same viewer |
| `GET /dev/mailbox.json?to=&subject=&since=`, `/dev/mailbox/latest.json?to=` | for agents and tests |
| `GET /dev/mailers` | 301 to the previews (older apps linked here) |

The stream polls the table once a second per open tab (development only), so mail from another process, a worker or a task, shows up too. The page swaps in the list fragment and announces new mail to screen readers. Without JavaScript everything still works by plain links and forms.

## Files

| File | What |
|------|------|
| `src/kernel/mailbox_web.rs` | the router, views, checks, the event stream, `in_scheme` |
| `src/kernel/mailbox_cli.rs` | `app mailbox tail\|list\|show\|clear\|seed` |
| `templates/kernel/mailbox/index.html`, `_list.html`, `_message.html` | the two panes |
| `static/kernel/mailbox.css`, `mailbox.js` | its own styles and behavior |
| `tests/app/dev_mailbox_test.rs` | the tests named in `CHECKS.md` |
