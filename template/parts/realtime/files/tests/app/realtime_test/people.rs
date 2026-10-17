//! Who the realtime suite signs in, and how it ends their sessions: the test side of
//! `channels::identity`. Development guests by default; with auth-core installed, the optional
//! `realtime people` wire re-exports `people_accounts` above the glob, and the whole suite runs
//! against accounts.

// The source of people. An explicit `pub use` added above this glob (the auth-core wire) wins over
// it for every name it lists.
// samples: realtime people
#[allow(unused_imports)]
pub use guests::*;

/// Development guests (`models::guest`, `models::guest_session`).
#[allow(dead_code)]
pub mod guests {
    use chrono::{Duration, Utc};
    use offrails_host::models::{guest, guest_session};
    use offrails_host::state::AppState;
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, DatabaseConnection, EntityTrait};

    /// A new person named `name` with a session: `(id, display name, session id, raw token)`.
    pub async fn create(state: &AppState, name: &str) -> (i64, String, i64, String) {
        let now = Utc::now();
        let new = guest::GuestParams { name: name.into() }
            .validate()
            .expect("a valid name");
        let g = guest::create(&state.db, new, now).await.expect("guest");
        let (session, token) = guest_session::start(&state.db, g.id, now)
            .await
            .expect("session");
        (g.id, g.name, session.id, token)
    }

    /// Another session (another browser) of person `id`: `(session id, raw token)`.
    pub async fn start_session(state: &AppState, id: i64) -> (i64, String) {
        let (session, token) = guest_session::start(&state.db, id, Utc::now())
            .await
            .expect("session");
        (session.id, token)
    }

    /// Ends one session, as signing out does (without the wake-up: tests call `session_ended`).
    pub async fn end_session(db: &DatabaseConnection, session_id: i64) {
        guest_session::destroy(db, session_id)
            .await
            .expect("destroy session");
    }

    /// Ends every session of person `id`.
    pub async fn end_all_sessions(db: &DatabaseConnection, id: i64) {
        guest_session::destroy_all_for(db, id)
            .await
            .expect("destroy sessions");
    }

    /// Makes a session look idle for `days` (past its idle expiry when long enough).
    pub async fn idle_session(db: &DatabaseConnection, session_id: i64, days: i64) {
        let row = guest_session::Entity::find_by_id(session_id)
            .one(db)
            .await
            .expect("query")
            .expect("session");
        let mut active: guest_session::session::ActiveModel = row.into();
        active.last_seen_at = Set(Utc::now() - Duration::days(days));
        active.update(db).await.expect("update");
    }
}
