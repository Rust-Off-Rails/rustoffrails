//! `app task auth_invite <email>`: creates an account for `email` and emails the invitation.
//! The way people get in when `config.auth.signup` is `Signup::Invite` (it works under `Open`
//! too). The account stays unverified until the invitation is used: with auth-password the email
//! carries a link to choose a password (which proves the address), with auth-magic-link the
//! person signs in by link or code from the sign-in page.
// APP: rename the task for your domain (`create_author`, `invite_member`) in src/tasks/mod.rs, or
// call `invite` from an admin screen (inside `state.background.spawn`, so the request never
// holds the token).

use chrono::Utc;

use crate::{
    error::AppError,
    mailers::auth_mailer::{AuthMail, url},
    models::{
        Errors,
        auth_event::{self, Kind, Meta},
        user,
    },
    routes::paths,
    state::AppState,
};

pub async fn run(state: &AppState, args: Vec<String>) -> anyhow::Result<()> {
    let [email] = args.as_slice() else {
        anyhow::bail!("usage: app task auth_invite <email>");
    };
    let invited = invite(state, email).await.map_err(|e| match e {
        AppError::Invalid(errors) => {
            anyhow::anyhow!("{email}: {}", errors.full_messages().join(", "))
        }
        other => other.into(),
    })?;
    println!(
        "Invited {} (account {}): the invitation is on its way.",
        invited.email, invited.id
    );
    Ok(())
}

/// Creates the account (unverified) unless it exists, records the invitation and emails it.
/// Inviting an unverified account again sends a fresh invitation (older links die); a verified
/// account is refused, so an invitation never reaches someone who already has access.
// SECURITY: the invitation token (auth-password) is minted here, inside the send, and exists only
// in the email and as a digest; an invitation never touches a verified account
// (test: the_invite_task_creates_an_unverified_account_and_emails_it, inviting_a_verified_account_changes_nothing)
pub async fn invite(state: &AppState, email: &str) -> Result<user::Model, AppError> {
    let email = user::validate_email(email).map_err(AppError::Invalid)?;
    let now = Utc::now();
    if user::find_by_email(&state.db, &email)
        .await?
        .is_some_and(|u| u.is_verified())
    {
        return Err(AppError::Invalid(Errors::single(
            "email",
            "already has an account",
        )));
    }
    let user = user::find_or_create(&state.db, &email, now).await?;
    auth_event::record(
        &state.db,
        Some(user.id),
        Kind::Invited,
        &Meta::default(),
        now,
    )
    .await?;
    let app = crate::config::APP_NAME;
    // APP: the invitation's wording.
    #[allow(unused_mut)] // sign-in samples change the button below
    let mut mail = AuthMail {
        to: user.email.clone(),
        subject: format!("You're invited to {app}"),
        heading: format!("You're invited to {app}"),
        intro: vec![format!(
            "An account on {app} is waiting for {}. Sign in with this address to start.",
            user.email
        )],
        action: Some((
            "Sign in".to_owned(),
            url(&state.config, &paths::new_session()),
        )),
        outro: vec![
            "If you weren't expecting this, ignore this email: nothing happens until you use it."
                .to_owned(),
        ],
        ..Default::default()
    };
    // samples: auth invite
    state.mailer.deliver(mail.render()?).await?;
    Ok(user)
}
