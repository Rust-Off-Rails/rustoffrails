//! `app task send_test_email <address>`: delivers `TestMailer::test_email` now, through the mailer
//! this environment selects. In production that's the way to prove `SMTP_URL` works.

use crate::mailers::test_mailer;
use crate::state::AppState;

pub async fn run(state: &AppState, args: Vec<String>) -> anyhow::Result<()> {
    let [to] = args.as_slice() else {
        anyhow::bail!("usage: app task send_test_email <address>");
    };
    let email = test_mailer::test_email(&state.config, to)?;
    state.mailer.deliver(email).await?;
    println!("sent a test email to {to} via {}", state.mailer.kind());
    Ok(())
}
