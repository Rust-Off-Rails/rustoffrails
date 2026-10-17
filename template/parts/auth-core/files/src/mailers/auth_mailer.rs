//! Every auth email: sign-in links and codes, verification, resets, email changes and security
//! notices. One HTML + text template pair (`templates/mailers/auth_mailer/message.*`) renders
//! them all from an `AuthMail`, so they look alike and each sample adds mail without templates.
//!
//! GUIDE: .offrails/guides/mailers.md#writing-a-mailer
//!
//! Every URL is absolute and built from `config.app_url`, which in development follows the port
//! actually served, so a link never points at a stale port.

use askama::Template;

use crate::config::Config;
use crate::error::AppError;
use crate::kernel::mailer::Email;
use crate::routes::paths;

/// The content of one auth email. Fields left empty are simply not rendered.
#[derive(Default)]
pub struct AuthMail {
    pub to: String,
    pub subject: String,
    pub heading: String,
    /// Paragraphs before the button.
    pub intro: Vec<String>,
    /// The main button: (label, absolute URL).
    pub action: Option<(String, String)>,
    /// A one-time code shown large (sign-in codes).
    pub code: Option<String>,
    /// "Device: Firefox on macOS" rows.
    pub details: Vec<(String, String)>,
    /// Paragraphs after the button.
    pub outro: Vec<String>,
    /// "This wasn't me": signs the account out everywhere.
    pub not_me_url: Option<String>,
}

#[derive(Template)]
#[template(path = "mailers/auth_mailer/message.html")]
struct MessageHtml<'a> {
    mail: &'a AuthMail,
}

#[derive(Template)]
#[template(path = "mailers/auth_mailer/message.txt")]
struct MessageText<'a> {
    mail: &'a AuthMail,
}

impl AuthMail {
    pub fn render(self) -> Result<Email, AppError> {
        let html = MessageHtml { mail: &self }.render()?;
        let text = MessageText { mail: &self }.render()?;
        Ok(Email::new(self.to, self.subject, html, text))
    }
}

/// `{APP_URL}{path}`.
pub fn url(config: &Config, path: &str) -> String {
    format!("{}{path}", config.app_url.trim_end_matches('/'))
}

/// A security event worth an email to the account owner.
#[derive(Clone, Debug)]
pub enum Notice {
    NewSignIn {
        device: String,
        ip: Option<String>,
    },
    PasswordChanged,
    /// Sent to the **old** address.
    EmailChanged {
        old_email: String,
        new_email: String,
    },
    TwoFactorEnabled,
    TwoFactorDisabled,
    RecoveryCodesRegenerated,
    RecoveryCodeUsed {
        remaining: u64,
    },
    /// Sign-in was paused after repeated failures.
    Throttled,
}

impl Notice {
    /// An explicit recipient, when not the account's current address.
    pub fn recipient(&self) -> Option<&str> {
        match self {
            Notice::EmailChanged { old_email, .. } => Some(old_email),
            _ => None,
        }
    }
}

/// The notice email, with its "this wasn't me" link.
// APP: wording of each notice.
pub fn notice(
    config: &Config,
    to: &str,
    notice: &Notice,
    not_me_token: &str,
) -> Result<Email, AppError> {
    let app = crate::config::APP_NAME;
    let at = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();
    let (subject, heading, intro, mut details) = match notice {
        Notice::NewSignIn { device, ip } => (
            format!("New sign-in to {app}"),
            "New sign-in to your account".to_owned(),
            vec![format!("Your {app} account was just signed in to from a device we haven't seen before.")],
            vec![
                ("Device".to_owned(), device.clone()),
                ("IP address".to_owned(), ip.clone().unwrap_or_else(|| "unknown".to_owned())),
            ],
        ),
        Notice::PasswordChanged => (
            format!("Your {app} password was changed"),
            "Your password was changed".to_owned(),
            vec!["The password for your account was just changed, and every other device was signed out.".to_owned()],
            vec![],
        ),
        Notice::EmailChanged { new_email, .. } => (
            format!("Your {app} email address was changed"),
            "Your email address was changed".to_owned(),
            vec![format!("Your account's email address was changed to {new_email}. Emails about the account now go there.")],
            vec![],
        ),
        Notice::TwoFactorEnabled => (
            format!("Two-factor authentication is on for {app}"),
            "Two-factor authentication is on".to_owned(),
            vec!["Signing in now also needs a code from your authenticator app. Keep your recovery codes somewhere safe.".to_owned()],
            vec![],
        ),
        Notice::TwoFactorDisabled => (
            format!("Two-factor authentication was turned off for {app}"),
            "Two-factor authentication was turned off".to_owned(),
            vec!["Signing in no longer needs a code from your authenticator app.".to_owned()],
            vec![],
        ),
        Notice::RecoveryCodesRegenerated => (
            format!("New {app} recovery codes"),
            "New recovery codes were generated".to_owned(),
            vec!["Your old recovery codes no longer work. Store the new ones somewhere safe.".to_owned()],
            vec![],
        ),
        Notice::RecoveryCodeUsed { remaining } => (
            format!("A {app} recovery code was used"),
            "A recovery code was used to sign in".to_owned(),
            vec![format!("You have {remaining} recovery codes left. Generate new ones from your account page when you're running low.")],
            vec![],
        ),
        Notice::Throttled => (
            format!("Sign-in to {app} paused"),
            "We paused sign-in to your account".to_owned(),
            vec!["There were several failed attempts to sign in to your account, so we paused sign-in for a few minutes. If it was you, wait a moment or reset your password. If it wasn't, your account is safe: nobody got in.".to_owned()],
            vec![],
        ),
    };
    details.push(("When".to_owned(), at));
    AuthMail {
        to: to.to_owned(),
        subject,
        heading,
        intro,
        details,
        outro: vec!["If this was you, there's nothing to do.".to_owned()],
        not_me_url: Some(url(config, &paths::not_me(not_me_token))),
        ..Default::default()
    }
    .render()
}

/// "Confirm your new email address", sent to the new address.
pub fn email_change(config: &Config, to: &str, token: &str) -> Result<Email, AppError> {
    let app = crate::config::APP_NAME;
    AuthMail {
        to: to.to_owned(),
        subject: format!("Confirm your new {app} email address"),
        heading: "Confirm your new email address".to_owned(),
        intro: vec![format!("Someone asked to use this address for a {app} account. If it was you, confirm it below. The link works once, for one hour.")],
        action: Some(("Confirm this address".to_owned(), url(config, &paths::email_change(token)))),
        outro: vec!["If you didn't ask for this, ignore this email; nothing changes.".to_owned()],
        ..Default::default()
    }
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_carry_an_absolute_not_me_link_and_no_markup_injection() {
        let config = Config::test();
        let email = notice(
            &config,
            "ada@example.com",
            &Notice::NewSignIn {
                device: "<script>x</script>".into(),
                ip: Some("192.0.2.1".into()),
            },
            "tok",
        )
        .unwrap();
        assert!(
            email
                .text
                .contains("http://localhost:3000/security/not-me/tok")
        );
        assert!(
            email
                .html
                .contains("http://localhost:3000/security/not-me/tok")
        );
        assert!(!email.html.contains("<script>x"));
    }
}
