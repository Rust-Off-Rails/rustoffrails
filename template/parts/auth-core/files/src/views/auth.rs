//! Page structs for sign-in, the account security page, re-authentication and the
//! "this wasn't me" page. The sign-in method samples add their own fields to these structs at the
//! `samples:` anchors, so a page shows whichever methods are installed.

use askama::Template;
use chrono::{DateTime, Utc};

use super::Ctx;
use crate::models::{Errors, auth_event, session, user};
use crate::routes::paths;

/// Canonical and `og:url` use this path instead of the request's, so a token in the URL is
/// never echoed into the page.
// SECURITY: keeps emailed tokens out of `<link rel="canonical">` and social-card metadata
// (test: token_pages_are_not_cached_and_send_no_referrer)
pub trait CtxExt {
    fn at(self, path: &str) -> Self;
}

impl CtxExt for Ctx {
    fn at(mut self, path: &str) -> Self {
        self.path = path.to_owned();
        self
    }
}

/// `GET /session/new`: one form per installed method (anchor in the template).
#[derive(Template)]
#[template(path = "sessions/new.html")]
pub struct SessionsNew {
    pub ctx: Ctx,
    /// Kept across a failed attempt (never the password).
    pub email: String,
    /// A message for the whole form ("Try again in 30 seconds").
    pub alert: Option<String>,
    pub errors: Errors,
    /// Which method's form produced `errors`/`alert`: "password", "magic_link".
    pub form: &'static str,
}

impl SessionsNew {
    pub fn blank(ctx: Ctx) -> Self {
        Self {
            ctx,
            email: String::new(),
            alert: None,
            errors: Errors::default(),
            form: "",
        }
    }

    /// Errors and alert only for the form that was submitted.
    pub fn alert_for(&self, form: &str) -> Option<&str> {
        (self.form == form)
            .then_some(self.alert.as_deref())
            .flatten()
    }

    pub fn errors_on(&self, form: &str, field: &str) -> &[String] {
        if self.form == form {
            self.errors.on(field)
        } else {
            &[]
        }
    }
}

/// One row of "Where you're signed in".
pub struct SessionRow {
    pub id: i64,
    pub device: String,
    pub ip: String,
    pub method: String,
    pub last_active: String,
    pub signed_in: String,
    pub current: bool,
}

impl SessionRow {
    pub fn new(s: &session::Model, current_id: i64, now: DateTime<Utc>) -> Self {
        Self {
            id: s.id,
            device: crate::kernel::user_agent::describe(s.user_agent.as_deref()),
            ip: s.ip.clone().unwrap_or_else(|| "unknown IP".to_owned()),
            method: method_label(&s.sign_in_method).to_owned(),
            last_active: if s.id == current_id {
                "Active now".to_owned()
            } else {
                format!("Active {}", time_ago(s.last_seen_at, now))
            },
            signed_in: time_ago(s.created_at, now),
            current: s.id == current_id,
        }
    }
}

/// One row of "Recent security activity".
pub struct EventRow {
    pub label: &'static str,
    pub warning: bool,
    pub device: String,
    pub ip: String,
    pub when: String,
}

impl EventRow {
    pub fn new(e: &auth_event::Model, now: DateTime<Utc>) -> Self {
        Self {
            label: e.kind.label(),
            warning: e.kind.is_warning(),
            device: crate::kernel::user_agent::describe(e.user_agent.as_deref()),
            ip: e.ip.clone().unwrap_or_else(|| "unknown IP".to_owned()),
            when: time_ago(e.created_at, now),
        }
    }
}

/// `GET /account`: the account security page.
#[derive(Template)]
#[template(path = "account/show.html")]
pub struct AccountShow {
    pub ctx: Ctx,
    pub user: user::Model,
    pub sessions: Vec<SessionRow>,
    pub events: Vec<EventRow>,
    /// Re-authenticated recently: sensitive buttons work without another prompt.
    pub recently_confirmed: bool,
    // samples: account fields
}

impl AccountShow {
    pub fn other_sessions(&self) -> usize {
        self.sessions.iter().filter(|s| !s.current).count()
    }
}

/// `GET /account/confirm`: "Confirm it's you" with every installed method.
#[derive(Template)]
#[template(path = "account/confirm.html")]
pub struct AccountConfirm {
    pub ctx: Ctx,
    pub user: user::Model,
    pub alert: Option<String>,
    pub errors: Errors,
    /// Which method's form produced `errors`/`alert`.
    pub form: &'static str,
    /// Two-factor is on: only a second factor may confirm.
    pub second_factor: bool,
    // samples: confirm fields
}

impl AccountConfirm {
    pub fn alert_for(&self, form: &str) -> Option<&str> {
        (self.form == form)
            .then_some(self.alert.as_deref())
            .flatten()
    }

    pub fn errors_on(&self, form: &str, field: &str) -> &[String] {
        if self.form == form {
            self.errors.on(field)
        } else {
            &[]
        }
    }
}

/// `GET /account/email/edit`
#[derive(Template)]
#[template(path = "account/email_edit.html")]
pub struct EmailEdit {
    pub ctx: Ctx,
    pub user: user::Model,
    pub email: String,
    pub errors: Errors,
}

/// `GET /account/email/confirm/{token}`
#[derive(Template)]
#[template(path = "account/email_confirm.html")]
pub struct EmailConfirm {
    pub ctx: Ctx,
    pub new_email: String,
    pub action: String,
}

/// `GET /security/not-me/{token}`
#[derive(Template)]
#[template(path = "security/not_me.html")]
pub struct NotMeShow {
    pub ctx: Ctx,
    pub email: String,
    pub action: String,
}

/// "Check your email" after asking for a link (sign-up, reset, magic link, verification).
#[derive(Template)]
#[template(path = "auth/check_email.html")]
pub struct CheckEmail {
    pub ctx: Ctx,
    pub heading: String,
    pub message: String,
    /// An extra form (the magic-link code entry) rendered under the message.
    pub code_form: Option<CodeForm>,
}

/// A "type the code from the email" form.
pub struct CodeForm {
    pub action: String,
    pub error: Option<String>,
}

/// An invalid, expired or used link.
#[derive(Template)]
#[template(path = "auth/invalid_link.html")]
pub struct InvalidLink {
    pub ctx: Ctx,
    pub heading: String,
    pub message: String,
    pub link_label: String,
    pub link_href: String,
}

/// "5 minutes ago", "yesterday", "3 days ago". Approximate on purpose: exact timestamps of
/// other devices aren't needed to recognize them.
pub fn time_ago(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - then).num_seconds().max(0);
    match secs {
        0..=59 => "just now".to_owned(),
        60..=3599 => plural(secs / 60, "minute"),
        3600..=86_399 => plural(secs / 3600, "hour"),
        86_400..=172_799 => "yesterday".to_owned(),
        _ => plural(secs / 86_400, "day"),
    }
}

fn plural(n: i64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{n} {unit}s ago")
    }
}

/// What the sessions list says about how a session began.
pub fn method_label(method: &str) -> &'static str {
    match method {
        "password" => "Password",
        "magic_link" => "Email link",
        "magic_link_code" => "Email code",
        "password+totp" | "magic_link+totp" | "magic_link_code+totp" | "reset+totp" => {
            "With two-factor"
        }
        "recovery_code" => "Recovery code",
        "trusted_device" => "Remembered device",
        "reset" => "Password reset",
        "verification" => "Email verification",
        _ => "Signed in",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn time_ago_is_approximate() {
        let now = Utc::now();
        assert_eq!(time_ago(now, now), "just now");
        assert_eq!(time_ago(now - Duration::minutes(5), now), "5 minutes ago");
        assert_eq!(time_ago(now - Duration::hours(1), now), "1 hour ago");
        assert_eq!(time_ago(now - Duration::hours(30), now), "yesterday");
        assert_eq!(time_ago(now - Duration::days(9), now), "9 days ago");
    }
}
