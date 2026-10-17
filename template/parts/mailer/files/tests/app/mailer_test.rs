//! The mailer sample: selection per environment, delivery adapters, the message on the wire,
//! header injection, previews and the test email task. samples/mailer/CHECKS.md lists what each
//! test proves.

use migration::{Migrator, MigratorTrait};
use offrails_host::config::{AppEnv, Config, MailConfig, resolve_app_url};
use offrails_host::kernel::mailbox;
use offrails_host::kernel::mailer::{self, Attachment, Email, Mailer};
use offrails_host::mailers::{previews::PREVIEWS, test_mailer};
use offrails_host::state::select_mailer;

use crate::support::TestApp;

fn config(env: AppEnv, mailer: Option<&str>, smtp_url: Option<&str>) -> Config {
    Config {
        env,
        mail: MailConfig {
            mailer: mailer.map(str::to_owned),
            smtp_url: smtp_url.map(str::to_owned),
            ..MailConfig::test()
        },
        ..Config::test()
    }
}

async fn db() -> sea_orm::DatabaseConnection {
    offrails_host::kernel::db::connect("sqlite::memory:", 1)
        .await
        .unwrap()
}

#[tokio::test]
async fn development_always_uses_the_dev_mailbox_even_with_smtp_url_set() {
    let c = config(AppEnv::Development, None, Some("smtp://localhost:1025"));
    assert_eq!(
        select_mailer(&c, &db().await).unwrap().kind(),
        "local (dev mailbox)"
    );
}

#[tokio::test]
async fn selection_follows_the_environment_and_mailer() {
    let db = db().await;
    let kind = |c: Config| select_mailer(&c, &db).map(|m| m.kind());
    assert_eq!(
        kind(config(AppEnv::Test, Some("smtp"), None)).unwrap(),
        "memory (tests)"
    );
    assert_eq!(
        kind(config(AppEnv::Production, None, None)).unwrap(),
        "log (not sent)"
    );
    assert_eq!(
        kind(config(
            AppEnv::Production,
            None,
            Some("smtps://u:p@smtp.example.com:465")
        ))
        .unwrap(),
        "smtp"
    );
    assert_eq!(
        kind(config(AppEnv::Production, Some("log"), Some("smtp://x:25"))).unwrap(),
        "log (not sent)"
    );
    assert_eq!(
        kind(config(
            AppEnv::Development,
            Some("smtp"),
            Some("smtp://localhost:1025")
        ))
        .unwrap(),
        "smtp"
    );
    assert!(
        kind(config(AppEnv::Development, Some("smtp"), None)).is_err(),
        "MAILER=smtp needs SMTP_URL"
    );
    assert!(kind(config(AppEnv::Production, None, Some("http://not-smtp"))).is_err());
}

#[tokio::test]
async fn an_invalid_mail_from_fails_at_boot() {
    let mut c = config(AppEnv::Development, None, None);
    c.mail.from = "not an address".into();
    assert!(select_mailer(&c, &db().await).is_err());
}

#[test]
fn the_smtp_url_never_shows_in_config_debug() {
    let c = config(
        AppEnv::Production,
        None,
        Some("smtps://user:s3cr3t-pw@smtp.example.com:465"),
    );
    let shown = format!("{c:?}");
    assert!(
        !shown.contains("s3cr3t-pw") && shown.contains("[redacted]"),
        "{shown}"
    );
}

#[tokio::test]
async fn the_test_environment_captures_mail_in_memory() {
    let app = TestApp::new().await;
    app.state
        .mailer
        .deliver(Email::new("ada@example.com", "Hi", "<p>Hi</p>", "Hi"))
        .await
        .unwrap();
    let sent = app.sent_emails();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, "ada@example.com");
    assert_eq!(app.last_email_to("ADA@example.com").unwrap().subject, "Hi");
    assert!(app.last_email_to("grace@example.com").is_none());
}

#[tokio::test]
async fn the_memory_and_log_mailers_refuse_what_smtp_would_refuse() {
    for m in [Mailer::memory(), Mailer::Log] {
        let bad = Email::new("ada@example.com", "Hi\r\nBcc: eve@example.com", "", "");
        assert!(m.deliver(bad).await.is_err(), "{}", m.kind());
    }
}

#[tokio::test]
async fn local_delivery_stores_the_message_that_smtp_would_send() {
    let app = TestApp::new().await;
    let from: lettre::message::Mailbox = "App <no-reply@example.test>".parse().unwrap();
    let local = Mailer::local(app.db().clone(), from);
    let email = Email::new(
        "Ada@Example.com",
        "Your receipt",
        "<p>Thanks, Ada</p>",
        "Thanks, Ada",
    )
    .cc("grace@example.com")
    .bcc("audit@example.com")
    .reply_to("support@example.com")
    .attach(Attachment::new(
        "receipt.pdf",
        "application/pdf",
        b"%PDF-1.4 fake",
    ));
    local.deliver(email).await.unwrap();

    let m = mailbox::latest(app.db(), Some("ada@example.com"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(m.to_addr, "ada@example.com");
    assert_eq!(m.subject, "Your receipt");
    assert_eq!(m.html, "<p>Thanks, Ada</p>");
    assert_eq!(m.cc, serde_json::json!(["grace@example.com"]));
    assert_eq!(m.bcc, serde_json::json!(["audit@example.com"]));
    assert_eq!(m.reply_to.as_deref(), Some("support@example.com"));
    assert!(m.eml.contains("multipart/mixed") && m.eml.contains("receipt.pdf"));
    assert!(m.eml.contains("Cc: grace@example.com"));
    assert!(
        !m.eml.contains("audit@example.com"),
        "Bcc never appears in the sent headers"
    );
    let headers = mailbox::parse_headers(&m.eml);
    assert!(
        headers
            .iter()
            .any(|(k, v)| k == "Reply-To" && v.contains("support@example.com"))
    );
    assert!(headers.iter().any(|(k, _)| k == "Message-ID"));
    assert!(headers.iter().any(|(k, _)| k == "Date"));
    assert_eq!(mailbox::counts(app.db()).await.unwrap(), (1, 1));
}

/// CRLF in a header value would let input add headers (Bcc: …) or a body. The message is never
/// built, so nothing is sent or stored.
#[tokio::test]
async fn header_injection_is_refused_before_anything_is_sent() {
    let app = TestApp::new().await;
    let from: lettre::message::Mailbox = "App <no-reply@example.test>".parse().unwrap();
    let local = Mailer::local(app.db().clone(), from);
    let attempts = [
        Email::new("ada@example.com", "Hi\r\nBcc: eve@example.com", "", ""),
        Email::new("ada@example.com\nBcc: eve@example.com", "Hi", "", ""),
        Email::new("ada@example.com", "Hi", "", "").cc("x@example.com\r\nX-Evil: 1"),
        Email::new("ada@example.com", "Hi", "", "").reply_to("x@example.com\rX-Evil: 1"),
        Email::new("ada@example.com", "Hi", "", "").attach(Attachment::new(
            "a.txt\"\r\nX-Evil: 1",
            "text/plain",
            b"x",
        )),
        Email::new("ada@example.com", "Hi", "", "").attach(Attachment::new(
            "a.txt",
            "text/plain\r\nX-Evil: 1",
            b"x",
        )),
        Email::new("ada@example.com, eve@example.com", "Hi", "", ""),
    ];
    for email in attempts {
        assert!(local.deliver(email.clone()).await.is_err(), "{email:?}");
        assert!(mailer::build(&"a@example.com".parse().unwrap(), &email).is_err());
    }
    assert_eq!(mailbox::counts(app.db()).await.unwrap(), (0, 0));
}

#[tokio::test]
async fn a_unicode_subject_is_encoded_not_raw() {
    let email = Email::new("ada@example.com", "Olá — café ☕", "<p>x</p>", "x");
    let raw = String::from_utf8(
        mailer::build(&"a@example.com".parse().unwrap(), &email)
            .unwrap()
            .formatted(),
    )
    .unwrap();
    let subject = mailbox::parse_headers(&raw)
        .into_iter()
        .find(|(k, _)| k == "Subject")
        .unwrap()
        .1;
    assert!(subject.starts_with("=?utf-8?"), "{subject}");
}

#[tokio::test]
async fn the_test_email_has_both_parts_and_absolute_links() {
    let app = TestApp::new().await;
    let email = test_mailer::test_email(&app.state.config, "ada@example.com").unwrap();
    assert_eq!(
        email.subject,
        format!("Test email from {}", offrails_host::config::APP_NAME)
    );
    assert!(email.html.contains("href=\"http://localhost:3000/\""));
    assert!(email.text.contains("http://localhost:3000/"));
    assert!(
        !email.html.contains("href=\"/"),
        "relative links break in email clients"
    );
}

#[tokio::test]
async fn every_preview_renders_and_is_never_sent_or_stored() {
    let app = TestApp::new().await;
    assert!(!PREVIEWS.is_empty());
    for p in PREVIEWS {
        let email = (p.build)(&app.state).await.unwrap();
        assert!(!email.subject.is_empty(), "{}", p.name);
        assert!(
            !email.html.is_empty() && !email.text.is_empty(),
            "{}",
            p.name
        );
        assert!(p.name.contains('/'), "{} is <mailer>/<action>", p.name);
        mailer::validate(&email).unwrap();
    }
    assert!(app.sent_emails().is_empty());
    assert_eq!(mailbox::counts(app.db()).await.unwrap(), (0, 0));
}

#[tokio::test]
async fn the_send_test_email_task_delivers_through_the_selected_mailer() {
    let app = TestApp::new().await;
    offrails_host::tasks::run(
        &app.state,
        "send_test_email",
        vec!["ada@example.com".into()],
    )
    .await
    .unwrap();
    assert_eq!(app.sent_emails()[0].to, "ada@example.com");
    assert!(
        offrails_host::tasks::run(&app.state, "send_test_email", vec![])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn mailbox_search_trim_and_latest() {
    let app = TestApp::new().await;
    let from: lettre::message::Mailbox = "App <no-reply@example.test>".parse().unwrap();
    let local = Mailer::local(app.db().clone(), from);
    for (to, subject) in [
        ("ada@example.com", "Welcome"),
        ("grace@example.com", "50% off_today"),
        ("ada@example.com", "Your code"),
    ] {
        local
            .deliver(Email::new(to, subject, "<p>x</p>", "body text"))
            .await
            .unwrap();
    }
    let hits = mailbox::search(app.db(), "WELCOME", 50).await.unwrap();
    assert_eq!(hits.len(), 1, "case-insensitive");
    assert_eq!(
        mailbox::search(app.db(), "%", 50).await.unwrap().len(),
        1,
        "% is literal"
    );
    assert_eq!(
        mailbox::search(app.db(), "_", 50).await.unwrap().len(),
        1,
        "_ is literal"
    );
    assert_eq!(
        mailbox::search(app.db(), "grace@", 50).await.unwrap().len(),
        1
    );
    assert_eq!(
        mailbox::search(app.db(), "", 2).await.unwrap()[0].subject,
        "Your code"
    );
    assert_eq!(
        mailbox::latest(app.db(), Some(" ADA@example.com "))
            .await
            .unwrap()
            .unwrap()
            .subject,
        "Your code"
    );
    assert_eq!(mailbox::trim(app.db(), 1).await.unwrap(), 2);
    assert_eq!(mailbox::counts(app.db()).await.unwrap().0, 1);
}

#[tokio::test]
async fn the_mailbox_migration_rolls_back_and_forward() {
    let app = TestApp::new().await;
    // Roll back to just before this migration, however many were added after it.
    let names: Vec<String> = Migrator::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    let at = names
        .iter()
        .position(|n| n.ends_with("create_mailbox_messages"))
        .unwrap();
    Migrator::down(app.db(), Some((names.len() - at) as u32))
        .await
        .unwrap();
    assert!(
        mailbox::counts(app.db()).await.is_err(),
        "the table is gone"
    );
    Migrator::up(app.db(), None).await.unwrap();
    assert_eq!(mailbox::counts(app.db()).await.unwrap(), (0, 0));
}

/// In development, links follow the port actually served: a stale `APP_URL` in `.env` (another
/// port) must not send the sign-in link to another server.
#[tokio::test]
async fn development_links_follow_the_port_being_served() {
    let c = Config {
        env: AppEnv::Development,
        port: 3960,
        app_url: resolve_app_url(
            AppEnv::Development,
            Some("http://localhost:3000"),
            3960,
            false,
        ),
        ..Config::test()
    };
    let email = test_mailer::test_email(&c, "ada@example.com").unwrap();
    assert!(
        email.text.contains("http://localhost:3960/"),
        "{}",
        email.text
    );
    assert!(email.html.contains("href=\"http://localhost:3960/\""));
    assert!(!email.text.contains(":3000"));
}

/// Production links come from `APP_URL`, never from the port.
#[tokio::test]
async fn production_links_come_from_app_url() {
    let c = Config {
        env: AppEnv::Production,
        port: 8080,
        app_url: resolve_app_url(
            AppEnv::Production,
            Some("https://app.example.com/"),
            8080,
            false,
        ),
        ..Config::test()
    };
    let email = test_mailer::test_email(&c, "ada@example.com").unwrap();
    assert!(
        email.text.contains("https://app.example.com/"),
        "{}",
        email.text
    );
}
