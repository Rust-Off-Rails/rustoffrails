//! Action Mailer delivery: the `Email` a mailer renders, and the `Mailer` that delivers it. SMTP in
//! production (`Log` when none is configured), the dev mailbox table in development (`Local`, read
//! at `/dev/mailbox` with the dev-mailbox sample), memory in tests. Spec: .offrails/guides/mailers.md
//!
//! The kernel only offers constructors; which one an environment uses is `state::select_mailer`.
//! GUIDE: .offrails/guides/mailers.md#kernel-spec-srckernelmailerrs

use std::sync::{Arc, Mutex};

use anyhow::{Context, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use lettre::message::{
    Attachment as MailAttachment, Mailbox, MultiPart, SinglePart, header::ContentType,
};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::kernel::mailbox;
use crate::state::AppState;

/// One rendered email: HTML and text, absolute URLs only. Plain JSON, so a job can carry it.
/// Every field added later gets `#[serde(default)]`: rows already in a queue must still decode.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Email {
    pub to: String,
    pub subject: String,
    pub html: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cc: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bcc: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

impl Email {
    /// Everything else via `..Default::default()` or the builder methods below.
    pub fn new(
        to: impl Into<String>,
        subject: impl Into<String>,
        html: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            to: to.into(),
            subject: subject.into(),
            html: html.into(),
            text: text.into(),
            ..Default::default()
        }
    }

    pub fn cc(mut self, address: impl Into<String>) -> Self {
        self.cc.push(address.into());
        self
    }

    pub fn bcc(mut self, address: impl Into<String>) -> Self {
        self.bcc.push(address.into());
        self
    }

    pub fn reply_to(mut self, address: impl Into<String>) -> Self {
        self.reply_to = Some(address.into());
        self
    }

    pub fn attach(mut self, attachment: Attachment) -> Self {
        self.attachments.push(attachment);
        self
    }
}

/// `data` is base64, so the email stays plain JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attachment {
    pub filename: String,
    pub content_type: String,
    pub data: String,
}

impl Attachment {
    pub fn new(filename: &str, content_type: &str, bytes: &[u8]) -> Self {
        Self {
            filename: filename.to_owned(),
            content_type: content_type.to_owned(),
            data: STANDARD.encode(bytes),
        }
    }

    pub fn bytes(&self) -> Vec<u8> {
        STANDARD.decode(&self.data).unwrap_or_default()
    }
}

#[derive(Clone)]
pub enum Mailer {
    Smtp {
        transport: AsyncSmtpTransport<Tokio1Executor>,
        from: Mailbox,
    },
    /// Development: rows in `mailbox_messages` (the dev-mailbox sample shows them at `/dev/mailbox`).
    Local {
        db: DatabaseConnection,
        from: Mailbox,
    },
    /// Production fallback when no SMTP is configured: a log line with recipient and subject only.
    // WHY: never the body: it may carry a link that signs someone in, and logs are widely readable.
    Log,
    /// Tests: `app.sent_emails()`.
    Memory(Arc<Mutex<Vec<Email>>>),
}

impl Mailer {
    /// `SMTP_URL`: `smtps://user:pass@smtp.example.com:465`, `smtp://localhost:1025` (no TLS, a
    /// local catcher), `smtp://user:pass@host:587?tls=required` (STARTTLS). Any provider over SMTP.
    pub fn smtp(url: &str, from: Mailbox) -> anyhow::Result<Self> {
        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
            .context("SMTP_URL is not a valid smtp:// or smtps:// URL")?
            .build();
        Ok(Self::Smtp { transport, from })
    }

    pub fn local(db: DatabaseConnection, from: Mailbox) -> Self {
        Self::Local { db, from }
    }

    pub fn memory() -> Self {
        Self::Memory(Arc::new(Mutex::new(Vec::new())))
    }

    /// Delivers now. Request handlers send through a job instead (the jobs sample's
    /// `mailers::deliver_later`), so SMTP latency and failures land in its retry loop.
    pub async fn deliver(&self, email: Email) -> anyhow::Result<()> {
        match self {
            Self::Smtp { transport, from } => {
                transport.send(build(from, &email)?).await?;
                Ok(())
            }
            Self::Local { db, from } => {
                let message = build(from, &email)?;
                let id = mailbox::store(db, from, &email, &message).await?;
                tracing::info!(message_id = id, "mail delivered to /dev/mailbox/{id}");
                Ok(())
            }
            Self::Log => {
                validate(&email)?;
                tracing::info!(to = %email.to, subject = %email.subject, "email not sent (no SMTP_URL configured)");
                Ok(())
            }
            Self::Memory(outbox) => {
                validate(&email)?;
                outbox
                    .lock()
                    .map_err(|_| anyhow::anyhow!("mailer outbox lock poisoned"))?
                    .push(email);
                Ok(())
            }
        }
    }

    /// Emails captured by a `Memory` mailer (empty for the others).
    pub fn sent(&self) -> Vec<Email> {
        match self {
            Self::Memory(outbox) => outbox.lock().map(|o| o.clone()).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// Whether mail lands in the dev mailbox (drives "check your mailbox" hints).
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local { .. })
    }

    /// For `/dev/system`: which adapter, never its credentials.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Smtp { .. } => "smtp",
            Self::Local { .. } => "local (dev mailbox)",
            Self::Log => "log (not sent)",
            Self::Memory(_) => "memory (tests)",
        }
    }
}

/// Refuses what would let input rewrite the message's headers: a line break (or any other
/// control character) in the subject, a filename or a content type, and recipients that aren't
/// one valid address each. Addresses are parsed by lettre; this covers the free-text headers.
// SECURITY: header injection. A CR/LF in a header value would let input add headers (Bcc: …) or
// a body; every adapter calls this before anything is sent or stored (test: header_injection_is_refused_before_anything_is_sent).
pub fn validate(email: &Email) -> anyhow::Result<()> {
    let unsafe_text = |s: &str| s.chars().any(char::is_control);
    if unsafe_text(&email.subject) {
        bail!("the subject contains a line break or control character");
    }
    for address in std::iter::once(&email.to)
        .chain(&email.cc)
        .chain(&email.bcc)
        .chain(&email.reply_to)
    {
        if unsafe_text(address) {
            bail!("an address contains a line break or control character");
        }
        address
            .parse::<Mailbox>()
            .with_context(|| format!("{address:?} is not a valid email address"))?;
    }
    for a in &email.attachments {
        if unsafe_text(&a.filename) || unsafe_text(&a.content_type) {
            bail!("an attachment's name or type contains a line break or control character");
        }
    }
    Ok(())
}

/// The exact message SMTP sends: From/To/Cc/Bcc/Reply-To, multipart/alternative (text + HTML),
/// wrapped in multipart/mixed with one part per attachment.
pub fn build(from: &Mailbox, email: &Email) -> anyhow::Result<Message> {
    validate(email)?;
    let alternative = MultiPart::alternative()
        .singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_PLAIN)
                .body(email.text.clone()),
        )
        .singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_HTML)
                .body(email.html.clone()),
        );
    let body = if email.attachments.is_empty() {
        alternative
    } else {
        let mut mixed = MultiPart::mixed().multipart(alternative);
        for a in &email.attachments {
            let content_type = ContentType::parse(&a.content_type)
                .or_else(|_| ContentType::parse("application/octet-stream"))?;
            mixed = mixed
                .singlepart(MailAttachment::new(a.filename.clone()).body(a.bytes(), content_type));
        }
        mixed
    };

    // WHY: Message-ID from the sender's domain: lettre's default uses this machine's hostname.
    let message_id = format!(
        "<{}@{}>",
        uuid::Uuid::new_v4().simple(),
        from.email.domain()
    );
    let mut builder = Message::builder()
        .message_id(Some(message_id))
        .from(from.clone())
        .to(email.to.parse()?)
        .subject(email.subject.clone());
    for cc in &email.cc {
        builder = builder.cc(cc.parse()?);
    }
    for bcc in &email.bcc {
        builder = builder.bcc(bcc.parse()?);
    }
    if let Some(reply_to) = &email.reply_to {
        builder = builder.reply_to(reply_to.parse()?);
    }
    Ok(builder.multipart(body)?)
}

/// What a preview builds: an email, rendered from fake data. Never sent or stored.
pub type PreviewFuture<'a> =
    std::pin::Pin<Box<dyn Future<Output = Result<Email, AppError>> + Send + 'a>>;

/// A mailer preview (`/rails/mailers`): `src/mailers/previews.rs` lists them, and the dev
/// mailbox renders them in the same viewer as real mail.
pub struct Preview {
    /// `<mailer>/<action>`, e.g. `test_mailer/test_email` (two segments: the dev mailbox routes
    /// them as `/dev/mailbox/previews/{mailer}/{action}`).
    pub name: &'static str,
    pub build: for<'a> fn(&'a AppState) -> PreviewFuture<'a>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from() -> Mailbox {
        "App <no-reply@example.test>".parse().unwrap()
    }

    #[test]
    fn attachment_round_trips_through_base64() {
        let a = Attachment::new("x.txt", "text/plain", b"hello");
        assert_eq!(a.bytes(), b"hello".to_vec());
    }

    #[test]
    fn a_plain_email_is_multipart_alternative() {
        let email = Email::new("ada@example.com", "Hi", "<p>Hi</p>", "Hi");
        let raw = String::from_utf8(build(&from(), &email).unwrap().formatted()).unwrap();
        assert!(raw.contains("multipart/alternative"));
        assert!(!raw.contains("multipart/mixed"));
    }

    #[test]
    fn line_breaks_in_free_text_headers_are_refused() {
        let email = Email::new("ada@example.com", "Hi\r\nBcc: eve@example.com", "", "");
        assert!(build(&from(), &email).is_err());
        let email = Email::new("ada@example.com\r\nBcc: eve@example.com", "Hi", "", "");
        assert!(build(&from(), &email).is_err());
        let email = Email::new("ada@example.com", "Hi", "", "").attach(Attachment::new(
            "a.txt\r\nX-Evil: 1",
            "text/plain",
            b"x",
        ));
        assert!(build(&from(), &email).is_err());
    }

    #[test]
    fn several_addresses_in_one_field_are_refused() {
        let email = Email::new("ada@example.com, eve@example.com", "Hi", "", "");
        assert!(validate(&email).is_err());
    }
}
