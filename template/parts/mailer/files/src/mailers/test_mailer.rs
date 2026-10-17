//! `TestMailer`: one harmless email that proves delivery works end to end. Sent by
//! `app task send_test_email <address>` (try a production SMTP_URL) and previewed in the dev
//! mailbox. It is also the pattern every other mailer follows.

use askama::Template;

use crate::config::Config;
use crate::error::AppError;
use crate::kernel::mailer::Email;
use crate::routes::paths;

#[derive(Template)]
#[template(path = "mailers/test_mailer/test_email.html")]
struct TestEmailHtml<'a> {
    app_name: &'a str,
    app_url: &'a str,
    sent_at: String,
}

#[derive(Template)]
#[template(path = "mailers/test_mailer/test_email.txt")]
struct TestEmailText<'a> {
    app_name: &'a str,
    app_url: &'a str,
    sent_at: String,
}

/// "Test email from <app>": who it's from, when it was rendered, and a link back to the app.
/// APP: the pattern for every mailer: two templates (HTML + text) sharing field names, absolute
/// URLs from `config.app_url` + a `paths::` helper, one preview in `previews.rs`.
pub fn test_email(config: &Config, to: &str) -> Result<Email, AppError> {
    // INVARIANT: links are absolute and follow the address being served in development (test: development_links_follow_the_port_being_served).
    let app_url = format!("{}{}", config.app_url, paths::root());
    let sent_at = chrono::Utc::now()
        .format("%Y-%m-%d %H:%M:%S UTC")
        .to_string();
    let app_name = crate::config::APP_NAME;
    Ok(Email::new(
        to,
        format!("Test email from {app_name}"),
        TestEmailHtml {
            app_name,
            app_url: &app_url,
            sent_at: sent_at.clone(),
        }
        .render()?,
        TestEmailText {
            app_name,
            app_url: &app_url,
            sent_at,
        }
        .render()?,
    ))
}
