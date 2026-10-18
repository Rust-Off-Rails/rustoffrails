# mailer

Action Mailer for a Rust off Rails app: an `Email` rendered from two Askama templates (HTML and text), delivered by the adapter the environment selects. **SMTP** in production (any provider: Postmark, Resend, SES, Mailgun), **the dev mailbox table** in development (read it with the [dev-mailbox](../dev-mailbox/) sample), a **log line** when production has no SMTP yet, **memory** in tests. Mailer previews, a `send_test_email` task to prove SMTP works, and header-injection checks before anything is sent.

Extracted from Rust off Rails apps and improved: links follow the address actually served in development (a stale `APP_URL` in `.env` sent sign-in links to another port), Message-IDs never carry this machine's hostname, line breaks in any header value are refused, and `SMTP_URL` is parsed with lettre's own URL parser (credentials percent-decoded, `smtp://` plain for a local catcher, `?tls=required` for STARTTLS).

Guide: [`.offrails/guides/mailers.md`](https://rustoffrails.org/docs/guides/mailers). Depends on: nothing.

## Screens

The test email, rendered in the dev mailbox's preview (with the dev-mailbox sample):

<a href="../../docs/screenshots/mailer/test-email-preview-light-desktop.png"><picture><source media="(prefers-color-scheme: dark)" srcset="../../docs/screenshots/mailer/test-email-preview-dark-desktop.png"><img src="../../docs/screenshots/mailer/test-email-preview-light-desktop.png" alt="The test email in the dev mailbox's HTML preview" width="860"></picture></a>

<table>
<tr><td valign="top"><a href="../../docs/screenshots/mailer/test-email-text-light-desktop.png"><picture><source media="(prefers-color-scheme: dark)" srcset="../../docs/screenshots/mailer/test-email-text-dark-desktop.png"><img src="../../docs/screenshots/mailer/test-email-text-light-desktop.png" alt="The test email's text part" width="420"></picture></a><br><sub>The text part</sub></td></tr>
</table>

<sub>Light or dark follows your GitHub theme; click an image for full size.</sub>

More: [dark](../../docs/screenshots/mailer/test-email-preview-dark-desktop.png) · [mobile](../../docs/screenshots/mailer/test-email-preview-light-mobile.png) · [text part](../../docs/screenshots/mailer/test-email-text-light-desktop.png)

## Install

By hand, in the app root:

1. `cargo add lettre --no-default-features --features builder,smtp-transport,pool,hostname,tokio1-rustls-tls` (never with a version; read the resolved one in `Cargo.lock`).
2. Copy `files/` into the app, keeping the paths. In the test file, replace `offrails_host` with your crate name.
3. Wiring (each at its `samples: <anchor>` comment when the app has one; otherwise at the spot described):
   - `src/kernel/mod.rs`: `pub mod mailbox;` and `pub mod mailer;`.
   - `src/lib.rs`: `pub mod mailers;`.
   - `src/config.rs`: a `pub mail: MailConfig` field on `Config`; `d.field("mail", &self.mail)` in its `Debug`; `let mail = MailConfig::from_env(env)?;` in `from_env`, `mail,` in its `Self { … }`, `mail: MailConfig::test(),` in `Config::test()`; and the `MailConfig` struct with its `impl` and redacting `Debug` at the end of the file (the text is in `sample.toml`).
   - `src/state.rs`: a `pub mailer: crate::kernel::mailer::Mailer` field, `let mailer = select_mailer(&config, &db)?;` before the struct is built, `mailer,` in it, and the `select_mailer` function.
   - `migration/src/lib.rs`: `mod m20261016_000100_create_mailbox_messages;` and `Box::new(m20261016_000100_create_mailbox_messages::Migration),` in `migrations()`.
   - `src/tasks/mod.rs`: `pub mod send_test_email;` and the `"send_test_email" => send_test_email::run(state, args).await,` arm.
   - `src/controllers/dev.rs`: the `SMTP_URL` / `MAILER` / `MAIL_FROM` facts on `/dev/system`.
   - `.env.example`: `SMTP_URL`, `MAILER`, `MAIL_FROM` (commented); `compose.yaml`: `SMTP_URL: ${SMTP_URL:-}`.
   - `tests/app/main.rs`: `mod mailer_test;`; `tests/app/support.rs`: the `sent_emails()` and `last_email_to(address)` helpers on `TestApp`.
4. `cargo fmt --all && cargo run -- db migrate`, then the Definition of Done (`.offrails/workflow.md`).

`tools/install.sh mailer <app>` does steps 1 to 3 mechanically (it needs the anchors; it refuses and names any it can't find).

## After installing

- Set `SMTP_URL` and `MAIL_FROM` in the deploy configuration; until then production logs mail instead of sending it. Prove it from the server: `app task send_test_email you@example.com`.
- Update the `EMAIL:` line in `AGENTS.md` (`dev: local mailbox (/dev/mailbox); prod: smtp via <provider>`).
- Request handlers send through a job, not `state.mailer.deliver(..)` directly: install the jobs sample for `mailers::deliver_later`. Until then, deliver from tasks and jobs only.
- Install [dev-mailbox](../dev-mailbox/) to read development mail.

## Adapting it

- **A new mailer**: copy `src/mailers/test_mailer.rs` to `src/mailers/<name>_mailer.rs` and its two templates to `templates/mailers/<name>_mailer/`; one function per action returning `Result<Email, AppError>`; build every link with `config.app_url` + a `paths::` helper; add a `Preview` per action in `previews.rs` (marked `// APP:`). A sample that ships a mailer (auth-core) wires its `pub mod` at `// samples: mailer mods` in `src/mailers/mod.rs` and its previews at `// samples: previews` in `PREVIEWS`; keep both anchors.
- **Cc, Bcc, Reply-To, attachments**: `Email::new(..).cc(..).bcc(..).reply_to(..).attach(Attachment::new(name, type, &bytes))`.
- **Your brand**: `templates/mailers/layout.html` is the shell every HTML email extends. Inline styles only (clients ignore `<link>`); keep the dark-mode `<style>` block's class names in step with it.
- **Try real delivery from development**: `MAILER=smtp SMTP_URL=smtp://localhost:1025` with a local catcher (Mailpit), or a provider's sandbox.
- **Tune code detection** in the dev mailbox: the `WORDS` list in `kernel/mailbox.rs` (`// APP:`).

## Do not change

- `mailer::validate` before every send, in every adapter (`header_injection_is_refused_before_anything_is_sent`).
- Development delivers to the dev mailbox unless `MAILER` says otherwise, even with `SMTP_URL` set (`development_always_uses_the_dev_mailbox_even_with_smtp_url_set`).
- `MailConfig`'s `Debug` redacts `SMTP_URL` (`the_smtp_url_never_shows_in_config_debug`).
- Absolute links from `config.app_url`, which follows the served address in development (`development_links_follow_the_port_being_served`).
- `#[serde(default)]` on every `Email` field added later: queued jobs must still decode.

## How it works

`state::select_mailer` picks the adapter once at boot:

| `APP_ENV` | `MAILER` | Adapter |
|-----------|----------|---------|
| test | any | `Memory` (`app.sent_emails()`) |
| any | `smtp` | `Smtp` (needs `SMTP_URL`) |
| any | `local` / `log` | that one |
| development | unset | `Local`, always, even with `SMTP_URL` set |
| production | unset | `Smtp` with `SMTP_URL`, otherwise `Log` |

`Mailer::Local` builds exactly the message SMTP would send (`mailer::build`: multipart/alternative, wrapped in multipart/mixed when there are attachments) and stores it with its decoded parts in `mailbox_messages`, so mail from requests, jobs and tasks lands in one table. `kernel/mailbox.rs` owns that table: search, filters, counts, trim, and the link and code extraction agents use.

## Files

| File | What |
|------|------|
| `src/kernel/mailer.rs` | `Email`, `Attachment`, `Mailer` (Smtp, Local, Log, Memory), `validate`, `build`, `Preview` |
| `src/kernel/mailbox.rs` | the `mailbox_messages` entity and queries; `links_in`, `codes_in`, `summary_json` |
| `src/mailers/mod.rs` | the mailers module |
| `src/mailers/test_mailer.rs` | `test_mailer::test_email`: the pattern for every mailer |
| `src/mailers/previews.rs` | `PREVIEWS` |
| `src/tasks/send_test_email.rs` | `app task send_test_email <address>` |
| `templates/mailers/layout.html` | the HTML shell every email extends |
| `templates/mailers/test_mailer/test_email.{html,txt}` | the test email |
| `migration/src/m20261016_000100_create_mailbox_messages.rs` | the table |
| `tests/app/mailer_test.rs` | the tests named in `CHECKS.md` |
