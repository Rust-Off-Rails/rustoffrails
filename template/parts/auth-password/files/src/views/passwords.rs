//! Page structs for sign-up, email verification, password reset and password change.

use askama::Template;

use super::Ctx;
use crate::models::{Errors, password_policy::Strength, user};
use crate::routes::paths;

/// `GET /registration/new`
#[derive(Template)]
#[template(path = "registrations/new.html")]
pub struct RegistrationNew {
    pub ctx: Ctx,
    pub email: String,
    pub errors: Errors,
    pub alert: Option<String>,
    /// Shown after a failed submit (the server-side meter; htmx updates it while typing).
    pub strength: Option<Strength>,
}

/// `GET /email-verification/{token}`
#[derive(Template)]
#[template(path = "registrations/verify.html")]
pub struct VerificationShow {
    pub ctx: Ctx,
    pub email: String,
    pub action: String,
    /// Opened in another browser than the one that signed up: ask for the password.
    pub needs_password: bool,
    pub errors: Errors,
    pub alert: Option<String>,
}

/// `GET /passwords/new`: "Forgot your password?"
#[derive(Template)]
#[template(path = "passwords/new.html")]
pub struct PasswordsNew {
    pub ctx: Ctx,
    pub email: String,
    pub errors: Errors,
    pub alert: Option<String>,
}

/// `GET /passwords/{token}/edit`: choose a new password.
#[derive(Template)]
#[template(path = "passwords/edit.html")]
pub struct PasswordsEdit {
    pub ctx: Ctx,
    pub email: String,
    /// An invitation link (the first password), not a reset.
    pub invitation: bool,
    pub action: String,
    pub errors: Errors,
    pub strength: Option<Strength>,
}

/// `GET /account/password/edit`
#[derive(Template)]
#[template(path = "account_passwords/edit.html")]
pub struct AccountPasswordEdit {
    pub ctx: Ctx,
    pub user: user::Model,
    /// A magic-link account adding its first password: no "current password" field.
    pub has_password: bool,
    pub errors: Errors,
    pub strength: Option<Strength>,
}

/// `POST /password-strength`: the meter alone (htmx swaps it in while typing).
#[derive(Template)]
#[template(path = "passwords/_strength.html")]
pub struct StrengthMeter {
    pub strength: Option<Strength>,
}
