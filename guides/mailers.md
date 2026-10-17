# Mailers (Action Mailer → lettre + Askama, with Swoosh's dev mailbox)

> **TL;DR**
> - `kernel/mailer.rs`: `Mailer::Local` in development (every email lands in `/dev/mailbox`), `Memory` in tests, SMTP in production (`Log` if none is set); selection lives in `state.rs`.
> - Send HTML **and** text with absolute URLs; deliver through a job (`deliver_later`); mail that carries a token gets its own job that mints the token.
> - lettre with `default-features = false` and the async Tokio transport.
> - Agents read mail from `/dev/mailbox/latest.json`, never from raw `.eml` (quoted-printable breaks long links).
>
> Read the rest when writing a mailer, previews, or changing the dev mailbox.

**Use the part.** The `mailer` and `dev-mailbox` [parts](samples.md#the-samples) are this guide built, tested on SQLite and Postgres and security-reviewed, with more on top: header-injection checks, a `send_test_email` task, and a mailbox with HTML/text/raw/headers, a 320 px phone-width lint, dark previews, one-key copy of codes and links, search and a CLI. When the product needs this, add the part rather than writing it from this guide: `cargo offrails add-part mailer dev-mailbox` (dependencies come first; [workflow](../workflow.md#when-the-product-grows-a-need-add-the-part)). Keep its `// SECURITY:` markers and the tests its CHECKS.md cites. This guide stays the spec: hand-write from it only when no part fits, or the human declined the part.

| Rails / Phoenix | Off Rails |
|-----------------|-----------|
| `UserMailer.welcome(user)` | `mailers::user_mailer::welcome(&state.config, &user)? -> Email` |
| `.deliver_later` | `mailers::deliver_later(&txn, email).await?` (enqueues `DeliverEmailJob`) |
| `.deliver_later` for time-limited mail without secrets | `mailers::deliver_later_until(&txn, email, expires_at).await?` (5 attempts, discarded once expired) |
| Password reset / magic link / invitation mail | A dedicated job with **only ids** in its args; the worker mints the token and delivers ([auth](auth.md)) |
| `.deliver_now` | `state.mailer.deliver(email).await?` (only inside jobs and tasks) |

This guide assumes the job queue, which `new` builds into every app ([jobs](jobs.md)). Without it (an app that removed it, or the `mailer` sample installed without `jobs`), there's no `deliver_later`: add the queue before the first email a request sends, and until then deliver only from tasks.
| `app/views/user_mailer/welcome.html.erb` + `.text.erb` | `templates/mailers/user_mailer/welcome.html` + `.txt` |
| `app/views/layouts/mailer.html.erb` | `templates/mailers/layout.html` |
| `default from:` | `MAIL_FROM` env var |
| `config.action_mailer.delivery_method` | `state::select_mailer` (per environment, `MAILER` overrides) |
| Swoosh `Plug.Swoosh.MailboxPreview` / letter_opener_web | `Mailer::Local` + **the dev mailbox at `/dev/mailbox`** |
| Mailer previews (`/rails/mailers`) | The mailbox's **Previews** tab, `/dev/mailbox/previews` |
| `ActionMailer::Base.deliveries` / `Swoosh.TestAssertions` | `app.sent_emails()` |

## Kernel spec (`src/kernel/mailer.rs`)

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Email {
    pub to: String, pub subject: String, pub html: String, pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]     pub cc: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]     pub bcc: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]  pub reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]     pub attachments: Vec<Attachment>,
}
impl Email { pub fn new(to, subject, html, text) -> Self }   // everything else via ..Default::default()

/// `data` is base64, so the email stays plain JSON in the job queue.
pub struct Attachment { pub filename: String, pub content_type: String, pub data: String }
impl Attachment { pub fn new(filename: &str, content_type: &str, bytes: &[u8]) -> Self; pub fn bytes(&self) -> Vec<u8> }

#[derive(Clone)]
pub enum Mailer {
    Smtp { transport: AsyncSmtpTransport<Tokio1Executor>, from: Mailbox },
    Local { db: DatabaseConnection, from: Mailbox },   // development: rows in mailbox_messages, shown at /dev/mailbox
    Log,                                               // production fallback when no SMTP is configured
    Memory(Arc<Mutex<Vec<Email>>>),                    // tests
}

impl Mailer {
    pub fn smtp(url: &str, from: Mailbox) -> anyhow::Result<Self>;   // AsyncSmtpTransport::<Tokio1Executor>::from_url(url)?.build()
    pub fn local(db: DatabaseConnection, from: Mailbox) -> Self;
    pub fn memory() -> Self;
    pub async fn deliver(&self, email: Email) -> anyhow::Result<()>;
    pub fn sent(&self) -> Vec<Email>;                                 // Memory only
}

/// The exact message SMTP sends: From/To/Cc/Bcc/Reply-To, multipart/alternative (text + HTML),
/// wrapped in multipart/mixed with one part per attachment.
pub fn build(from: &Mailbox, email: &Email) -> anyhow::Result<Message>;
```

The kernel only offers constructors; it never reads `Config`. **Selection lives in the app**, in `src/state.rs`:

```rust
/// Which mailer this environment uses.
pub fn select_mailer(c: &Config, db: &DatabaseConnection) -> anyhow::Result<Mailer>
```

| `APP_ENV` | `MAILER` | Mailer |
|-----------|----------|--------|
| test | – | `Memory` |
| any | `smtp` | `Smtp` (requires `SMTP_URL`). Use it to try real delivery from development |
| any | `local` / `log` | That one |
| **development** | unset | **`Local`, always, even when `SMTP_URL` is set** |
| production | unset | `Smtp` if `SMTP_URL` is set, else `Log` |

Any other `MAILER` value fails at boot. There is no "write .eml files to storage/" mode: in development, mail goes to the dev mailbox, full stop.

`SMTP_URL` examples: `smtps://user:pass@smtp.postmarkapp.com:465`, `smtp://localhost:1025` (optional Mailpit). Any provider works over SMTP (Postmark, Resend, SES, Mailgun). **Don't add provider SDKs.**

lettre must be added **without default features** (its defaults pull in native-tls and break the rustls build): `cargo add lettre --no-default-features --features builder,smtp-transport,pool,hostname,tokio1-rustls-tls`.

## Writing a mailer (`src/mailers/user_mailer.rs`)

```rust
#[derive(Template)] #[template(path = "mailers/user_mailer/welcome.html")]
struct WelcomeHtml<'a> { user: &'a User, login_url: String }

#[derive(Template)] #[template(path = "mailers/user_mailer/welcome.txt")]
struct WelcomeText<'a> { user: &'a User, login_url: String }

pub fn welcome(config: &Config, user: &User) -> Result<Email, AppError> {
    let login_url = format!("{}{}", config.app_url, paths::new_session());   // absolute URLs only
    Ok(Email::new(
        user.email.clone(),
        "Welcome to Bookshelf",
        WelcomeHtml { user, login_url: login_url.clone() }.render()?,
        WelcomeText { user, login_url }.render()?,
    ))
}
```

- Two template structs per email (HTML + text), sharing field names.
- **The mail layout is `templates/mailers/layout.html`** (the shared HTML shell; inline styles only, since email clients ignore `<link>`). Each HTML template `{% extends "mailers/layout.html" %}`. There's no `templates/layouts/mailer.html`.
- **Every link is absolute**, built from `config.app_url` + a `paths::` helper.
- **Every mailer action gets a preview** (below).

## Delivering

```rust
// in a model fn, inside the transaction that created the user
let email = user_mailer::welcome(config, &user)?;
mailers::deliver_later(&txn, email).await?;

// time-limited, no secret inside (an event reminder, a limited offer)
mailers::deliver_later_until(&txn, email, starts_at).await?;

// anything carrying a token: enqueue ids only; the job mints the token and sends
jobs::enqueue(&txn, SendPasswordResetJob { user_id: user.id }).await?;
```

`src/mailers/mod.rs` (generated by `new`):

```rust
pub async fn deliver_later(db: &impl ConnectionTrait, email: Email) -> Result<Inserted, AppError> {
    jobs::enqueue(db, DeliverEmailJob { email, expires_at: None }).await
}

/// Time-limited mail with no secret: at most 5 attempts (~6 minutes of retries), discarded
/// rather than delivered after `expires_at`. Never for tokens (see below).
pub async fn deliver_later_until(db: &impl ConnectionTrait, email: Email, expires_at: DateTime<Utc>) -> Result<Inserted, AppError> {
    let job = DeliverEmailJob { email, expires_at: Some(expires_at) };
    Ok(job.into_job().max_attempts(5).insert(db).await?)
}
```

`src/jobs/deliver_email.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliverEmailJob {
    #[serde(flatten)] pub email: Email,                     // flatten: rows that are a bare Email still decode
    #[serde(default, skip_serializing_if = "Option::is_none")] pub expires_at: Option<DateTime<Utc>>,
}

impl Worker for DeliverEmailJob {
    const NAME: &'static str = "deliver_email";
    const QUEUE: &'static str = "mailers";
    const MAX_ATTEMPTS: i32 = 10;                           // ~4 hours of retries, not the 3-week default
    async fn perform(self, _job: &JobInfo, state: &AppState) -> anyhow::Result<Outcome> {
        if let Some(at) = self.expires_at && at <= Utc::now() {
            return Ok(Outcome::Discard(format!("expired at {at} before it could be delivered")));
        }
        state.mailer.deliver(self.email).await?;
        Ok(Outcome::Done)
    }
}
```

- **Render at enqueue time.** The job carries the finished `Email`, so the worker never needs template context. This is the one job allowed to carry content instead of ids.
- Never call `deliver` from a request handler. SMTP latency and failures belong in the job's retry loop.
- **Secrets never go in job args.** `DeliverEmailJob` carries the rendered email, and job args are readable by anyone with `/admin/jobs` (and `app jobs show`). So an email containing a token (password reset, magic link, email confirmation, invitation) is sent by a **dedicated job** whose args hold only ids (`SendPasswordResetJob { user_id }`). Its worker creates the token, stores only its digest, renders the email and calls `state.mailer.deliver(email)` directly (allowed inside jobs). Give it `const MAX_ATTEMPTS: i32 = 5`, and have it `Discard` when the request is stale (e.g. `inserted_at` older than the token lifetime), so a reset link never arrives hours late. The full recipe is in [auth](auth.md).

## The dev mailbox (`kernel/mailbox.rs`), Swoosh's local mailbox

In development every email lands in a real inbox you click through, with no extra service. Because it's a **table** (not process memory), mail sent by the inline worker, a separate `app worker` or a task all shows up in the same place. **`generators/new.md` builds all of it in every app, from day one. It is never a stub and never "later".**

It is a self-contained kernel UI, like `Plug.Swoosh.MailboxPreview`: its own Askama templates (`templates/kernel/mailbox/`), the kernel stylesheet (`static/kernel/admin.css`, shared with `/admin/jobs` and `/dev`) and vendored htmx. It never uses the app layout, Tailwind or React, so it works identically in every `FRONTEND_MODE`. (It can't use inline `<style>`: the app's CSP is `style-src 'self'`.)

### Table (migration `create_mailbox_messages`, generated by `new`)

```
mailbox_messages:
  id           big_pk_auto
  to_addr      string
  from_addr    string
  cc           json         default []     // ["a@x.com"]
  bcc          json         default []     // stored separately: Bcc never appears in the sent headers
  reply_to     string_null
  subject      string
  html         text
  text         text
  headers      json                        // [[name, value], …] parsed from the formatted message
  eml          text                        // the full RFC 822 message from lettre's `formatted()`
  attachments  json         default []     // [{filename, content_type, data (base64)}]
  read_at      timestamp_with_time_zone_null
  created_at   timestamp_with_time_zone
```

`Mailer::Local::deliver` builds the same `lettre::Message` the SMTP path would (so formatting bugs show up locally), stores it with `kernel::mailbox::store(db, from, &email, &message) -> id`, and logs `mail delivered to /dev/mailbox/{id}`.

### The UI

Two panes. Left: **Inbox | Previews** tabs. Right: the selected message.

- **Inbox list**, newest first: recipient, subject (bold + accent dot when unread), relative time, 📎 when it has attachments. The Inbox tab shows the unread count. "Clear all" at the top.
- **Live:** the list polls every 2s (`hx-trigger="every 2s"`). The page remembers the newest id it loaded (`since`), and new mail appears with a "N new messages · show newest" banner and a brief highlight. The header's Live/Paused toggle turns polling off.
- **Message pane:** subject, then From, To, Cc, Bcc, Reply-To (only the ones set), Date; "All headers (n)" expands every raw header. Tabs **HTML** (sandboxed iframe) · **Text** · **Raw** (the `.eml`). Width toggle **Desktop / Mobile** (375px in a phone frame). **Download .eml** and **Delete**. **Attachments** (name, type, size, Download). **Links** found in the text part, clickable, so reset and magic links are one click away.
- **Empty state** explains how mail gets there (`Mailer::Local`, any process, appears within a second), points to Previews, and has a **Send a test email** button (also next to "Clear all").
- Opening a message marks it read. Deleting or clearing flashes "Message deleted." / "Cleared N messages."
- **Back/Forward never resurrects deleted mail:** every mailbox response is `Cache-Control: no-store` (one `kernel::ui::no_store()` layer on the router), the kernel layout's htmx config has `historyCacheSize: 0`, and `static/kernel/admin.js` reloads a page restored from the back/forward cache (`pageshow` with `event.persisted`). A deleted message's URL redirects to the inbox with "That message was deleted." instead of a bare 404.

### Routes (development only)

| Route | Does |
|-------|------|
| `GET /dev/mailbox` | The inbox, newest message selected. `?part=list&since=<id>` (with `HX-Request`) returns the list fragment |
| `GET /dev/mailbox/{id}` | The inbox with message `id` selected. `?tab=html\|text\|raw`, `?w=desktop\|mobile`. Marks it read. Unknown id → 303 to the inbox with a flash |
| `GET /dev/mailbox/{id}/message.eml` | Download (`message/rfc822`, `attachment`). Not `/{id}.eml`: axum 0.8 panics at startup on a parameter that shares a segment with a suffix ("Only one parameter is allowed per path segment"), even when it's the only such route. A test pins this |
| `GET /dev/mailbox/{id}/html` | The HTML body alone, for the iframe, with `<base target="_blank">` injected and its **own** headers: `Content-Security-Policy: default-src 'none'; img-src * data:; style-src 'unsafe-inline' *; font-src *; frame-ancestors 'self'`, `X-Frame-Options: SAMEORIGIN` (set on the response, so the global `if_not_present` headers don't apply) |
| `GET /dev/mailbox/{id}/attachments/{n}` | Download attachment `n` with its content type, always `Content-Disposition: attachment` (never rendered on our origin) |
| `GET /dev/mailbox/previews`, `/dev/mailbox/previews/{*name}` | The Previews tab (below). `…/{name}/html` is the preview's iframe body |
| `POST /dev/mailbox/{id}/delete`, `POST /dev/mailbox/clear` | Housekeeping (303 back to the inbox, with a flash) |
| `POST /dev/mailbox/test` | **Send a test email**: a sample message stored straight into the mailbox (built with `Mailer::local`, so never sent over SMTP whatever `MAILER` says). Also how the browser smoke seeds mail |
| `GET /dev/mailbox.json` | **For agents:** the newest 50 as `[{id, to, cc, bcc, subject, text, links, attachments: [filename], created_at}]` |
| `GET /dev/mailbox/latest.json?to=` | The newest message (optionally for one recipient); `404 {"error": "no messages"}` when none, so `curl -f` fails |
| `GET /dev/mailers` | 301 to `/dev/mailbox/previews` (older apps linked here) |

- The iframe is `<iframe sandbox="allow-popups allow-popups-to-escape-sandbox">`: no scripts, no same-origin access, links open in a new tab.
- `routes::paths::dev_mailbox()` returns `/dev/mailbox`. Auth screens use it for the development-only "Check your mailbox" hint, so the route is stable.
- `CleanupJob` keeps the newest 500 messages in development (`mailbox::trim`).

### Previews

`src/mailers/previews.rs` (generated by `new`, initially empty):

```rust
pub use crate::kernel::mailbox::Preview;   // { name: &'static str, build: for<'a> fn(&'a AppState) -> PreviewFuture<'a> }

pub const PREVIEWS: &[Preview] = &[
    Preview { name: "user_mailer/welcome", build: |s| Box::pin(async { user_mailer::welcome(&s.config, &fake_user()) }) },
];
```

`routes.rs` mounts `kernel::mailbox::router(crate::mailers::previews::PREVIEWS)`. The Previews tab lists them and renders the selected one in the **same viewer** as real mail (HTML/Text/Raw, Desktop/Mobile, attachments, links), with a "Preview · rendered now, not sent" note. Previews are rendered, never sent or stored. Build them from fake data; don't touch the database.

### For agents

Exercising a flow that sends mail (sign-up, password reset, invitation) is part of "ran the app and exercised it": trigger it with curl or a browser, then `curl -s localhost:3000/dev/mailbox/latest.json?to=ada@example.com` and follow `links[0]`.

## Local development

- Default: the `Local` mailer. Open http://localhost:3000/dev/mailbox (`bin/dev` prints the link).
- To try real SMTP delivery: `MAILER=smtp SMTP_URL=smtp://localhost:1025` with `docker compose up -d mailpit` (http://localhost:8025), or a real provider. Most apps never need this.

## Gotchas agents get wrong

- Sending relative URLs (`/posts/1`) in emails. They must be absolute.
- Only sending HTML. Always send the text part too.
- Rendering a token into an email and enqueueing it (`deliver_later`/`deliver_later_until`): the token is then readable in `/admin/jobs`. Mint tokens inside a dedicated job instead (ids-only args, 5 attempts, discard when stale).
- Writing a "log to files" mailer for development, or using `SMTP_URL` presence to pick the dev mailer. Development is always `Local` unless `MAILER` says otherwise.
- **Grepping links out of raw `.eml`.** lettre encodes bodies as quoted-printable, wrapped at 76 columns with `=` soft line breaks, so a token in a long URL gets cut. Read links from `/dev/mailbox/latest.json` (`links`, built from the decoded text part) or `app.sent_emails()` in tests, never from the raw message.
- Building `lettre` messages with unvalidated addresses. `addr.parse::<Mailbox>()` can fail, so validate at the model layer (`AppError::Invalid`) instead of failing in the job.
- Using `SmtpTransport` (blocking) instead of `AsyncSmtpTransport<Tokio1Executor>`.
- `cargo add lettre` with default features (native-tls). See above.
- Adding a field to `Email` or `DeliverEmailJob` without `#[serde(default)]`. Rows already in the queue must still decode.

## Testing

```rust
user::sign_up(&app.db, params).await?;
app.drain_jobs().await;
let mails = app.sent_emails();
assert_eq!(mails.len(), 1);
assert_eq!(mails[0].to, "ada@example.com");
assert!(mails[0].text.contains("http://localhost:3000/session/new"));
```

Kernel tests (generated by `new`, `tests/app/kernel_web_test.rs`) cover the mailbox in development config: mail sent through `deliver_later` → job → `Local` appears at `/dev/mailbox`; tabs, widths, the iframe body and its CSP; `/message.eml` download; a deleted message redirects with a flash; `no-store` on every response (including redirects and JSON); the test-email button; Cc/Bcc/Reply-To and attachments (listed, downloadable, and present in the raw multipart message); the "new mail" banner; JSON endpoints; delete/clear; previews rendered through `kernel::mailbox::router(TEST_PREVIEWS)`; mailer selection (development stays `Local` with `SMTP_URL` set; `MAILER=smtp` needs `SMTP_URL`; production falls back to `Log`); `deliver_later_until` caps attempts and discards late mail; and a bare-`Email` row still decodes as `DeliverEmailJob`. `security_test.rs` proves every `/dev/*` route is 404 in production.
