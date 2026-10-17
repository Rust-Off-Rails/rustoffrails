//! The realtime suite's people as auth-core accounts: compiled only when auth-core is installed
//! (the optional `realtime test mods` and `realtime people` wires), so every realtime test then
//! runs with accounts instead of development guests.

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{Duration, Utc};
use offrails_host::channels::accounts;
use offrails_host::models::{session, user};
use offrails_host::state::AppState;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, sea_query::Expr};

/// Keeps addresses unique when a test names two people alike.
static NEXT: AtomicU64 = AtomicU64::new(1);

/// A new account for `name` ("Ann Example" is ann.example+N@example.com), signed in:
/// `(id, display name, session id, raw token)`. The display name is the account's ("Member N"),
/// never `name`: assert against what this returns.
pub async fn create(state: &AppState, name: &str) -> (i64, String, i64, String) {
    let local: String = name
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(".")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.')
        .collect();
    let email = format!(
        "{local}+{}@example.com",
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let u = user::find_or_create(&state.db, &email, Utc::now())
        .await
        .expect("user");
    let (session_id, token) = start_session(state, u.id).await;
    (u.id, accounts::display_name(&u), session_id, token)
}

/// Another session (another browser) of account `id`: `(session id, raw token)`.
pub async fn start_session(state: &AppState, id: i64) -> (i64, String) {
    let (s, token) = session::start(&state.db, id, "test", None, None, Utc::now())
        .await
        .expect("session");
    (s.id, token)
}

/// Ends one session, as signing out does (without the wake-up: tests call `session_ended`).
pub async fn end_session(db: &DatabaseConnection, session_id: i64) {
    session::Entity::delete_by_id(session_id)
        .exec(db)
        .await
        .expect("delete session");
}

/// Ends every session of account `id`.
pub async fn end_all_sessions(db: &DatabaseConnection, id: i64) {
    session::revoke_all(db, id).await.expect("revoke sessions");
}

/// Makes a session look idle for `days` (past auth-core's idle expiry when long enough).
pub async fn idle_session(db: &DatabaseConnection, session_id: i64, days: i64) {
    session::Entity::update_many()
        .col_expr(
            session::Column::LastSeenAt,
            Expr::value(Utc::now() - Duration::days(days)),
        )
        .filter(session::Column::Id.eq(session_id))
        .exec(db)
        .await
        .expect("update session");
}
