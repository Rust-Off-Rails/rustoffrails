//! `app task auth_demo`: development only. Creates (or refreshes) a demo account with obviously
//! fake credentials and prints them, so you can sign in right away. `tools/demo.sh` runs it.
// APP: the demo address. Keep it on example.com (a reserved domain: mail to it goes nowhere).

use crate::models::{auth_event, session, user};
use crate::state::AppState;

pub const DEMO_EMAIL: &str = "demo@example.com";
/// A second demo account; the two-factor sample turns two-factor on for it.
pub const DEMO_2FA_EMAIL: &str = "demo-2fa@example.com";

pub async fn run(state: &AppState, _args: Vec<String>) -> anyhow::Result<()> {
    // SECURITY: never creates a known-credential account outside development
    // (test: the_demo_task_refuses_outside_development)
    anyhow::ensure!(
        state.config.is_development(),
        "auth_demo only runs with APP_ENV=development"
    );
    let now = chrono::Utc::now();
    let user = user::find_or_create(&state.db, DEMO_EMAIL, now).await?;
    user::mark_verified(&state.db, user.id, now).await?;
    let two_factor = user::find_or_create(&state.db, DEMO_2FA_EMAIL, now).await?;
    user::mark_verified(&state.db, two_factor.id, now).await?;
    // Two other "devices" (documentation IP ranges, never real), so the sessions list has rows.
    if session::all_for(&state.db, &user).await?.is_empty() {
        let devices = [
            (
                "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1",
                "198.51.100.23",
                "magic_link",
            ),
            (
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36",
                "203.0.113.54",
                "password",
            ),
        ];
        for (ua, ip, method) in devices {
            let meta = auth_event::Meta {
                ip: Some(ip.to_owned()),
                user_agent: Some(ua.to_owned()),
                device_digest: None,
            };
            let at = now - chrono::Duration::hours(26);
            session::start(
                &state.db,
                user.id,
                method,
                meta.ip.clone(),
                meta.user_agent.clone(),
                at,
            )
            .await?;
            auth_event::record(
                &state.db,
                Some(user.id),
                auth_event::Kind::SignedIn,
                &meta,
                at,
            )
            .await?;
        }
    }
    println!("Demo accounts (development only, fake credentials):");
    println!("  email:     {DEMO_EMAIL} (and {DEMO_2FA_EMAIL})");
    // samples: auth demo
    Ok(())
}
